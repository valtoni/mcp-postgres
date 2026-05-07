use anyhow::Result;
use async_trait::async_trait;
use kube::api::{Api, ListParams};
use kube::core::DynamicObject;
use kube::discovery::{ApiResource, Scope};
use serde::Deserialize;
use std::sync::Arc;

use super::k8s_client::K8sHandle;
use super::{make_alias, now, DatabaseDiscoverer};
use crate::registry::{DatabaseEntry, Source};

pub struct CnpgDiscoverer {
    pub k8s: Arc<K8sHandle>,
}

#[derive(Debug, Deserialize, Default)]
struct CnpgClusterStatus {
    #[serde(default)]
    write_service: Option<String>,
}

#[async_trait]
impl DatabaseDiscoverer for CnpgDiscoverer {
    fn name(&self) -> &'static str {
        "k8s-cnpg"
    }

    async fn discover(&self) -> Result<Vec<DatabaseEntry>> {
        let client = self.k8s.client().await?;

        let ar = ApiResource {
            group: "postgresql.cnpg.io".to_string(),
            version: "v1".to_string(),
            api_version: "postgresql.cnpg.io/v1".to_string(),
            kind: "Cluster".to_string(),
            plural: "clusters".to_string(),
        };
        let scope = Scope::Namespaced;

        let api: Api<DynamicObject> = Api::all_with(client, &ar);
        let lp = ListParams::default().limit(500);
        let list = match api.list(&lp).await {
            Ok(l) => l,
            Err(e) => {
                anyhow::bail!("CRD clusters.postgresql.cnpg.io indisponivel ({}): {}", scope_label(&scope), e);
            }
        };

        let mut out = Vec::new();
        for cluster in list.items {
            let name = cluster.metadata.name.clone().unwrap_or_default();
            let ns = cluster.metadata.namespace.clone().unwrap_or_default();
            if name.is_empty() || ns.is_empty() {
                continue;
            }

            let write_service: String = cluster
                .data
                .get("status")
                .and_then(|s| serde_json::from_value::<CnpgClusterStatus>(s.clone()).ok())
                .and_then(|s| s.write_service)
                .unwrap_or_else(|| format!("{}-rw", name));

            let host = format!("{}.{}.svc.cluster.local", write_service, ns);
            let secret_name = format!("{}-app", name);

            out.push(DatabaseEntry {
                alias: make_alias("cnpg", &format!("{}-{}", ns, name)),
                host,
                port: 5432,
                user: "app".to_string(),
                database: "app".to_string(),
                password_ref: format!("k8s-secret://{}/{}/password", ns, secret_name),
                source: Source::K8sCnpg,
                description: Some(format!("CloudNativePG cluster {}/{}", ns, name)),
                cluster_ref: Some(format!("{}/{}", ns, name)),
                container_id: None,
                discovered_at: Some(now()),
            });
        }

        Ok(out)
    }
}

fn scope_label(scope: &Scope) -> &'static str {
    match scope {
        Scope::Cluster => "cluster",
        Scope::Namespaced => "namespaced",
    }
}
