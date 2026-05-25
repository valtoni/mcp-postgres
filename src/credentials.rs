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
        
        let env_var_name = format!("VAULT_{}_{}", path.replace('/', "_").to_uppercase(), key.to_uppercase());
        if let Ok(password) = std::env::var(&env_var_name) {
            return Ok(Credentials {
                password,
                source: CredSource::Vault { path, key },
            });
        }
        
        bail!(
            "backend Vault nao implementado nesta versao real (para fins de teste/desenvolvimento, \
             defina a variavel de ambiente '{}'); caso contrario, use 'set_database_credentials' \
             para fornecer a senha em sessao (ref='{}')",
            env_var_name,
            password_ref
        );
    }

    bail!(
        "password_ref desconhecida '{}' (esperado env://, k8s-secret:// ou vault://)",
        password_ref
    )
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
        
        let res = store.resolve("my-db", "env://TEST_DB_PASSWORD").await;
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), "supersecret");

        // Verify it is cached in the store
        assert!(store.has("my-db").await);

        std::env::remove_var("TEST_DB_PASSWORD");
    }

    #[tokio::test]
    async fn test_resolve_env_missing() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        let res = store.resolve("my-db", "env://NON_EXISTENT_VAR_12345").await;
        assert!(res.is_err());
        let err_msg = format!("{:#}", res.unwrap_err());
        assert!(err_msg.contains("nao definida"));
    }

    #[tokio::test]
    async fn test_resolve_vault_mock() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        // Vault URI: vault://secret/data/postgres#password
        // Expected env var name: VAULT_SECRET_DATA_POSTGRES_PASSWORD
        std::env::set_var("VAULT_SECRET_DATA_POSTGRES_PASSWORD", "vault_secret_val");

        let res = store.resolve("my-vault-db", "vault://secret/data/postgres#password").await;
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), "vault_secret_val");

        std::env::remove_var("VAULT_SECRET_DATA_POSTGRES_PASSWORD");
    }

    #[tokio::test]
    async fn test_resolve_vault_unimplemented() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        let res = store.resolve("my-vault-db", "vault://secret/data/postgres-unimplemented#password").await;
        assert!(res.is_err());
        let err_msg = format!("{:#}", res.unwrap_err());
        assert!(err_msg.contains("backend Vault nao implementado"));
    }

    #[tokio::test]
    async fn test_resolve_k8s_unavailable() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        let res = store.resolve("my-k8s-db", "k8s-secret://my-ns/my-sec/pwd").await;
        assert!(res.is_err());
        let err_msg = format!("{:#}", res.unwrap_err());
        assert!(err_msg.contains("Kubernetes indisponivel"));
    }

    #[tokio::test]
    async fn test_set_manual() {
        let k8s = K8sHandle::new();
        let store = CredentialStore::new(k8s);

        store.set_manual("manual-db", "manual_pwd".to_string()).await;
        assert!(store.has("manual-db").await);

        let res = store.resolve("manual-db", "env://ANYTHING").await;
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), "manual_pwd");
    }
}


