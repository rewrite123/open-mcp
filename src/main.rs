use anyhow::Result;
use clap::{Parser, Subcommand};
use omcp::config;
use omcp::meta::Meta;

/// omcp (openmcp) - a CLI acting as an MCP client, bridging AI models
/// (e.g. Ollama) with Model Context Protocol servers.
#[derive(Parser)]
#[command(name = "omcp", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List MCP server entries configured in ~/.mcp/config
    List,
    /// List the tools exposed by a configured MCP server
    Tools {
        /// Name of the entry in ~/.mcp/config
        name: String,
    },
    /// Start a chat session bridging a model and an MCP server. If MESSAGE is
    /// given, sends that single message and exits instead of starting an
    /// interactive session.
    Chat {
        /// Name of the entry in ~/.mcp/config
        name: String,
        /// A single message to send non-interactively; prints the answer and exits
        message: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List => cmd_list(),
        Command::Tools { name } => cmd_tools(&name).await,
        Command::Chat { name, message } => cmd_chat(&name, message.as_deref()).await,
    }
}

fn cmd_list() -> Result<()> {
    let entries = config::load_all()?;
    if entries.is_empty() {
        println!("No entries found in {}", config::config_path()?.display());
        return Ok(());
    }
    for entry in entries {
        println!("{}\n  mcp:   {}\n  model: {} @ {}", entry.name, entry.mcp, entry.model, entry.model_host);
    }
    Ok(())
}

fn load_meta(entry: &config::ServerConfig) -> Result<Meta> {
    match &entry.meta {
        Some(path) => Meta::load(path),
        None => Ok(Meta::default()),
    }
}

async fn cmd_tools(name: &str) -> Result<()> {
    let entry = config::load(name)?;
    let meta = load_meta(&entry)?;
    let client = omcp::connect_endpoint(&entry.mcp_endpoint, &meta).await?;
    let tools = client.list_tools().await?;
    if tools.is_empty() {
        println!("Server '{}' exposes no tools.", entry.name);
        return Ok(());
    }
    for tool in tools {
        println!("{} - {}", tool.name, tool.description.unwrap_or_default());
    }
    Ok(())
}

async fn cmd_chat(name: &str, message: Option<&str>) -> Result<()> {
    let entry = config::load(name)?;
    let meta = load_meta(&entry)?;
    let allowed_tools = meta.allowed_tools.clone();
    omcp::run_chat_hosts(
        entry.mcp_endpoints,
        entry.model_endpoint,
        &entry.model,
        meta,
        allowed_tools.as_deref(),
        entry.messages,
        message,
    ).await
}
