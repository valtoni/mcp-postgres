use anyhow::{anyhow, bail, Context, Result};
use k8s_openapi::api::core::v1::Secret;
use kube::{api::Api, Client};
use std::sync::Arc;
use tokio::sync::OnceCell;

pub struct K8sHandle {
    client: OnceCell<Result<Client, String>>,
}

impl K8sHandle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            client: OnceCell::new(),
        })
    }

    pub async fn client(&self) -> Result<Client> {
        let result = self
            .client
            .get_or_init(|| async {
                match Client::try_default().await {
                    Ok(c) => Ok(c),
                    Err(e) => Err(format!("kube::Client::try_default falhou: {}", e)),
                }
            })
            .await;

        match result {
            Ok(c) => Ok(c.clone()),
            Err(msg) => Err(anyhow!("Kubernetes indisponivel: {}", msg)),
        }
    }

    pub async fn read_secret(&self, ns: &str, name: &str, key: &str) -> Result<String> {
        let client = self.client().await?;
        let api: Api<Secret> = Api::namespaced(client, ns);
        let secret = api
            .get(name)
            .await
            .with_context(|| format!("falha lendo Secret {}/{}", ns, name))?;
        let data = secret
            .data
            .ok_or_else(|| anyhow!("Secret {}/{} sem campo 'data'", ns, name))?;
        let value = data
            .get(key)
            .ok_or_else(|| anyhow!("Secret {}/{} nao tem chave '{}'", ns, name, key))?;
        let raw = String::from_utf8(value.0.clone())
            .with_context(|| format!("Secret {}/{}/{} nao e UTF-8", ns, name, key))?;
        if raw.is_empty() {
            bail!("Secret {}/{}/{} esta vazio", ns, name, key);
        }
        Ok(raw)
    }
}
