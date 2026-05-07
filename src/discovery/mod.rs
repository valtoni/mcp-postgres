pub mod cnpg;
pub mod generic;
pub mod k8s_client;
pub mod local_docker;
pub mod local_host;
pub mod zalando;

use async_trait::async_trait;
use futures::future::join_all;
use serde::Serialize;
use std::sync::Arc;

use crate::registry::{DatabaseEntry, Source};

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryReport {
    pub found: Vec<DatabaseEntry>,
    pub errors: Vec<DiscoveryError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryError {
    pub source: String,
    pub message: String,
}

#[async_trait]
pub trait DatabaseDiscoverer: Send + Sync {
    fn name(&self) -> &'static str;
    async fn discover(&self) -> anyhow::Result<Vec<DatabaseEntry>>;
}

pub struct DiscoveryRunner {
    pub include_local: bool,
    pub sources_filter: Option<Vec<String>>,
    pub k8s: Arc<k8s_client::K8sHandle>,
}

impl DiscoveryRunner {
    pub async fn run(&self) -> DiscoveryReport {
        let mut discoverers: Vec<Box<dyn DatabaseDiscoverer>> = Vec::new();

        if self.matches_filter("k8s-cnpg") {
            discoverers.push(Box::new(cnpg::CnpgDiscoverer {
                k8s: self.k8s.clone(),
            }));
        }
        if self.matches_filter("k8s-zalando") {
            discoverers.push(Box::new(zalando::ZalandoDiscoverer {
                k8s: self.k8s.clone(),
            }));
        }
        if self.matches_filter("k8s-generic") {
            discoverers.push(Box::new(generic::GenericDiscoverer {
                k8s: self.k8s.clone(),
            }));
        }
        if self.include_local && self.matches_filter("local-host") {
            discoverers.push(Box::new(local_host::LocalHostDiscoverer));
        }
        if self.include_local && self.matches_filter("local-docker") {
            discoverers.push(Box::new(local_docker::LocalDockerDiscoverer));
        }

        let futures: Vec<_> = discoverers
            .iter()
            .map(|d| async move {
                let name = d.name();
                match d.discover().await {
                    Ok(entries) => (name, Ok(entries)),
                    Err(e) => (name, Err(e)),
                }
            })
            .collect();

        let results = join_all(futures).await;

        let mut found: Vec<DatabaseEntry> = Vec::new();
        let mut errors: Vec<DiscoveryError> = Vec::new();
        for (name, result) in results {
            match result {
                Ok(entries) => found.extend(entries),
                Err(e) => errors.push(DiscoveryError {
                    source: name.to_string(),
                    message: format!("{:#}", e),
                }),
            }
        }

        let found = dedup(found);
        DiscoveryReport { found, errors }
    }

    fn matches_filter(&self, source: &str) -> bool {
        match &self.sources_filter {
            None => true,
            Some(list) => list.iter().any(|s| s == source),
        }
    }
}

fn dedup(entries: Vec<DatabaseEntry>) -> Vec<DatabaseEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let key = (e.host.clone(), e.port, e.database.clone());
        if seen.insert(key) {
            out.push(e);
        }
    }
    out
}

pub fn make_alias(prefix: &str, name: &str) -> String {
    let normalized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{}-{}", prefix, normalized.trim_matches('-'))
}

pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

pub fn _source_label(s: &Source) -> &'static str {
    s.as_str()
}
