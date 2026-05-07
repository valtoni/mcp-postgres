use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::credentials::CredentialStore;
use crate::discovery::{k8s_client::K8sHandle, DiscoveryRunner};
use crate::pool::ConnectionPool;
use crate::registry::Registry;

pub struct AppState {
    pub registry: Arc<Registry>,
    pub credentials: Arc<CredentialStore>,
    pub pool: Arc<ConnectionPool>,
    pub k8s: Arc<K8sHandle>,
    pub cwd: PathBuf,
}

pub async fn handle_list_databases(state: &AppState) -> Result<Value> {
    let entries = state.registry.list().await;
    let default = state.registry.default_alias().await;

    let mut out = Vec::new();
    for e in entries {
        let credentials_loaded = state.credentials.has(&e.alias).await;
        out.push(json!({
            "alias": e.alias,
            "host": e.host,
            "port": e.port,
            "user": e.user,
            "database": e.database,
            "source": e.source.as_str(),
            "description": e.description,
            "cluster_ref": e.cluster_ref,
            "container_id": e.container_id,
            "discovered_at": e.discovered_at,
            "default": Some(&e.alias) == default.as_ref(),
            "credentials_loaded": credentials_loaded,
        }));
    }

    let payload = json!({
        "default": default,
        "persistent": state.registry.is_persistent().await,
        "databases": out,
    });
    Ok(crate::tools::json_response(&payload))
}

pub async fn handle_discover_k8s_databases(
    state: &AppState,
    args: &Value,
) -> Result<Value> {
    let include_local = args
        .get("include_local")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sources_filter = args.get("sources").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect::<Vec<_>>()
    });

    let runner = DiscoveryRunner {
        include_local,
        sources_filter,
        k8s: state.k8s.clone(),
    };

    let report = runner.run().await;

    let merge = if !dry_run {
        state
            .registry
            .ensure_persistent(&state.cwd)
            .await
            .map_err(|e| anyhow!("falha tornando registry persistente: {}", e))?;
        state
            .registry
            .merge_discovered(report.found.clone(), false)
            .await?
    } else {
        state
            .registry
            .merge_discovered(report.found.clone(), true)
            .await?
    };

    let payload = json!({
        "dry_run": dry_run,
        "found_count": report.found.len(),
        "added": merge.added,
        "updated": merge.updated,
        "skipped": merge.skipped,
        "errors": report.errors,
    });
    Ok(crate::tools::json_response(&payload))
}

pub async fn handle_set_database_credentials(
    state: &AppState,
    args: &Value,
) -> Result<Value> {
    let alias = args
        .get("database")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("parametro 'database' obrigatorio"))?;
    let password = args
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("parametro 'password' obrigatorio"))?;

    if state.registry.get(alias).await.is_none() {
        return Err(anyhow!("alias '{}' nao existe no registry", alias));
    }

    state.credentials.set_manual(alias, password.to_string()).await;
    state.pool.invalidate(alias).await;

    Ok(crate::tools::json_response(&json!({
        "ok": true,
        "alias": alias,
        "credentials_loaded": true,
    })))
}

pub async fn handle_set_default_database(
    state: &AppState,
    args: &Value,
) -> Result<Value> {
    let alias = args
        .get("database")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("parametro 'database' obrigatorio"))?;

    state
        .registry
        .ensure_persistent(&state.cwd)
        .await?;
    state.registry.set_default(alias).await?;

    Ok(crate::tools::json_response(&json!({
        "ok": true,
        "default": alias,
    })))
}
