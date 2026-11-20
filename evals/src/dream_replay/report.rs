use std::fmt::Write;
use std::fs;
use std::path::Path;

use ai_memory_consolidate::dream::DreamReport;
use anyhow::{Context, Result};
use serde::Serialize;

use super::{scoring, verify};

#[derive(Serialize)]
pub(super) struct Receipt {
    pub fixture: String,
    pub fixture_sha256: String,
    pub source_timestamp: &'static str,
    pub top_k: usize,
    pub same_input: bool,
    pub plan_read_only: bool,
    pub baseline_unchanged: bool,
    pub plan: DreamReport,
    pub apply: DreamReport,
    pub restore: verify::RestoreReceipt,
    pub tasks: Vec<TaskReceipt>,
    pub controls: scoring::Controls,
}

#[derive(Serialize)]
pub(super) struct TaskReceipt {
    pub id: String,
    pub query: String,
    pub baseline: scoring::PhaseReceipt,
    pub candidate: scoring::PhaseReceipt,
    pub restored: scoring::PhaseReceipt,
}

impl Receipt {
    pub fn write(&self, dir: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        let markdown = self.markdown()?;
        fs::create_dir_all(dir)
            .with_context(|| format!("create receipt directory {}", dir.display()))?;
        fs::write(dir.join("receipt.json"), format!("{json}\n"))?;
        fs::write(dir.join("receipt.md"), markdown)?;
        Ok(())
    }

    fn markdown(&self) -> Result<String> {
        let mut out = String::new();
        writeln!(out, "# Literal Dream replay\n")?;
        writeln!(
            out,
            "Fixture `{}` at SHA-256 `{}`. Both arms started from the same snapshot dated `{}`.\n",
            self.fixture, self.fixture_sha256, self.source_timestamp
        )?;
        writeln!(
            out,
            "The baseline and the candidate dry run preserved their original heads, file bytes, history and checkpoints. The candidate applied {} merge with {} inference requests.\n",
            self.apply.clusters_merged, self.apply.inference_requests
        )?;
        for merge in &self.plan.merges {
            writeln!(
                out,
                "The `{}` plan selected `{}` with `{}` and source digest `{}`.\n",
                merge.reason,
                merge.survivor,
                merge.merged.join(", "),
                merge.digest
            )?;
        }
        writeln!(out, "## Retrieval and context\n")?;
        writeln!(
            out,
            "Each query uses the same top-{} scoped FTS search. Context counts complete retrieved bodies. Estimated tokens round Unicode characters divided by four up per query.\n",
            self.top_k
        )?;
        writeln!(
            out,
            "| Task | Phase | Supported | Bodies | UTF-8 bytes | Est. tokens | Retrieved paths |"
        )?;
        writeln!(out, "|---|---|---:|---:|---:|---:|---|")?;
        for task in &self.tasks {
            for (phase, result) in [
                ("Baseline", &task.baseline),
                ("Applied", &task.candidate),
                ("Restored", &task.restored),
            ] {
                let paths: Vec<_> = result
                    .hits
                    .iter()
                    .map(|hit| format!("`{}`", hit.path))
                    .collect();
                writeln!(
                    out,
                    "| {} | {phase} | {}/{} | {} | {} | {} | {} |",
                    task.id,
                    result.evidence.len(),
                    result.evidence.len() + result.missing.len(),
                    result.context.pages,
                    result.context.utf8_bytes,
                    result.context.estimated_tokens,
                    paths.join(", ")
                )?;
            }
        }
        writeln!(out, "\n## Restore\n")?;
        writeln!(
            out,
            "Both source files resolved at `{}`. Restore recovered the original bytes, including the author metadata. The original authors remain in version history. The applied heads were checkpointed at `{}` and the restored heads at `{}`.\n",
            self.restore.from, self.restore.before_restore, self.restore.after_restore
        )?;
        writeln!(
            out,
            "| Source | Original author | Original ID | Applied ID | Restored ID |"
        )?;
        writeln!(out, "|---|---|---|---|---|")?;
        for source in &self.restore.sources {
            writeln!(
                out,
                "| `{}` | `{}` | `{}` | `{}` | `{}` |",
                source.path,
                source.author,
                source.original_id,
                source.applied_id,
                source.restored_id
            )?;
        }
        writeln!(out, "\n## Loss controls\n")?;
        writeln!(
            out,
            "On `{}`, removing one literal from the retrieved body left {} requirement unsupported. Removing one source record left {} requirements unsupported. The controls ran on copies of the retrieved context.\n",
            self.controls.task,
            self.controls.lost_literal.len(),
            self.controls.lost_source.len()
        )?;
        writeln!(
            out,
            "The JSON receipt contains the native Dream reports and the source version supporting each literal."
        )?;
        Ok(out)
    }
}
