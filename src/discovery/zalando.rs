use anyhow::Result;
use async_trait::async_trait;
use kube::api::{Api, ListParams};
use kube::core::DynamicObject;
use kube::discovery::ApiResource;
use std::sync::Arc;

use super::k8s_client::K8sHandle;
use super::{make_alias, now, DatabaseDiscoverer};
use crate::registry::{DatabaseEntry, Source};

pub struct ZalandoDiscoverer {
    pub k8s: Arc<K8sHandle>,
}

#[async_trait]
impl DatabaseDiscoverer for ZalandoDiscoverer {
    fn name(&self) -> &'static str {
        "k8s-zalando"
    }

    async fn discover(&self) -> Result<Vec<DatabaseEntry>> {
        let client = self.k8s.client().await?;

        let ar = ApiResource {
            group: "acid.zalan.do".to_string(),
            version: "v1".to_string(),
            api_version: "acid.zalan.do/v1".to_string(),
            kind: "postgresql".to_string(),
            plural: "postgresqls".to_string(),
        };

        let api: Api<DynamicObject> = Api::all_with(client, &ar);
        let lp = ListParams::default().limit(500);
        let list = match api.list(&lp).await {
            Ok(l) => l,
            Err(e) => {
                anyhow::bail!("CRD postgresqls.acid.zalan.do indisponivel: {}", e);
            }
        };

        let mut out = Vec::new();
        for cluster in list.items {
            let name = cluster.metadata.name.clone().unwrap_or_default();
            let ns = cluster.metadata.namespace.clone().unwrap_or_default();
            if name.is_empty() || ns.is_empty() {
                continue;
            }

            let host = format!("{}.{}.svc.cluster.local", name, ns);
            let secret_name = format!("postgres.{}.credentials.postgresql.acid.zalan.do", name);

            out.push(DatabaseEntry {
                alias: make_alias("zalando", &format!("{}-{}", ns, name)),
                host,
                port: 5432,
                user: "postgres".to_string(),
                database: "postgres".to_string(),
                password_ref: format!("k8s-secret://{}/{}/password", ns, secret_name),
                source: Source::K8sZalando,
                description: Some(format!("Zalando postgres-operator cluster {}/{}", ns, name)),
                cluster_ref: Some(format!("{}/{}", ns, name)),
                container_id: None,
                discovered_at: Some(now()),
            });
        }

        Ok(out)
    }
}
