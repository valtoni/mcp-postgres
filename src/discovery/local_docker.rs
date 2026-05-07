use anyhow::{Context, Result};
use async_trait::async_trait;
use bollard::container::ListContainersOptions;
use bollard::Docker;
use std::collections::HashMap;

use super::{make_alias, now, DatabaseDiscoverer};
use crate::registry::{DatabaseEntry, Source};

pub struct LocalDockerDiscoverer;

#[async_trait]
impl DatabaseDiscoverer for LocalDockerDiscoverer {
    fn name(&self) -> &'static str {
        "local-docker"
    }

    async fn discover(&self) -> Result<Vec<DatabaseEntry>> {
        let docker = Docker::connect_with_local_defaults()
            .context("falha conectando ao Docker (socket/named-pipe nao disponivel)")?;

        let opts = ListContainersOptions::<String> {
            all: false,
            ..Default::default()
        };

        let containers = docker
            .list_containers(Some(opts))
            .await
            .context("falha listando containers Docker")?;

        let mut out = Vec::new();
        for c in containers {
            let image = c.image.clone().unwrap_or_default();
            if !is_postgres_image(&image) {
                continue;
            }

            let name = c
                .names
                .as_ref()
                .and_then(|n| n.first().cloned())
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_else(|| c.id.clone().unwrap_or_default());

            let host_port = c
                .ports
                .as_ref()
                .and_then(|ports| {
                    ports
                        .iter()
                        .find(|p| p.private_port == 5432 && p.public_port.is_some())
                        .and_then(|p| p.public_port)
                })
                .map(|p| p as u16);

            let host_port = match host_port {
                Some(p) => p,
                None => continue,
            };

            let id = c.id.clone().unwrap_or_default();
            let env = inspect_env(&docker, &id).await.unwrap_or_default();

            let user = env
                .get("POSTGRES_USER")
                .cloned()
                .unwrap_or_else(|| "postgres".to_string());
            let database = env
                .get("POSTGRES_DB")
                .cloned()
                .unwrap_or_else(|| user.clone());

            out.push(DatabaseEntry {
                alias: make_alias("docker", &name),
                host: "127.0.0.1".to_string(),
                port: host_port,
                user,
                database,
                password_ref: format!("env://PGPASSWORD_{}", env_var_name(&name)),
                source: Source::LocalDocker,
                description: Some(format!("Container Docker {} ({})", name, image)),
                cluster_ref: None,
                container_id: Some(id),
                discovered_at: Some(now()),
            });
        }

        Ok(out)
    }
}

fn is_postgres_image(image: &str) -> bool {
    let lower = image.to_lowercase();
    lower.starts_with("postgres")
        || lower.starts_with("bitnami/postgresql")
        || lower.contains("/postgres:")
        || lower.contains("/postgresql:")
}

fn env_var_name(container_name: &str) -> String {
    container_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

async fn inspect_env(docker: &Docker, id: &str) -> Result<HashMap<String, String>> {
    let inspect = docker
        .inspect_container(id, None)
        .await
        .context("inspect_container falhou")?;
    let mut out = HashMap::new();
    if let Some(cfg) = inspect.config {
        if let Some(env) = cfg.env {
            for kv in env {
                if let Some((k, v)) = kv.split_once('=') {
                    out.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    Ok(out)
}
