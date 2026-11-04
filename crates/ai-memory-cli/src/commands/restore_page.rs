//! `ai-memory restore-page` — restore one wiki page from a git checkpoint.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::show::terminal_text;
use crate::cli::RestorePageArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

#[derive(Serialize)]
struct RestorePageRequest {
    workspace: String,
    project: String,
    path: String,
    rev: String,
}

#[derive(Deserialize, Serialize)]
struct RestorePageResponse {
    page_id: String,
    path: String,
    restored_from: String,
    #[serde(default)]
    pre_checkpoint: Option<String>,
    #[serde(default)]
    checkpoint: Option<String>,
}

/// Run the `restore-page` subcommand.
///
/// # Errors
/// Returns an error if the server is unreachable, the checkpoint/path cannot be
/// restored, or the restored page cannot be reindexed.
pub async fn run(config: &Config, args: RestorePageArgs) -> Result<()> {
    let (workspace, project) =
        super::resolve_scope(config, args.workspace.as_deref(), args.project.as_deref())?;
    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    let resp: RestorePageResponse = post_json(
        &endpoint,
        "/admin/restore-page",
        &RestorePageRequest {
            workspace: workspace.clone(),
            project: project.clone(),
            path: args.path.clone(),
            rev: args.from.clone(),
        },
    )
    .await
    .context("restoring page via server")?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    println!(
        "restored {} under {}/{} from {} (page_id={})",
        terminal_text(&resp.path),
        workspace,
        project,
        terminal_text(&resp.restored_from),
        terminal_text(&resp.page_id.chars().take(8).collect::<String>())
    );
    if let Some(pre) = resp.pre_checkpoint {
        println!("pre-restore checkpoint: {}", terminal_text(&pre));
    }
    if let Some(cp) = resp.checkpoint {
        println!("restore checkpoint: {}", terminal_text(&cp));
    }
    Ok(())
}
