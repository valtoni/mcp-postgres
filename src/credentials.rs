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
        let _ = (&path, &key);
        bail!(
            "backend Vault nao implementado nesta versao; use 'set_database_credentials' \
             para fornecer a senha em sessao (ref='{}')",
            password_ref
        );
    }

    bail!(
        "password_ref desconhecida '{}' (esperado env://, k8s-secret:// ou vault://)",
        password_ref
    )
}

