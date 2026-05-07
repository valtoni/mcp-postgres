use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

use crate::credentials::CredentialStore;
use crate::registry::{DatabaseEntry, Registry};

pub struct ConnectionPool {
    clients: Mutex<HashMap<String, Arc<Client>>>,
    registry: Arc<Registry>,
    credentials: Arc<CredentialStore>,
}

impl ConnectionPool {
    pub fn new(registry: Arc<Registry>, credentials: Arc<CredentialStore>) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            registry,
            credentials,
        }
    }

    pub async fn invalidate(&self, alias: &str) {
        self.clients.lock().await.remove(alias);
    }

    pub async fn get(&self, alias: &str) -> Result<Arc<Client>> {
        if let Some(client) = self.lookup_alive(alias).await {
            return Ok(client);
        }

        let entry = self
            .registry
            .get(alias)
            .await
            .ok_or_else(|| anyhow!("alias '{}' nao encontrado em databases.yaml", alias))?;

        let password = self
            .credentials
            .resolve(alias, &entry.password_ref)
            .await?;

        let client = connect_with(&entry, &password).await?;
        let arc = Arc::new(client);
        self.clients
            .lock()
            .await
            .insert(alias.to_string(), arc.clone());
        Ok(arc)
    }

    async fn lookup_alive(&self, alias: &str) -> Option<Arc<Client>> {
        let mut guard = self.clients.lock().await;
        if let Some(client) = guard.get(alias) {
            if client.is_closed() {
                guard.remove(alias);
                return None;
            }
            return Some(client.clone());
        }
        None
    }
}

pub async fn connect_with(entry: &DatabaseEntry, password: &str) -> Result<Client> {
    let conn_str = format!(
        "host={} user={} password={} dbname={} port={}",
        entry.host, entry.user, password, entry.database, entry.port
    );

    eprintln!(
        "Conectando alias='{}' ao Postgres: {}@{}:{}/{}",
        entry.alias, entry.user, entry.host, entry.port, entry.database
    );

    let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
        .await
        .with_context(|| format!("falha conectando alias '{}'", entry.alias))?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("Connection error: {}", e);
        }
    });

    Ok(client)
}
