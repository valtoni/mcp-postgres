use anyhow::{Context, Result};
use std::env;
use tokio_postgres::{Client, NoTls};

pub async fn connect() -> Result<Client> {
    // Tenta carregar as credenciais a partir de um arquivo .mcp_postgres local
    if let Ok(p) = env::current_dir() {
        let config_path = p.join(".mcp_postgres");
        if config_path.exists() {
            eprintln!("Carregando configuracoes de projeto locais: {:?}", config_path);
            let _ = dotenvy::from_path(&config_path);
        } else {
            eprintln!("Nenhum '.mcp_postgres' encontrado em {:?} (utilizando variaveis de ambiente globais se existirem)", p);
        }
    }

    let host = env::var("PGHOST").unwrap_or_else(|_| "localhost".to_string());
    let user = env::var("PGUSER").unwrap_or_else(|_| "postgres".to_string());
    let pass = env::var("PGPASSWORD").unwrap_or_else(|_| "".to_string());
    let dbname = env::var("PGDATABASE").unwrap_or_else(|_| "postgres".to_string());
    let port = env::var("PGPORT").unwrap_or_else(|_| "5432".to_string());

    let conn_str = format!(
        "host={} user={} password={} dbname={} port={}",
        host, user, pass, dbname, port
    );
    
    eprintln!("Connecting to PostgreSQL: {}@{}:{}", user, host, port);
    
    let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
        .await
        .context("Failed to connect to postgres database")?;

    // The connection object performs the actual communication, spawn it in a task
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("Connection error: {}", e);
        }
    });

    Ok(client)
}
