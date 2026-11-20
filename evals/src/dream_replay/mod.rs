//! Paired offline replay of the literal Dream pass.

mod environment;
mod report;
mod scoring;
mod verify;

use std::path::PathBuf;

use ai_memory_consolidate::dream::DreamConfig;
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use environment::{Environment, SEED_TIMESTAMP};
use report::{Receipt, TaskReceipt};
use scoring::Task;

#[cfg(test)]
mod tests;

const FIXTURE: &str = include_str!("../../fixtures/dream-replay/v1.json");

#[derive(Args, Debug)]
pub struct DreamReplayArgs {
    /// Output directory for receipt.json and receipt.md.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Deserialize)]
struct Fixture {
    id: String,
    merge_sources: [String; 2],
    pages: Vec<SeedPage>,
    tasks: Vec<Task>,
}

#[derive(Deserialize)]
struct SeedPage {
    path: String,
    title: String,
    author: String,
    body: String,
}

pub async fn run(args: DreamReplayArgs) -> Result<()> {
    let fixture: Fixture = serde_json::from_str(FIXTURE).context("parse Dream replay fixture")?;
    for task in &fixture.tasks {
        ensure!(
            !task.query.is_empty() && !task.requires.is_empty(),
            "empty task {}",
            task.id
        );
        for required in &task.requires {
            ensure!(
                !required.literal.is_empty()
                    && fixture.pages.iter().any(|page| {
                        page.path == required.source && page.body.contains(&required.literal)
                    }),
                "task {} has an unsupported fixture requirement: {required:?}",
                task.id
            );
        }
    }

    let scratch = tempfile::Builder::new().prefix("dream-replay-").tempdir()?;
    let (baseline, candidate, embedding) =
        Environment::pair(&scratch.path().canonicalize()?, &fixture).await?;
    let cfg = DreamConfig {
        enabled: true,
        embedding: Some(embedding),
        ..DreamConfig::default()
    };
    let receipt = replay(fixture, &baseline, &candidate, &cfg).await;
    baseline.store.close().await;
    candidate.store.close().await;
    let receipt = receipt?;
    scratch.close()?;
    receipt.write(&args.out)?;
    println!("Dream replay receipts written to {}", args.out.display());
    Ok(())
}

async fn replay(
    fixture: Fixture,
    baseline: &Environment,
    candidate: &Environment,
    cfg: &DreamConfig,
) -> Result<Receipt> {
    let original = baseline.snapshot().await?;
    verify::seeded(&fixture, &original)?;
    let same_input = original == candidate.snapshot().await?;
    ensure!(
        same_input,
        "baseline and candidate started with different input"
    );

    let plan = candidate
        .dream(cfg, true)
        .await
        .context("plan literal Dream")?;
    let planned = verify::single_merge(&fixture, &original, &plan, true)?;
    let plan_read_only = original == candidate.snapshot().await?;
    ensure!(plan_read_only, "Dream plan mutated the candidate");
    let baseline_tasks =
        scoring::run_queries(baseline, &fixture.tasks, &original, None, "baseline").await?;

    let apply = candidate
        .dream(cfg, false)
        .await
        .context("apply literal Dream")?;
    let merged = verify::single_merge(&fixture, &original, &apply, false)?;
    ensure!(
        planned.digest == merged.digest,
        "apply differed from the inspected plan"
    );
    let applied = candidate.snapshot().await?;
    verify::applied(candidate, &original, &applied, merged).await?;
    let candidate_tasks = scoring::run_queries(
        candidate,
        &fixture.tasks,
        &original,
        Some(merged),
        "applied",
    )
    .await?;
    let control_task = fixture.tasks.first().context("missing loss-control task")?;
    let controls =
        scoring::negative_controls(control_task, &candidate_tasks[0], &original, merged)?;

    let restore = verify::restore(candidate, &original, &applied, merged).await?;
    let restored_tasks =
        scoring::run_queries(candidate, &fixture.tasks, &original, None, "restored").await?;
    let baseline_unchanged = original == baseline.snapshot().await?;
    ensure!(baseline_unchanged, "the baseline changed during the replay");

    let mut tasks = Vec::with_capacity(fixture.tasks.len());
    for (((task, baseline), candidate), restored) in fixture
        .tasks
        .into_iter()
        .zip(baseline_tasks)
        .zip(candidate_tasks)
        .zip(restored_tasks)
    {
        ensure!(
            baseline.receipt.context == restored.receipt.context,
            "restored context volume differs from baseline for {}",
            task.id
        );
        tasks.push(TaskReceipt {
            id: task.id,
            query: task.query,
            baseline: baseline.receipt,
            candidate: candidate.receipt,
            restored: restored.receipt,
        });
    }
    Ok(Receipt {
        fixture: fixture.id,
        fixture_sha256: sha256(FIXTURE),
        source_timestamp: SEED_TIMESTAMP,
        top_k: scoring::TOP_K,
        same_input,
        plan_read_only,
        baseline_unchanged,
        plan,
        apply,
        restore,
        tasks,
        controls,
    })
}

fn sha256(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(data))
}
