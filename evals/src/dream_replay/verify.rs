use ai_memory_consolidate::dream::{DreamMerge, DreamReport};
use ai_memory_core::{OwnerFilter, PageId, PagePath};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::json;

use super::environment::{Environment, Head, Snapshot};
use super::{Fixture, sha256};

#[derive(Serialize)]
pub(super) struct RestoreReceipt {
    pub from: String,
    pub before_restore: String,
    pub after_restore: String,
    pub sources: Vec<RestoredSource>,
}

#[derive(Serialize)]
pub(super) struct RestoredSource {
    pub path: String,
    pub author: String,
    pub original_id: PageId,
    pub applied_id: PageId,
    pub restored_id: PageId,
    pub file_sha256: String,
}

pub(super) fn seeded(fixture: &Fixture, original: &Snapshot) -> Result<()> {
    ensure!(
        original.heads.len() == fixture.pages.len(),
        "incorrect seeded page count"
    );
    for page in &fixture.pages {
        let head = original
            .heads
            .get(&page.path)
            .with_context(|| format!("missing seed {}", page.path))?;
        ensure!(
            head.markdown.body == page.body
                && head.history.len() == 1
                && head.history[0].id == head.id
                && head.history[0].supersedes.is_none()
                && head.history[0].author.as_deref() == Some(page.author.as_str())
                && head.history[0].is_latest
                && head.markdown.frontmatter["last_modified_by"]["username"] == page.author,
            "seed body or authorship differs for {}",
            page.path,
        );
    }
    Ok(())
}

pub(super) fn single_merge<'a>(
    fixture: &Fixture,
    original: &Snapshot,
    report: &'a DreamReport,
    dry_run: bool,
) -> Result<&'a DreamMerge> {
    let writes = usize::from(!dry_run);
    ensure!(
        report.dry_run == dry_run
            && !report.disabled
            && !report.cancelled
            && report.cold_pages == original.heads.len()
            && report.clusters_considered == 1
            && report.clusters_selected == 1
            && report.skipped == 0
            && report.clusters_merged == writes
            && report.pages_rewritten == writes
            && report.pages_superseded == writes
            && report.provider == "none"
            && report.inference_requests == 0
            && report.semantic_questions == 0
            && report.http_requests.is_none_or(|requests| requests == 0),
        "unexpected Dream report: {report:?}",
    );
    let [merge] = report.merges.as_slice() else {
        anyhow::bail!("expected one Dream merge record: {report:?}");
    };
    ensure!(
        merge.applied != dry_run
            && merge.checkpoint.is_none() == dry_run
            && merge.new_survivor_id.is_none() == dry_run
            && (!dry_run || merge.reason == "planned_literal"),
        "unexpected Dream outcome: {merge:?}"
    );
    let [survivor, loser] = &fixture.merge_sources;
    ensure!(
        merge.survivor == *survivor
            && merge.merged.iter().eq([loser])
            && merge
                .sources
                .iter()
                .map(|source| &source.path)
                .eq([survivor, loser]),
        "unexpected Dream sources: {merge:?}"
    );
    for source in &merge.sources {
        let head = &original.heads[&source.path];
        ensure!(
            source.id == head.id
                && source.updated_at_us == head.updated_at_us
                && source.body_sha256 == sha256(&head.markdown.body)
                && source.file_sha256 == head.file_sha256
                && source.eligibility_sha256 == head.eligibility_sha256,
            "Dream source snapshot disagrees at {}",
            source.path
        );
    }
    ensure!(
        merge.survivor_id == original.heads[survivor].id,
        "Dream reported the wrong original survivor"
    );
    Ok(merge)
}

pub(super) async fn applied(
    env: &Environment,
    original: &Snapshot,
    after: &Snapshot,
    merge: &DreamMerge,
) -> Result<()> {
    let survivor = after
        .heads
        .get(&merge.survivor)
        .context("missing merged survivor")?;
    let [loser_path] = merge.merged.as_slice() else {
        anyhow::bail!("expected one merged-away source");
    };
    let loser = after.heads.get(loser_path).context("missing merge stub")?;
    let complete_body = format!(
        "{}\n\n{}",
        original.heads[&merge.survivor].markdown.body, original.heads[loser_path].markdown.body
    );
    ensure!(
        merge.new_survivor_id == Some(survivor.id) && survivor.markdown.body == complete_body,
        "applied survivor is not the complete literal copy"
    );
    ensure!(
        loser.markdown.frontmatter["merged_into"] == merge.survivor
            && loser.markdown.frontmatter["compacted"] == true
            && loser
                .markdown
                .body
                .contains(&format!("[[{}]]", merge.survivor)),
        "loser did not become a linked merge stub"
    );
    ensure!(
        merge.embedding_required_ids == [survivor.id, loser.id],
        "receipt did not identify both new heads"
    );
    unchanged_peers(original, after, merge)?;

    for source in &merge.sources {
        let head = &after.heads[&source.path];
        history(env, &source.path, head, &original.heads[&source.path], &[]).await?;
        let attribution = env
            .store
            .reader
            .page_provenance(
                env.ws,
                env.project,
                source.path.clone(),
                None,
                OwnerFilter::Any,
                0,
            )
            .await?
            .context("missing reconsolidation evidence")?;
        ensure!(
            attribution.page_id == head.id
                && attribution.source_count == merge.sources.len()
                && attribution
                    .sources
                    .iter()
                    .all(|source| source.kind == "reconsolidation"),
            "incorrect reconsolidation evidence at {}",
            source.path
        );
    }
    ensure!(
        survivor.markdown.frontmatter["dream_sources"] == json!(merge.sources)
            && survivor.markdown.frontmatter["merged_from"]
                == json!(
                    merge
                        .sources
                        .iter()
                        .map(|source| &source.path)
                        .collect::<Vec<_>>()
                ),
        "merged frontmatter lost source attribution"
    );
    Ok(())
}

pub(super) async fn restore(
    env: &Environment,
    original: &Snapshot,
    applied: &Snapshot,
    merge: &DreamMerge,
) -> Result<RestoreReceipt> {
    let from = merge
        .checkpoint
        .as_ref()
        .context("apply omitted the premerge checkpoint")?;
    ensure!(
        applied.checkpoints.contains(from),
        "premerge checkpoint missing from wiki history"
    );
    for source in &merge.sources {
        let path = PagePath::new(&source.path)?;
        let absolute = env.wiki.abs_path(env.ws, env.project, &path);
        let file = env
            .wiki
            .git()
            .file_at_rev(from, absolute.strip_prefix(env.wiki.root())?)?;
        ensure!(
            sha256(file) == original.heads[&source.path].file_sha256,
            "wrong checkpoint bytes for {path}"
        );
    }

    let before_restore = env
        .wiki
        .commit_all("dream-replay: retain the applied heads before restore")?
        .context("missing checkpoint of the applied heads")?
        .to_string();
    let mut restored_ids = Vec::with_capacity(merge.sources.len());
    for source in &merge.sources {
        let id = env
            .wiki
            .restore_page_from_checkpoint(env.ws, env.project, PagePath::new(&source.path)?, from)
            .await?;
        restored_ids.push(id);
    }
    let after_restore = env
        .wiki
        .commit_all("dream-replay: restore both Dream sources")?
        .context("missing checkpoint of the restored heads")?
        .to_string();

    let restored = env.snapshot().await?;
    unchanged_peers(original, &restored, merge)?;
    let mut sources = Vec::with_capacity(merge.sources.len());
    for (source, restored_id) in merge.sources.iter().zip(restored_ids) {
        let path = &source.path;
        let head = restored
            .heads
            .get(path)
            .with_context(|| format!("missing restored {path}"))?;
        let prior = &original.heads[path];
        let applied_id = applied.heads[path].id;
        ensure!(
            head.id == restored_id
                && head.markdown == prior.markdown
                && head.file_sha256 == prior.file_sha256,
            "restore did not recover the original file and authorship for {path}"
        );
        history(env, path, head, prior, &[applied_id]).await?;
        sources.push(RestoredSource {
            path: path.clone(),
            author: prior.history[0]
                .author
                .clone()
                .context("missing original author")?,
            original_id: prior.id,
            applied_id,
            restored_id,
            file_sha256: head.file_sha256.clone(),
        });
    }
    Ok(RestoreReceipt {
        from: from.clone(),
        before_restore,
        after_restore,
        sources,
    })
}

fn unchanged_peers(original: &Snapshot, after: &Snapshot, merge: &DreamMerge) -> Result<()> {
    ensure!(
        original.heads.len() == after.heads.len(),
        "Dream scope gained or lost a head"
    );
    for (path, head) in &original.heads {
        if !merge.sources.iter().any(|source| &source.path == path) {
            ensure!(
                after.heads.get(path) == Some(head),
                "unrelated fixture page changed at {path}"
            );
        }
    }
    Ok(())
}

async fn history(
    env: &Environment,
    path: &str,
    head: &Head,
    original: &Head,
    middle: &[PageId],
) -> Result<()> {
    let ids: Vec<_> = std::iter::once(head.id)
        .chain(middle.iter().copied())
        .chain([original.id])
        .collect();
    ensure!(
        head.history.len() == ids.len(),
        "wrong supersession depth at {path}"
    );
    for (index, version) in head.history.iter().enumerate() {
        ensure!(
            version.id == ids[index]
                && version.supersedes == ids.get(index + 1).copied()
                && version.is_latest == (index == 0),
            "broken supersession chain at {path}"
        );
    }
    ensure!(
        head.history
            .last()
            .and_then(|version| version.author.as_ref())
            == original.history[0].author.as_ref(),
        "original author missing from version history at {path}"
    );
    let old = env.version(path, original.id).await?;
    let strip = ai_memory_core::okf::strip_generated_at;
    ensure!(
        old.body == original.markdown.body
            && strip(&old.frontmatter) == strip(&original.markdown.frontmatter),
        "original version content missing from history at {path}"
    );
    Ok(())
}
