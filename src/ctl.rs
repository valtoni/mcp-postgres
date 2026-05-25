use anyhow::{bail, Context, Result};
use chrono::Local;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub async fn run_ctl(args: &[String]) -> Result<()> {
    if args.is_empty() {
        print_usage();
        return Ok(());
    }

    match args[0].as_str() {
        "init" => handle_init().await?,
        "status" => handle_status().await?,
        "install" => {
            if args.len() < 2 {
                bail!("Uso: mcp-dba-postgres ctl install [claude | gemini | codex | opencode]");
            }
            handle_install(&args[1]).await?;
        }
        "uninstall" => {
            if args.len() < 2 {
                bail!("Uso: mcp-dba-postgres ctl uninstall [claude | gemini | codex | opencode]");
            }
            handle_uninstall(&args[1]).await?;
        }
        "discover" => handle_discover().await?,
        _ => {
            eprintln!("Subcomando desconhecido: '{}'", args[0]);
            print_usage();
        }
    }

    Ok(())
}

fn print_usage() {
    println!("Uso: mcp-dba-postgres ctl [subcomando]");
    println!();
    println!("Subcomandos disponíveis:");
    println!("  init                 Cria um databases.yaml de exemplo no diretório atual");
    println!("  status               Verifica conexões e credenciais de todas as bases");
    println!("  install [cliente]    Configura o MCP server no cliente (claude, gemini, codex, opencode)");
    println!("  uninstall [cliente]  Remove a configuração do MCP server do cliente");
    println!("  discover             Executa autodescoberta local/k8s de bancos e atualiza o YAML");
}

async fn handle_init() -> Result<()> {
    let path = Path::new("databases.yaml");
    if path.exists() {
        println!("databases.yaml já existe no diretório atual.");
        return Ok(());
    }

    let template = r#"version: 1
default: local
databases:
  - alias: local
    host: host.docker.internal
    port: 5432
    user: postgres
    database: postgres
    password_ref: "env://PGPASSWORD"
    source: static
    description: "Postgres do dev local"
"#;

    fs::write(path, template).context("Falha ao criar databases.yaml")?;
    println!("[✔] databases.yaml criado com sucesso no diretório atual!");
    Ok(())
}

async fn handle_status() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let registry = crate::registry::Registry::load_or_legacy(&cwd)
        .await
        .context("Erro ao carregar o inventário de bancos de dados")?;

    let k8s = crate::discovery::k8s_client::K8sHandle::new();
    let credentials = crate::credentials::CredentialStore::new(k8s);
    let dbs = registry.list().await;

    if dbs.is_empty() {
        println!("Nenhum banco de dados configurado no databases.yaml.");
        return Ok(());
    }

    println!("Verificando conexões do databases.yaml...");
    for db in dbs {
        match credentials.resolve(&db.alias, &db.password_ref).await {
            Ok(pwd) => {
                match crate::pool::connect_with(&db, &pwd).await {
                    Ok(client) => {
                        match client.simple_query("SELECT version()").await {
                            Ok(rows) => {
                                let version = rows.first()
                                    .and_then(|r| match r {
                                        tokio_postgres::SimpleQueryMessage::Row(row) => row.get(0),
                                        _ => None
                                    })
                                    .unwrap_or("Postgres");
                                println!("[✔] {} -> Conectado com sucesso ({})", db.alias, version.split_whitespace().take(2).collect::<Vec<&str>>().join(" "));
                            }
                            Err(e) => {
                                println!("[✘] {} -> Erro ao executar query de teste: {}", db.alias, e);
                            }
                        }
                    }
                    Err(e) => {
                        println!("[✘] {} -> Erro ao conectar ao Postgres: {:#}", db.alias, e);
                    }
                }
            }
            Err(e) => {
                println!("[✘] {} -> Erro ao resolver senha ref '{}': {:#}", db.alias, db.password_ref, e);
            }
        }
    }

    Ok(())
}

async fn handle_discover() -> Result<()> {
    println!("Iniciando autodescoberta de bancos de dados (Kubernetes + Docker local)...");
    let cwd = std::env::current_dir()?;
    let registry = crate::registry::Registry::load_or_legacy(&cwd).await?;
    let k8s = crate::discovery::k8s_client::K8sHandle::new();
    
    let runner = crate::discovery::DiscoveryRunner {
        include_local: true,
        sources_filter: None,
        k8s: k8s.clone(),
    };
    let report = runner.run().await;
    
    registry.ensure_persistent(&cwd).await?;
    let merge = registry.merge_discovered(report.found.clone(), false).await?;
    
    let payload = json!({
        "found_count": report.found.len(),
        "added": merge.added,
        "updated": merge.updated,
        "skipped": merge.skipped,
        "errors": report.errors,
    });
    
    println!("[✔] Descoberta concluída!");
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

// OS-specific path and configuration helper
struct ClientConfig {
    label: &'static str,
    path: PathBuf,
    is_toml: bool,
    is_vscode: bool,
}

fn is_windows_host() -> bool {
    if cfg!(target_os = "windows") {
        return true;
    }
    if std::env::var("HOST_OS").unwrap_or_default().to_lowercase() == "windows" {
        return true;
    }
    if std::env::var("OS").unwrap_or_default().to_lowercase().contains("windows") {
        return true;
    }
    if let Ok(kpath) = std::env::var("KUBECONFIG") {
        if kpath.contains('\\') || kpath.starts_with("C:") || kpath.starts_with("c:") {
            return true;
        }
    }
    false
}

fn print_manual_config(client: &str, kubeconfig: &Option<String>) -> Result<()> {
    let is_windows = is_windows_host();
    let host_kubeconfig = kubeconfig.clone();
    
    match client {
        "claude" => {
            let mcp_entry = json!({
                "mcpServers": {
                    "mcp-dba-postgres": {
                        "command": if is_windows { "cmd.exe" } else { "sh" },
                        "args": get_docker_args(host_kubeconfig)
                    }
                }
            });
            println!("  [!] Adicione a seguinte configuração ao seu arquivo 'claude_desktop_config.json' do host:");
            println!("{}", serde_json::to_string_pretty(&mcp_entry)?);
        }
        "gemini" => {
            let mcp_entry = json!({
                "mcpServers": {
                    "mcp-dba-postgres": {
                        "command": if is_windows { "cmd.exe" } else { "sh" },
                        "args": get_docker_args(host_kubeconfig)
                    }
                }
            });
            println!("  [!] Adicione a seguinte configuração ao seu arquivo '.gemini/settings.json' do host:");
            println!("{}", serde_json::to_string_pretty(&mcp_entry)?);
        }
        "codex" => {
            let docker_cmd = get_docker_args(host_kubeconfig)[1].clone();
            let docker_cmd_escaped = docker_cmd.replace("\\", "\\\\");
            let block = format!(
                "[mcp_servers.mcp-dba-postgres]\ncommand = \"{}\"\nargs = [\"{}\", \"{}\"]\n",
                if is_windows { "cmd.exe" } else { "sh" },
                if is_windows { "/c" } else { "-c" },
                docker_cmd_escaped
            );
            println!("  [!] Adicione o seguinte bloco ao seu arquivo '.codex/config.toml' do host:");
            println!("{}", block);
        }
        "opencode" => {
            let mcp_entry = json!({
                "mcp.servers": {
                    "mcp-dba-postgres": {
                        "command": if is_windows { "cmd.exe" } else { "sh" },
                        "args": get_docker_args(host_kubeconfig)
                    }
                }
            });
            println!("  [!] Adicione a seguinte configuração ao seu arquivo '.vscode/settings.json' do workspace:");
            println!("{}", serde_json::to_string_pretty(&mcp_entry)?);
        }
        _ => {}
    }
    
    if !cfg!(target_os = "windows") && is_windows {
        println!("\n  💡 Dica para Docker no Windows:");
        println!("  Para configurar automaticamente a partir deste container Linux, monte suas pastas do host usando:");
        println!("    -v \"%APPDATA%:/appdata\" (para Claude)");
        println!("    -v \"%USERPROFILE%:/userprofile\" (para Gemini/Codex)");
    }
    
    Ok(())
}

fn get_client_config(client: &str) -> Result<ClientConfig> {
    let is_windows = is_windows_host();
    
    match client {
        "claude" => {
            let path = if is_windows {
                if Path::new("/appdata").exists() {
                    PathBuf::from("/appdata").join("Claude").join("claude_desktop_config.json")
                } else if let Ok(appdata) = std::env::var("APPDATA") {
                    PathBuf::from(appdata).join("Claude").join("claude_desktop_config.json")
                } else {
                    PathBuf::from("claude_desktop_config.json")
                }
            } else {
                let home = std::env::var("HOME").context("Variável HOME não encontrada")?;
                PathBuf::from(home).join(".config").join("Claude").join("claude_desktop_config.json")
            };
            Ok(ClientConfig {
                label: "Claude Desktop",
                path,
                is_toml: false,
                is_vscode: false,
            })
        }
        "gemini" => {
            let path = if is_windows {
                if Path::new("/userprofile").exists() {
                    PathBuf::from("/userprofile").join(".gemini").join("settings.json")
                } else if let Ok(home) = std::env::var("USERPROFILE") {
                    PathBuf::from(home).join(".gemini").join("settings.json")
                } else {
                    PathBuf::from("settings.json")
                }
            } else {
                let home = std::env::var("HOME").context("Pasta pessoal do usuário não encontrada")?;
                PathBuf::from(home).join(".gemini").join("settings.json")
            };
            Ok(ClientConfig {
                label: "Gemini CLI/Code",
                path,
                is_toml: false,
                is_vscode: false,
            })
        }
        "codex" => {
            let path = if is_windows {
                if Path::new("/userprofile").exists() {
                    PathBuf::from("/userprofile").join(".codex").join("config.toml")
                } else if let Ok(home) = std::env::var("USERPROFILE") {
                    PathBuf::from(home).join(".codex").join("config.toml")
                } else {
                    PathBuf::from("config.toml")
                }
            } else {
                let home = std::env::var("HOME").context("Pasta pessoal do usuário não encontrada")?;
                PathBuf::from(home).join(".codex").join("config.toml")
            };
            Ok(ClientConfig {
                label: "Codex CLI",
                path,
                is_toml: true,
                is_vscode: false,
            })
        }
        "opencode" => {
            let path = PathBuf::from(".vscode").join("settings.json");
            Ok(ClientConfig {
                label: "OpenCode VS Code Workspace",
                path,
                is_toml: false,
                is_vscode: true,
            })
        }
        _ => bail!("Cliente inválido. Escolha entre: claude, gemini, codex, opencode"),
    }
}

fn create_backup(path: &Path) -> Result<()> {
    if path.exists() && path.file_name().map(|n| n != "claude_desktop_config.json" && n != "settings.json" && n != "config.toml").unwrap_or(true) {
        let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
        let backup_path = path.with_extension(format!("bak.{}", stamp));
        fs::copy(path, &backup_path)
            .with_context(|| format!("Falha ao criar backup de {:?}", path))?;
        println!("  .. Criado backup da configuração em {:?}", backup_path);
    }
    Ok(())
}

fn get_docker_args(kubeconfig: Option<String>) -> Vec<String> {
    let is_windows = is_windows_host();
    let mut cmd = if is_windows {
        "docker run -i --rm -e PGPASSWORD -v %cd%:/project -w /project".to_string()
    } else {
        "docker run -i --rm -e PGPASSWORD -v $PWD:/project -w /project".to_string()
    };

    if let Some(ref kpath) = kubeconfig {
        if !kpath.trim().is_empty() {
            let kpath_escaped = kpath.replace("\\", "/");
            cmd.push_str(&format!(" -e KUBECONFIG=/kubeconfig -v {}:/kubeconfig:ro", kpath_escaped));
        }
    } else {
        if is_windows {
            cmd.push_str(" -v %USERPROFILE%\\.kube:/root/.kube:ro");
        } else {
            cmd.push_str(" -v $HOME/.kube:/root/.kube:ro");
        }
    }

    if is_windows {
        cmd.push_str(" -v //./pipe/docker_engine://./pipe/docker_engine valtoni/mcp-dba-postgres:1.1");
    } else {
        cmd.push_str(" -v /var/run/docker.sock:/var/run/docker.sock valtoni/mcp-dba-postgres:1.1");
    }

    vec![
        if is_windows { "/c" } else { "-c" }.to_string(),
        cmd,
    ]
}

async fn handle_install(client: &str) -> Result<()> {
    let config = get_client_config(client)?;
    let is_windows = is_windows_host();
    let host_kubeconfig = std::env::var("KUBECONFIG").ok();

    // Verify if we can access/write to the configuration directory
    let print_only = if let Some(parent) = config.path.parent() {
        if parent.as_os_str().is_empty() || parent == Path::new("") {
            true
        } else {
            match fs::create_dir_all(parent) {
                Ok(_) => false,
                Err(_) => true,
            }
        }
    } else {
        true
    };

    if print_only {
        println!("Configurando {} (modo exibição/cópia manual):", config.label);
        print_manual_config(client, &host_kubeconfig)?;
        return Ok(());
    }

    println!("Configurando {} em {:?}", config.label, config.path);
    if let Err(e) = create_backup(&config.path) {
        println!("  [!] Alerta ao criar backup: {}. Prosseguindo...", e);
    }

    if config.is_toml {
        let docker_cmd = get_docker_args(host_kubeconfig.clone())[1].clone();
        let docker_cmd_escaped = docker_cmd.replace("\\", "\\\\");
        
        let block = format!(
            "[mcp_servers.mcp-dba-postgres]\ncommand = \"{}\"\nargs = [\"{}\", \"{}\"]\n",
            if is_windows { "cmd.exe" } else { "sh" },
            if is_windows { "/c" } else { "-c" },
            docker_cmd_escaped
        );

        let mut content = if config.path.exists() {
            fs::read_to_string(&config.path).unwrap_or_default()
        } else {
            String::new()
        };

        let header = "[mcp_servers.mcp-dba-postgres]";
        if content.contains(header) {
            println!("  .. Entrada 'mcp-dba-postgres' já existe no Codex. Atualizando...");
            let lines: Vec<&str> = content.lines().collect();
            let mut new_lines = Vec::new();
            let mut skip = false;
            for line in lines {
                if line.trim() == header {
                    skip = true;
                    continue;
                }
                if skip && line.trim().starts_with('[') {
                    skip = false;
                }
                if !skip {
                    new_lines.push(line);
                }
            }
            content = new_lines.join("\n");
            content.push('\n');
            content.push_str(&block);
        } else {
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&block);
        }

        if let Err(e) = fs::write(&config.path, content) {
            println!("  [!] Falha ao gravar arquivo de configuração: {}. Exibindo para cópia manual:", e);
            print_manual_config(client, &host_kubeconfig)?;
        } else {
            println!("[✔] Integração para {} configurada com sucesso!", config.label);
        }
    } else {
        // Handle JSON config
        let mut root: Value = if config.path.exists() {
            let data = fs::read_to_string(&config.path).unwrap_or_default();
            if data.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&data).unwrap_or(json!({}))
            }
        } else {
            json!({})
        };

        let key = if config.is_vscode { "mcp.servers" } else { "mcpServers" };
        if root.get(key).is_none() {
            if let Some(obj) = root.as_object_mut() {
                obj.insert(key.to_string(), json!({}));
            }
        }

        let mcp_entry = json!({
            "command": if is_windows { "cmd.exe" } else { "sh" },
            "args": get_docker_args(host_kubeconfig.clone())
        });

        if let Some(servers) = root.get_mut(key).and_then(|s| s.as_object_mut()) {
            servers.insert("mcp-dba-postgres".to_string(), mcp_entry);
        }

        match serde_json::to_string_pretty(&root) {
            Ok(output) => {
                if let Err(e) = fs::write(&config.path, output) {
                    println!("  [!] Falha ao gravar arquivo JSON: {}. Exibindo para cópia manual:", e);
                    print_manual_config(client, &host_kubeconfig)?;
                } else {
                    println!("[✔] Integração para {} configurada com sucesso!", config.label);
                }
            }
            Err(e) => {
                println!("  [!] Falha ao serializar JSON: {}. Exibindo para cópia manual:", e);
                print_manual_config(client, &host_kubeconfig)?;
            }
        }
    }

    Ok(())
}

async fn handle_uninstall(client: &str) -> Result<()> {
    let config = get_client_config(client)?;
    if !config.path.exists() {
        println!("Configuração para {} não encontrada em {:?}.", config.label, config.path);
        return Ok(());
    }

    println!("Removendo {} de {:?}", config.label, config.path);
    create_backup(&config.path)?;

    if config.is_toml {
        let content = fs::read_to_string(&config.path).context("Falha ao ler Codex TOML")?;
        let header = "[mcp_servers.mcp-dba-postgres]";
        if !content.contains(header) {
            println!("Entrada 'mcp-dba-postgres' não encontrada no Codex.");
            return Ok(());
        }

        let lines: Vec<&str> = content.lines().collect();
        let mut new_lines = Vec::new();
        let mut skip = false;
        for line in lines {
            if line.trim() == header {
                skip = true;
                continue;
            }
            if skip && line.trim().starts_with('[') {
                skip = false;
            }
            if !skip {
                new_lines.push(line);
            }
        }
        let output = new_lines.join("\n");
        fs::write(&config.path, output).context("Falha ao escrever Codex TOML")?;
    } else {
        let mut root: Value = {
            let data = fs::read_to_string(&config.path).context("Falha ao ler JSON de configuração")?;
            serde_json::from_str(&data).context("Falha ao parsear JSON de configuração")?
        };

        let key = if config.is_vscode { "mcp.servers" } else { "mcpServers" };
        let mut removed = false;

        if let Some(servers) = root.get_mut(key).and_then(|s| s.as_object_mut()) {
            if servers.remove("mcp-dba-postgres").is_some() {
                removed = true;
            }
        }

        if removed {
            let output = serde_json::to_string_pretty(&root)?;
            fs::write(&config.path, output).context("Falha ao escrever JSON de configuração")?;
            println!("[✔] Integração para {} removida com sucesso!", config.label);
        } else {
            println!("Entrada 'mcp-dba-postgres' não encontrada na configuração.");
        }
    }

    Ok(())
}
