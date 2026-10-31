//! Command line: `serve`, `token issue|list|revoke`, `oauth list|revoke`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::scope::Scopes;
use crate::store::rfc3339;

#[derive(Debug, Parser)]
#[command(
    name = "secretary-mcp",
    version,
    about = "Lucina MCP server (Streamable HTTP + OAuth 2.1)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Run the server (configuration from SECRETARY_MCP_* environment variables).
    Serve,
    /// Service tokens for Claude Code, Grok and scripts.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// OAuth grants (ChatGPT connectors).
    Oauth {
        #[command(subcommand)]
        cmd: OauthCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum TokenCmd {
    /// Issue a token; it is printed once, only its SHA-256 is stored.
    Issue {
        /// Caller name Lucina sees: claude-code, chatgpt, grok, … (a-z, 0-9, '-').
        #[arg(long)]
        name: String,
        /// Comma-separated scopes: read, chat, write, computer, agents, notebooks.
        #[arg(long, default_value = "read,chat")]
        scopes: String,
        /// Delegation depth: 0 for the owner's clients, 1 for a token handed to an
        /// agent run (it can use agents but not start any).
        #[arg(long, default_value_t = 0)]
        depth: u8,
    },
    /// List tokens (never the tokens themselves).
    List,
    /// Revoke by token id, or every token with this name.
    Revoke { id_or_name: String },
    /// Add scopes to an active token (by id, or every active token with this name),
    /// e.g. `add-scope grok computer`.
    AddScope { id_or_name: String, scopes: String },
}

#[derive(Debug, Subcommand)]
pub enum OauthCmd {
    /// List registered clients and active grants.
    List,
    /// Revoke a grant (by grant id or client id), or `all`.
    Revoke { target: String },
    /// Add scopes to a grant (by grant id or client id) and its live tokens,
    /// e.g. `add-scope grant_… computer`. The owner's decision, as on the consent page.
    AddScope { target: String, scopes: String },
}

fn data_dir() -> PathBuf {
    std::env::var("SECRETARY_MCP_DATA_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "/var/lib/secretary-mcp".into())
        .into()
}

/// Run a non-`serve` command; returns the process exit code.
pub fn run_admin(cmd: Cmd) -> anyhow::Result<i32> {
    let dir = data_dir();
    match cmd {
        Cmd::Serve => unreachable!("serve is handled by main"),
        Cmd::Token { cmd } => {
            let store = crate::tokens::TokenStore::new(&dir);
            match cmd {
                TokenCmd::Issue { name, scopes, depth } => {
                    let scopes = Scopes::parse_list(&scopes).map_err(anyhow::Error::msg)?;
                    let (rec, secret) = store.issue_at(&name, scopes, depth)?;
                    eprintln!(
                        "Issued {} for {:?} with scopes {}. It is shown only now:",
                        rec.id, rec.name, rec.scopes
                    );
                    println!("{secret}");
                    Ok(0)
                }
                TokenCmd::List => {
                    let list = store.list()?;
                    if list.is_empty() {
                        println!("no tokens");
                    }
                    for t in list {
                        println!(
                            "{}  {:<14} scopes={:<16}{} created={} last_used={} {}",
                            t.id,
                            t.name,
                            t.scopes.to_string(),
                            if t.depth > 0 { format!(" depth={}", t.depth) } else { String::new() },
                            rfc3339(t.created_at),
                            t.last_used_at
                                .map(rfc3339)
                                .unwrap_or_else(|| "never".into()),
                            if t.revoked { "REVOKED" } else { "active" }
                        );
                    }
                    Ok(0)
                }
                TokenCmd::AddScope { id_or_name, scopes } => {
                    let add = Scopes::parse_list(&scopes).map_err(anyhow::Error::msg)?;
                    let hit = store.add_scopes(&id_or_name, &add)?;
                    if hit.is_empty() {
                        eprintln!("no active token with id or name {id_or_name:?}");
                        return Ok(1);
                    }
                    for t in hit {
                        println!("{} ({}) scopes={}", t.id, t.name, t.scopes);
                    }
                    Ok(0)
                }
                TokenCmd::Revoke { id_or_name } => {
                    let hit = store.revoke(&id_or_name)?;
                    if hit.is_empty() {
                        eprintln!("no active token with id or name {id_or_name:?}");
                        return Ok(1);
                    }
                    for t in hit {
                        println!("revoked {} ({})", t.id, t.name);
                    }
                    Ok(0)
                }
            }
        }
        Cmd::Oauth { cmd } => {
            let cfg = crate::Config::new("https://localhost", &dir)?;
            let oauth = crate::oauth::OAuth::new(&cfg);
            match cmd {
                OauthCmd::List => {
                    let d = oauth.data()?;
                    println!("registered clients: {}", d.clients.len());
                    for c in &d.clients {
                        println!(
                            "  {}  {:<20} caller={} auth={} created={}",
                            c.client_id,
                            c.client_name,
                            c.caller,
                            c.auth_method,
                            rfc3339(c.created_at)
                        );
                    }
                    let grants: Vec<_> = d.grants.iter().filter(|g| !g.revoked).collect();
                    println!("active grants: {}", grants.len());
                    for g in grants {
                        println!(
                            "  {}  {:<20} caller={} scopes={} created={} client={}",
                            g.id,
                            g.client_name,
                            g.caller,
                            g.scopes,
                            rfc3339(g.created_at),
                            g.client_id
                        );
                    }
                    Ok(0)
                }
                OauthCmd::AddScope { target, scopes } => {
                    let add = Scopes::parse_list(&scopes).map_err(anyhow::Error::msg)?;
                    let n = oauth.add_scopes(&target, &add)?;
                    println!("updated {n} grant(s)");
                    Ok(if n == 0 { 1 } else { 0 })
                }
                OauthCmd::Revoke { target } => {
                    let n = oauth.revoke(&target)?;
                    println!("revoked {n} grant(s)");
                    Ok(if n == 0 { 1 } else { 0 })
                }
            }
        }
    }
}
