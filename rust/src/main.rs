#![allow(clippy::result_large_err)]
use nbmcp::config::{load_config, merge_dotenv, process_env, Env};
use nbmcp::error::{ErrorType, NbError};
use nbmcp::http::{self, HttpOptions};
use nbmcp::runtime::Runtime;
use nbmcp::server::{Registry, Session};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

const HELP: &str = "nbmcp: MCP server for NetBox (read, guarded write, plugins)

Usage: nbmcp [--transport stdio|http] [--host 127.0.0.1] [--port 8000] [--list-tools]
             [--netbox-url URL] [--netbox-token TOKEN] [--[no-]verify-ssl]
             [--enable-plugin-discovery true|false] [--log-level LEVEL]
             [--cors-origins a,b] [--mcp-auth-token TOKEN]

Options override the environment, which overrides a .env file in the current directory.
  NETBOX_URL, NETBOX_TOKEN            the NetBox instance
  NETBOX_ALLOW_WRITES=true            enable create/update (read-only by default)
  NETBOX_ALLOW_DELETE=true            also enable deletions and scripts
  NETBOX_DRY_RUN=true                 writes are previewed, never sent
  NETBOX_TOKEN_PASSTHROUGH=true       HTTP: each client's Bearer is its own NetBox token
See the README for the full list.
";

#[derive(Default)]
struct Cli {
    http: bool,
    host: Option<String>,
    port: Option<String>,
    list_tools: bool,
    help: bool,
    set: Vec<(&'static str, String)>,
}

fn parse_cli() -> Result<Cli, NbError> {
    let mut cli = Cli::default();
    let mut it = std::env::args().skip(1);
    let bad = |m: String| NbError::new(ErrorType::ConfigError, m);
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let mut value = |name: &str| inline.clone().or_else(|| it.next()).ok_or_else(|| bad(format!("{name} needs a value.")));
        match flag.as_str() {
            "--http" => cli.http = true,
            "--transport" => match value("--transport")?.as_str() {
                "http" | "streamable-http" => cli.http = true,
                "stdio" => cli.http = false,
                v => return Err(bad(format!("--transport: \"{v}\" is not stdio or http."))),
            },
            "--list-tools" => cli.list_tools = true,
            "-h" | "--help" => cli.help = true,
            "--host" => cli.host = Some(value("--host")?),
            "--port" => cli.port = Some(value("--port")?),
            "--netbox-url" => cli.set.push(("NETBOX_URL", value("--netbox-url")?)),
            "--netbox-token" => cli.set.push(("NETBOX_TOKEN", value("--netbox-token")?)),
            "--verify-ssl" => cli.set.push(("VERIFY_SSL", "true".into())),
            "--no-verify-ssl" => cli.set.push(("VERIFY_SSL", "false".into())),
            "--enable-plugin-discovery" => cli.set.push(("ENABLE_PLUGIN_DISCOVERY", value("--enable-plugin-discovery")?)),
            "--log-level" => cli.set.push(("LOG_LEVEL", value("--log-level")?)),
            "--cors-origins" => cli.set.push(("CORS_ORIGINS", value("--cors-origins")?)),
            "--mcp-auth-token" => cli.set.push(("NETBOX_MCP_TOKEN", value("--mcp-auth-token")?)),
            other => return Err(bad(format!("Unknown option \"{other}\". See --help."))),
        }
    }
    Ok(cli)
}

fn build_env(cli: &Cli) -> Env {
    let mut env = process_env();
    for (k, v) in &cli.set {
        env.insert((*k).to_string(), v.clone());
    }
    if let Ok(text) = std::fs::read_to_string(".env") {
        merge_dotenv(&mut env, &text);
    }
    env
}

async fn run() -> Result<(), NbError> {
    let cli = parse_cli()?;
    if cli.help {
        print!("{HELP}");
        return Ok(());
    }
    let env = build_env(&cli);
    let cfg = load_config(&env)?;
    nbmcp::log::set_level(&cfg.log_level);
    let (mcp_token, allowed_hosts, cors_origins, passthrough) = (cfg.mcp_token.clone(), cfg.allowed_hosts.clone(), cfg.cors_origins.clone(), cfg.passthrough);
    let registry = Registry::new(Runtime::new(cfg).await?);

    if cli.list_tools {
        for def in &registry.tools {
            println!("{}\t{}", def.access.as_str(), def.name);
        }
        return Ok(());
    }

    if cli.http {
        let port_text = cli.port.or_else(|| env.get("PORT").cloned()).unwrap_or_else(|| "8000".into());
        let port: u16 = port_text.parse().ok().filter(|p| *p >= 1).ok_or_else(|| NbError::new(ErrorType::ConfigError, format!("--port: \"{port_text}\" is not a valid port.")))?;
        let host = cli.host.or_else(|| env.get("HOST").cloned()).unwrap_or_else(|| "127.0.0.1".into());
        return http::serve(registry, HttpOptions { host, port, token: mcp_token, allowed_hosts, cors_origins, passthrough }).await;
    }

    serve_stdio(registry).await;
    Ok(())
}

async fn serve_stdio(registry: Arc<Registry>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = rx.recv().await {
            let mut line = msg.to_string();
            line.push('\n');
            if stdout.write_all(line.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });
    let session = Session::new(registry, Some(tx.clone()), None);
    nbmcp::log::info("ready on stdio");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            let _ = tx.send(serde_json::json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }));
            continue;
        };
        if msg.get("method").is_none() {
            // Réponse du client à une requête du serveur (elicitation).
            session.deliver_response(msg);
            continue;
        }
        let (session, tx) = (session.clone(), tx.clone());
        tokio::spawn(async move {
            if let Some(reply) = session.handle(msg).await {
                let _ = tx.send(reply);
            }
        });
    }
    drop(tx);
    drop(session);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("[nbmcp] {}{}", err.message, err.hint.as_ref().map(|h| format!("\n        {h}")).unwrap_or_default());
        std::process::exit(1);
    }
}
