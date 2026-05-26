use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct Credentials {
    pub password: String,
    pub source: CredSource,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("password", &"***redacted***")
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum CredSource {
    Env(String),
    K8sSecret { ns: String, name: String, key: String },
    Vault { path: String, key: String },
    Manual,
}

pub struct CredentialStore {
    inner: RwLock<HashMap<String, Credentials>>,
    k8s: Arc<crate::discovery::k8s_client::K8sHandle>,
}

impl CredentialStore {
    pub fn new(k8s: Arc<crate::discovery::k8s_client::K8sHandle>) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            k8s,
        }
    }

    pub async fn has(&self, alias: &str) -> bool {
        self.inner.read().await.contains_key(alias)
    }

    pub async fn set_manual(&self, alias: &str, password: String) {
        self.inner.write().await.insert(
            alias.to_string(),
            Credentials {
                password,
                source: CredSource::Manual,
            },
        );
    }

    pub async fn resolve(&self, alias: &str, password_ref: &str) -> Result<String> {
        if let Some(c) = self.inner.read().await.get(alias) {
            return Ok(c.password.clone());
        }

        let creds = resolve_ref(password_ref, &self.k8s).await.with_context(|| {
            format!(
                "falha resolvendo credencial do alias '{}' (ref='{}')",
                alias, password_ref
            )
        })?;

        let password = creds.password.clone();
        self.inner.write().await.insert(alias.to_string(), creds);
        Ok(password)
    }
}

async fn resolve_ref(
    password_ref: &str,
    k8s: &Arc<crate::discovery::k8s_client::K8sHandle>,
) -> Result<Credentials> {
    if let Some(rest) = password_ref.strip_prefix("env://") {
        let var = rest.trim();
        if var.is_empty() {
            bail!("env:// sem nome de variavel");
        }
        let password = std::env::var(var)
            .with_context(|| format!("variavel de ambiente '{}' nao definida", var))?;
        return Ok(Credentials {
            password,
            source: CredSource::Env(var.to_string()),
        });
    }

    if let Some(rest) = password_ref.strip_prefix("k8s-secret://") {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() != 3 {
            bail!("k8s-secret:// formato esperado: k8s-secret://<namespace>/<name>/<key>");
        }
        let (ns, name, key) = (parts[0], parts[1], parts[2]);
        let raw = k8s.read_secret(ns, name, key).await?;
        return Ok(Credentials {
            password: raw,
            source: CredSource::K8sSecret {
                ns: ns.to_string(),
                name: name.to_string(),
                key: key.to_string(),
            },
        });
    }

    if let Some(rest) = password_ref.strip_prefix("vault://") {
        let mut split = rest.splitn(2, '#');
        let path = split.next().unwrap_or("").to_string();
        let key = split
            .next()
            .ok_or_else(|| anyhow!("vault:// requer fragmento '#<key>'"))?
            .to_string();
        
        let password = resolve_vault(k8s, &path, &key).await?;
        return Ok(Credentials {
            password,
            source: CredSource::Vault { path, key },
        });
    }

    bail!(
        "password_ref desconhecida '{}' (esperado env://, k8s-secret:// ou vault://)",
        password_ref
    )
}

async fn resolve_vault(
    k8s: &Arc<crate::discovery::k8s_client::K8sHandle>,
    path: &str,
    key: &str,
) -> Result<String> {
    // 1. Get the Vault Token
    let mut token = std::env::var("VAULT_TOKEN").unwrap_or_default();
    if token.is_empty() {
        if let Ok(t) = tokio::fs::read_to_string("/userprofile/.vault-token").await {
            token = t.trim().to_string();
        }
    }
    if token.is_empty() {
        if let Ok(t) = tokio::fs::read_to_string("/root/.vault-token").await {
            token = t.trim().to_string();
        }
    }
    if token.is_empty() {
        bail!("VAULT_TOKEN nao encontrado no ambiente nem em ~/.vault-token");
    }

    // 2. Determine Vault Address
    let vault_addr = std::env::var("VAULT_ADDR").unwrap_or_else(|_| "http://vault.vault.svc.cluster.local:8200".to_string());
    
    eprintln!("Resolvendo segredo do Vault via {}...", vault_addr);

    // Establish connection to Vault (direct or port-forward)
    let host = vault_addr
        .strip_prefix("http://").unwrap_or(&vault_addr)
        .strip_prefix("https://").unwrap_or(&vault_addr)
        .split(':')
        .next()
        .unwrap_or("vault.vault.svc.cluster.local");

    let mut resolved_value = None;

    // Try KV v2 path mapping (inserting /data/ after the mount point/first segment)
    let kv2_path = if let Some(slash_idx) = path.find('/') {
        let (mount, rest) = path.split_at(slash_idx);
        format!("{}/data{}", mount, rest)
    } else {
        path.to_string()
    };

    // Try KV v2 first
    match connect_vault_stream(k8s, &vault_addr, host).await {
        Ok(mut stream) => {
            match query_vault_http(&mut stream, &kv2_path, key, &token).await {
                Ok(val) => {
                    resolved_value = Some(val);
                }
                Err(e) => {
                    eprintln!("Vault KV v2 read failed for path '{}': {:#}", kv2_path, e);
                }
            }
        }
        Err(e) => {
            eprintln!("Vault connection failed for KV v2 (path='{}'): {:#}", kv2_path, e);
        }
    }

    // Fallback to KV v1 if KV v2 failed
    if resolved_value.is_none() {
        match connect_vault_stream(k8s, &vault_addr, host).await {
            Ok(mut stream) => {
                match query_vault_http(&mut stream, path, key, &token).await {
                    Ok(val) => {
                        resolved_value = Some(val);
                    }
                    Err(e) => {
                        eprintln!("Vault KV v1 read failed for path '{}': {:#}", path, e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Vault connection failed for KV v1 (path='{}'): {:#}", path, e);
            }
        }
    }

    resolved_value.ok_or_else(|| anyhow!("Falha ao resolver chave '{}' no caminho '{}' do Vault (Token e Conexao validados, verifique as mensagens acima)", key, path))
}

pub trait VaultStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static> VaultStream for T {}

async fn connect_k8s_vault_portforward(
    k8s: &Arc<crate::discovery::k8s_client::K8sHandle>,
    ns: &str,
    svc_name: &str,
) -> Result<std::pin::Pin<Box<dyn VaultStream>>> {
    eprintln!("Estabelecendo túnel de port-forward para o Vault {}/{} na porta 8200...", ns, svc_name);
    
    let client = k8s.client().await?;
    use k8s_openapi::api::core::v1::Endpoints;
    use kube::Api;
    let ep_api: Api<Endpoints> = Api::namespaced(client.clone(), ns);
    let ep = ep_api.get(svc_name).await
        .with_context(|| format!("Falha ao ler Endpoints para o Vault {}/{}", ns, svc_name))?;
    
    let mut pod_name = None;
    if let Some(subsets) = ep.subsets {
        for subset in subsets {
            if let Some(addresses) = subset.addresses {
                for addr in addresses {
                    if let Some(target_ref) = addr.target_ref {
                        if target_ref.kind.as_deref() == Some("Pod") {
                            if let Some(name) = target_ref.name {
                                pod_name = Some(name);
                                break;
                            }
                        }
                    }
                }
            }
            if pod_name.is_some() { break; }
        }
    }
    
    let pod = pod_name.ok_or_else(|| anyhow!("Nenhum pod ativo encontrado nos Endpoints para o Vault {}/{}", ns, svc_name))?;
    
    use k8s_openapi::api::core::v1::Pod;
    let pod_api: Api<Pod> = Api::namespaced(client, ns);
    let mut pf = pod_api.portforward(&pod, &[8200]).await
        .context("Falha ao abrir portforward para o pod do Vault")?;
    
    let pf_stream = pf.take_stream(8200)
        .ok_or_else(|| anyhow!("Falha ao extrair stream do Vault portforward"))?;
    
    tokio::spawn(async move {
        let _keep_alive = pf;
        tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
    });

    Ok(Box::pin(pf_stream))
}

async fn connect_vault_stream(
    k8s: &Arc<crate::discovery::k8s_client::K8sHandle>,
    vault_addr: &str,
    host: &str,
) -> Result<std::pin::Pin<Box<dyn VaultStream>>> {
    // If the address explicitly mentions in-cluster domains or the special vault.vox domain
    if vault_addr.contains(".svc.cluster.local") || host.contains("vault.vox") {
        let parts: Vec<&str> = host.split('.').collect();
        let (svc_name, ns) = if parts.len() >= 3 && parts[2] == "svc" {
            (parts[0], parts[1])
        } else {
            ("vault", "vault")
        };
        
        match connect_k8s_vault_portforward(k8s, ns, svc_name).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                eprintln!("Falha no port-forward do Vault para {}/{}: {:#}", ns, svc_name, e);
                // Fall back to direct TCP
            }
        }
    }

    // Direct TCP connection
    let host_cleaned = if host.contains("localhost") {
        host.replace("localhost", "host.docker.internal")
    } else if host.contains("127.0.0.1") {
        host.replace("127.0.0.1", "host.docker.internal")
    } else {
        host.to_string()
    };

    // Determine the port
    let port = if let Some(idx) = vault_addr.rfind(':') {
        if idx > 5 {
            vault_addr[idx + 1..].parse::<u16>().unwrap_or(8200)
        } else {
            if vault_addr.starts_with("https") { 443 } else { 8200 }
        }
    } else {
        if vault_addr.starts_with("https") { 443 } else { 8200 }
    };

    let addr = format!("{}:{}", host_cleaned.split(':').next().unwrap_or(&host_cleaned), port);
    eprintln!("Tentando conexão direta TCP ao Vault em {}...", addr);
    
    match tokio::net::TcpStream::connect(&addr).await {
        Ok(tcp_stream) => Ok(Box::pin(tcp_stream)),
        Err(direct_err) => {
            eprintln!("Conexão direta ao Vault em {} falhou: {:#}. Tentando port-forward de fallback para vault.vault...", addr, direct_err);
            // Fallback to K8s port forward
            connect_k8s_vault_portforward(k8s, "vault", "vault").await
                .with_context(|| format!("Falha total ao conectar ao Vault (conexão direta para {} falhou e port-forward de fallback falhou)", addr))
        }
    }
}

async fn query_vault_http<S>(
    stream: &mut S,
    path: &str,
    key: &str,
    token: &str,
) -> Result<String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    
    let http_req = format!(
        "GET /v1/{} HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         X-Vault-Token: {}\r\n\
         Connection: close\r\n\r\n",
        path, token
    );
    
    stream.write_all(http_req.as_bytes()).await?;
    stream.flush().await?;
    
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    
    let resp_str = String::from_utf8_lossy(&response);
    
    let status_line = resp_str.lines().next().unwrap_or("");
    let body_start = resp_str.find("\r\n\r\n")
        .map(|idx| idx + 4)
        .unwrap_or(0);
    let body = &resp_str[body_start..];
    
    if !status_line.contains("200 OK") {
        bail!("Erro na resposta HTTP do Vault ({}). Resposta: {}", status_line, body);
    }
    
    let json: serde_json::Value = serde_json::from_str(body)
        .context("Falha ao analisar JSON do Vault")?;
    
    let val = json.get("data")
        .and_then(|d| {
            d.get("data")
                .and_then(|d2| d2.get(key))
                .or_else(|| d.get(key))
        })
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Chave '{}' nao encontrada no segredo do Vault", key))?;
        
    Ok(val.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::k8s_client::K8sHandle;

    #[tokio::test]
    async fn test_resolve_env() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        std::env::set_var("TEST_DB_PASSWORD", "supersecret");
        let pwd = store.resolve("test_alias", "env://TEST_DB_PASSWORD").await.unwrap();
        assert_eq!(pwd, "supersecret");
    }

    #[tokio::test]
    async fn test_resolve_env_missing() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        let res = store.resolve("test_alias", "env://NON_EXISTENT_VAR").await;
        assert!(res.is_err());
    }


    #[tokio::test]
    async fn test_set_manual() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        store.set_manual("test_alias", "manualsecret".to_string()).await;
        let pwd = store.resolve("test_alias", "env://ANY_REF").await.unwrap();
        assert_eq!(pwd, "manualsecret");
    }
}
