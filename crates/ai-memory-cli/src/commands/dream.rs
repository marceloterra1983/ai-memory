//! `ai-memory dream` — a thin client using the server's existing writer.

use anyhow::Result;
use serde::Serialize;

use crate::cli::DreamArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

#[derive(Serialize)]
struct DreamRequest {
    workspace: String,
    project: String,
    apply: bool,
}

/// Report IDs, digests and preservation metadata from one scoped run.
///
/// # Errors
/// Fails when the root-authenticated server refuses the scope or is unavailable.
pub async fn run(config: &Config, args: DreamArgs) -> Result<()> {
    if args.workspace.trim().is_empty() || args.project.trim().is_empty() {
        anyhow::bail!("DREAM requires explicit nonempty --workspace and --project");
    }
    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    let report: serde_json::Value = post_json(
        &endpoint,
        "/admin/dream",
        &DreamRequest {
            workspace: args.workspace,
            project: args.project,
            apply: args.apply,
        },
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Command};
    use clap::Parser;

    #[test]
    fn dream_defaults_to_dry_run_and_requires_explicit_scope() {
        for arguments in [
            vec!["ai-memory", "dream"],
            vec!["ai-memory", "dream", "--workspace", "fixture"],
            vec!["ai-memory", "dream", "--project", "app"],
        ] {
            assert!(Cli::try_parse_from(arguments).is_err());
        }
        let cli = Cli::try_parse_from([
            "ai-memory",
            "dream",
            "--workspace",
            "fixture",
            "--project",
            "app",
        ])
        .unwrap();
        let Command::Dream(args) = cli.command else {
            panic!("expected dream");
        };
        assert!(!args.apply);
        assert!(
            Cli::try_parse_from([
                "ai-memory",
                "dream",
                "--workspace",
                "fixture",
                "--project",
                "app",
                "--apply",
                "--dry-run"
            ])
            .is_err()
        );
    }
}
