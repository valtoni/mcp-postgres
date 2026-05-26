use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

use crate::credentials::CredentialStore;
use crate::registry::{DatabaseEntry, Registry};
use crate::discovery::k8s_client::K8sHandle;

pub struct ConnectionPool {
    clients: Mutex<HashMap<String, Arc<Client>>>,
    registry: Arc<Registry>,
    credentials: Arc<CredentialStore>,
    k8s: Arc<K8sHandle>,
}

impl ConnectionPool {
    pub fn new(
        registry: Arc<Registry>,
        credentials: Arc<CredentialStore>,
        k8s: Arc<K8sHandle>,
    ) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            registry,
            credentials,
            k8s,
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

        let client = if let Some(dsn) = parse_dsn(&password) {
            // Override host, port, user, database dynamically from the DSN connection string
            let mut dsn_entry = entry.clone();
            dsn_entry.host = dsn.host;
            dsn_entry.port = dsn.port;
            dsn_entry.user = dsn.user;
            dsn_entry.database = dsn.database;

            // If the original host is a K8s service, or we have a cluster_ref, but the DSN host is a short name,
            // we enrich it to a fully-qualified in-cluster DNS so that parse_k8s_svc matches and triggers port-forward!
            if parse_k8s_svc(&dsn_entry.host).is_none() {
                if let Some((_, orig_ns)) = parse_k8s_svc(&entry.host) {
                    dsn_entry.host = format!("{}.{}.svc.cluster.local", dsn_entry.host, orig_ns);
                } else if let Some(ref cref) = entry.cluster_ref {
                    let parts: Vec<&str> = cref.split('/').collect();
                    if parts.len() == 2 {
                        dsn_entry.host = format!("{}.{}.svc.cluster.local", dsn_entry.host, parts[0]);
                    }
                }
            }

            if let Some((svc_name, ns)) = parse_k8s_svc(&dsn_entry.host) {
                connect_k8s_portforward(&self.k8s, &ns, &svc_name, &dsn_entry, &dsn.password).await?
            } else {
                connect_with(&dsn_entry, &dsn.password).await?
            }
        } else {
            if let Some((svc_name, ns)) = parse_k8s_svc(&entry.host) {
                connect_k8s_portforward(&self.k8s, &ns, &svc_name, &entry, &password).await?
            } else {
                connect_with(&entry, &password).await?
            }
        };

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

struct DsnInfo {
    user: String,
    password: String,
    host: String,
    port: u16,
    database: String,
}

fn parse_dsn(dsn: &str) -> Option<DsnInfo> {
    if !dsn.starts_with("postgresql://") && !dsn.starts_with("postgres://") {
        return None;
    }
    
    let s = dsn.strip_prefix("postgresql://")
        .or_else(|| dsn.strip_prefix("postgres://"))?;
    
    // Split user/password and host/port/database at '@'
    let parts: Vec<&str> = s.split('@').collect();
    if parts.len() != 2 {
        return None;
    }
    
    let user_pass: Vec<&str> = parts[0].splitn(2, ':').collect();
    let user = user_pass[0].to_string();
    let password = user_pass.get(1).map(|v| v.to_string()).unwrap_or_default();
    
    let host_port_db: Vec<&str> = parts[1].splitn(2, '/').collect();
    let host_port: Vec<&str> = host_port_db[0].split(':').collect();
    let host = host_port[0].to_string();
    let port = host_port.get(1).and_then(|p| p.parse::<u16>().ok()).unwrap_or(5432);
    let database = host_port_db.get(1).map(|v| v.to_string()).unwrap_or_default();
    
    Some(DsnInfo {
        user,
        password,
        host,
        port,
        database,
    })
}

fn parse_k8s_svc(host: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() >= 3 && parts[2] == "svc" {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

async fn connect_k8s_portforward(
    k8s: &Arc<crate::discovery::k8s_client::K8sHandle>,
    namespace: &str,
    service_name: &str,
    entry: &DatabaseEntry,
    password: &str,
) -> Result<Client> {
    eprintln!(
        "Conectando via Port-Forward Kubernetes ao serviço {}/{} na porta {}",
        namespace, service_name, entry.port
    );

    let client = k8s.client().await?;
    
    // 1. Obter endpoints do serviço para descobrir o pod ativo
    use k8s_openapi::api::core::v1::Endpoints;
    use kube::Api;
    let ep_api: Api<Endpoints> = Api::namespaced(client.clone(), namespace);
    let ep = ep_api
        .get(service_name)
        .await
        .with_context(|| format!("Falha ao ler Endpoints para o serviço {}/{}", namespace, service_name))?;

    let mut pod_name = None;
    if let Some(subsets) = ep.subsets {
        for subset in subsets {
            if let Some(addresses) = subset.addresses {
                for addr in addresses {
                    if let Some(target_ref) = addr.target_ref {
                        if target_ref.kind.as_deref() == Some("Pod") {
                            if let Some(name) = target_ref.name {
                                pod_name = Some(name);
                                break;
                            }
                        }
                    }
                }
            }
            if pod_name.is_some() { break; }
        }
    }

    let pod = pod_name.ok_or_else(|| {
        anyhow!(
            "Nenhum pod ativo encontrado para o serviço {}/{} nos Endpoints",
            namespace, service_name
        )
    })?;

    eprintln!("Selecionado o pod {} para estabelecer túnel de port-forward...", pod);

    // 2. Estabelecer portforward para o pod
    use k8s_openapi::api::core::v1::Pod;
    let pod_api: Api<Pod> = Api::namespaced(client, namespace);
    let mut pf = pod_api
        .portforward(&pod, &[entry.port as u16])
        .await
        .with_context(|| format!("Falha ao estabelecer portforward para o pod {} na porta {}", pod, entry.port))?;

    let stream = pf
        .take_stream(entry.port as u16)
        .ok_or_else(|| anyhow!("Falha ao extrair stream de portforward para {}:{}", pod, entry.port))?;

    // 3. Conectar usando tokio_postgres::connect_raw
    let config_str = format!(
        "user={} password={} dbname={}",
        entry.user, password, entry.database
    );
    let config: tokio_postgres::Config = config_str.parse()
        .context("Falha ao analisar a string de conexão do Postgres")?;

    let (postgres_client, connection) = config
        .connect_raw(stream, tokio_postgres::NoTls)
        .await
        .context("Falha no handshake do Postgres sobre o túnel portforward")?;

    // 4. Iniciar thread em background para gerenciar a conexão e o portforwarder
    tokio::spawn(async move {
        let _keep_alive = pf;
        if let Err(e) = connection.await {
            eprintln!("Erro na conexão portforward de background: {}", e);
        }
    });

    Ok(postgres_client)
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
