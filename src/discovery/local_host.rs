use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::{now, DatabaseDiscoverer};
use crate::registry::{DatabaseEntry, Source};

pub struct LocalHostDiscoverer;

#[async_trait]
impl DatabaseDiscoverer for LocalHostDiscoverer {
    fn name(&self) -> &'static str {
        "local-host"
    }

    async fn discover(&self) -> Result<Vec<DatabaseEntry>> {
        let mut out = Vec::new();
        for host in ["127.0.0.1", "host.docker.internal"] {
            if probe(host, 5432).await {
                out.push(DatabaseEntry {
                    alias: format!("local-{}", host.replace('.', "-")),
                    host: host.to_string(),
                    port: 5432,
                    user: std::env::var("PGUSER").unwrap_or_else(|_| "postgres".to_string()),
                    database: std::env::var("PGDATABASE").unwrap_or_else(|_| "postgres".to_string()),
                    password_ref: "env://PGPASSWORD".to_string(),
                    source: Source::LocalHost,
                    description: Some(format!("Postgres em {}:5432", host)),
                    cluster_ref: None,
                    container_id: None,
                    discovered_at: Some(now()),
                });
            }
        }
        Ok(out)
    }
}

async fn probe(host: &str, port: u16) -> bool {
    let addr = format!("{}:{}", host, port);
    matches!(
        timeout(Duration::from_millis(500), TcpStream::connect(&addr)).await,
        Ok(Ok(_))
    )
}
