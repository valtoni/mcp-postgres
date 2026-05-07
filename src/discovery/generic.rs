use anyhow::Result;
use async_trait::async_trait;
use k8s_openapi::api::core::v1::{Secret, Service};
use kube::api::{Api, ListParams};
use std::sync::Arc;

use super::k8s_client::K8sHandle;
use super::{make_alias, now, DatabaseDiscoverer};
use crate::registry::{DatabaseEntry, Source};

pub struct GenericDiscoverer {
    pub k8s: Arc<K8sHandle>,
}

#[async_trait]
impl DatabaseDiscoverer for GenericDiscoverer {
    fn name(&self) -> &'static str {
        "k8s-generic"
    }

    async fn discover(&self) -> Result<Vec<DatabaseEntry>> {
        let client = self.k8s.client().await?;
        let svc_api: Api<Service> = Api::all(client.clone());
        let secret_api: Api<Secret> = Api::all(client);

        let services = svc_api
            .list(&ListParams::default().limit(1000))
            .await?;
        let secrets = secret_api
            .list(&ListParams::default().limit(1000))
            .await?;

        let mut out = Vec::new();
        for svc in services.items {
            let ns = svc
                .metadata
                .namespace
                .clone()
                .unwrap_or_default();
            let name = svc.metadata.name.clone().unwrap_or_default();
            if name.is_empty() || ns.is_empty() {
                continue;
            }

            let port = svc
                .spec
                .as_ref()
                .and_then(|s| s.ports.as_ref())
                .and_then(|ports| ports.iter().find(|p| p.port == 5432))
                .map(|p| p.port as u16);
            if port.is_none() {
                continue;
            }

            let labels = svc.metadata.labels.clone().unwrap_or_default();
            let is_bitnami = labels
                .get("app.kubernetes.io/name")
                .map(|v| v == "postgresql")
                .unwrap_or(false);

            let (source, hint_secret) = if is_bitnami {
                let release = labels
                    .get("app.kubernetes.io/instance")
                    .cloned()
                    .unwrap_or_else(|| name.clone());
                (Source::K8sBitnami, Some(format!("{}-postgresql", release)))
            } else {
                (Source::K8sGeneric, None)
            };

            let secret_name = match hint_secret {
                Some(s) if secret_exists(&secrets.items, &ns, &s) => Some(s),
                _ => find_secret_for(&secrets.items, &ns, &name),
            };

            let password_ref = match &secret_name {
                Some(sn) => format!("k8s-secret://{}/{}/password", ns, sn),
                None => continue,
            };

            let secret_key = pick_password_key(&secrets.items, &ns, secret_name.as_deref().unwrap());
            let password_ref = if let Some(k) = secret_key {
                format!("k8s-secret://{}/{}/{}", ns, secret_name.as_ref().unwrap(), k)
            } else {
                password_ref
            };

            let host = format!("{}.{}.svc.cluster.local", name, ns);
            out.push(DatabaseEntry {
                alias: make_alias(
                    if is_bitnami { "bitnami" } else { "k8s" },
                    &format!("{}-{}", ns, name),
                ),
                host,
                port: 5432,
                user: "postgres".to_string(),
                database: "postgres".to_string(),
                password_ref,
                source,
                description: Some(format!("Service Postgres em {}/{}", ns, name)),
                cluster_ref: Some(format!("{}/{}", ns, name)),
                container_id: None,
                discovered_at: Some(now()),
            });
        }

        Ok(out)
    }
}

fn secret_exists(secrets: &[Secret], ns: &str, name: &str) -> bool {
    secrets.iter().any(|s| {
        s.metadata.namespace.as_deref() == Some(ns)
            && s.metadata.name.as_deref() == Some(name)
    })
}

fn find_secret_for(secrets: &[Secret], ns: &str, svc_name: &str) -> Option<String> {
    let candidates = [
        format!("{}-postgresql", svc_name),
        format!("{}-postgres", svc_name),
        format!("{}-credentials", svc_name),
        svc_name.to_string(),
    ];
    for c in &candidates {
        if secret_exists(secrets, ns, c) {
            return Some(c.clone());
        }
    }
    None
}

fn pick_password_key(secrets: &[Secret], ns: &str, name: &str) -> Option<String> {
    let secret = secrets.iter().find(|s| {
        s.metadata.namespace.as_deref() == Some(ns) && s.metadata.name.as_deref() == Some(name)
    })?;
    let data = secret.data.as_ref()?;
    for key in [
        "password",
        "postgres-password",
        "POSTGRES_PASSWORD",
        "PGPASSWORD",
    ] {
        if data.contains_key(key) {
            return Some(key.to_string());
        }
    }
    data.keys().next().cloned()
}
