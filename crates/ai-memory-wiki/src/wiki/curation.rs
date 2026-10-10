//! Exact sentinel cleanup through the common admission and atomic persistence path.
use super::*;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// A conditional, metadata-only relation cleanup. No replacement body is accepted.
pub struct RelationCurationRequest {
    /// Existing workspace.
    pub workspace_id: WorkspaceId,
    /// Existing project.
    pub project_id: ProjectId,
    /// Existing page path.
    pub path: PagePath,
    /// Expected latest version, scoped to the page.
    pub expected_head_id: PageId,
    /// Expected body hash, independently checked on disk and in the writer.
    pub expected_body_sha256: [u8; 32],
    /// Validate without persisting.
    pub dry_run: bool,
    /// Authenticated operator passed to admission.
    pub actor: ActorContext,
}

/// Bounded receipt. It contains no body, relation target, or title.
#[derive(Serialize)]
pub struct RelationCurationResult {
    /// Current or newly written page id.
    pub page_id: PageId,
    /// Number of exact sentinels removed from relation arrays.
    pub removed: usize,
    /// Whether a new version was persisted.
    pub applied: bool,
    /// Recoverable checkpoint before changing the file.
    pub before_checkpoint: Option<String>,
    /// Checkpoint containing the curated file.
    pub checkpoint: Option<String>,
}

impl Wiki {
    /// Remove exact sentinels from relation arrays while preserving all other data.
    /// Serializes with writes and watcher reindexes, repeats the CAS in the writer,
    /// and refuses admission/sanitizer changes to protected content.
    ///
    /// # Errors
    /// Returns an error on stale state, disk/index skew, admission rejection,
    /// protected changes, or persistence/checkpoint failure.
    pub async fn curate_relations(
        &self,
        req: RelationCurationRequest,
    ) -> WikiResult<RelationCurationResult> {
        req.path.ensure_portable()?;
        let _guard = self.mutation_lock.write().await;
        self.ensure_project_scope_rows(req.workspace_id, req.project_id)
            .await?;
        let reader = self
            .store_reader
            .as_ref()
            .ok_or_else(|| ai_memory_wiki_error("curation requires a store reader"))?;
        let latest = reader
            .latest_page_id_by_ids(req.workspace_id, req.project_id, req.path.to_string())
            .await?;
        if latest != Some(req.expected_head_id) {
            return Err(WikiError::StalePageHead);
        }
        let stored = reader
            .page_body_by_ids(req.workspace_id, req.project_id, req.path.as_str())
            .await?
            .ok_or(WikiError::StalePageHead)?;
        let mut markdown = self.read_page(req.workspace_id, req.project_id, &req.path)?;
        let stored_fm: serde_json::Value = serde_json::from_str(&stored.frontmatter_json)?;
        if markdown.body != stored.body
            || markdown.frontmatter != stored_fm
            || <[u8; 32]>::from(Sha256::digest(markdown.body.as_bytes()))
                != req.expected_body_sha256
        {
            return Err(WikiError::StalePageHead);
        }
        let original = markdown.clone();
        drop(_guard);
        // Bare markers are unconditional absence. The `.md` sentinel forms
        // (`none.md`, `nenhuma.md`, `n/a.md`) can name a live page in this
        // scope, so resolve their existence once before deciding removal.
        let mut md_candidates: Vec<String> = Vec::new();
        if let Some(relations) = markdown
            .frontmatter
            .get("relations")
            .and_then(serde_json::Value::as_object)
        {
            for key in ["causes", "fixes", "contradicts", "references"] {
                if let Some(items) = relations.get(key).and_then(serde_json::Value::as_array) {
                    for v in items {
                        if let Some(s) = v.as_str() {
                            let s = s.trim();
                            if ai_memory_core::relation_target_is_sentinel_path(s)
                                && !md_candidates.iter().any(|c| c == s)
                            {
                                md_candidates.push(s.to_string());
                            }
                        }
                    }
                }
            }
        }
        let mut live_md_sentinels: Vec<String> = Vec::new();
        for target in &md_candidates {
            if reader
                .latest_page_id_by_ids(req.workspace_id, req.project_id, target.clone())
                .await?
                .is_some()
            {
                live_md_sentinels.push(target.clone());
            }
        }
        let mut removed = 0;
        if let Some(relations) = markdown
            .frontmatter
            .get_mut("relations")
            .and_then(serde_json::Value::as_object_mut)
        {
            for key in ["causes", "fixes", "contradicts", "references"] {
                if let Some(items) = relations
                    .get_mut(key)
                    .and_then(serde_json::Value::as_array_mut)
                {
                    let old = items.len();
                    items.retain(|v| {
                        let Some(s) = v.as_str() else {
                            return true;
                        };
                        if ai_memory_core::relation_target_is_bare_sentinel(s) {
                            return false;
                        }
                        let s = s.trim();
                        if ai_memory_core::relation_target_is_sentinel_path(s)
                            && !live_md_sentinels.iter().any(|live| live.as_str() == s)
                        {
                            return false;
                        }
                        true
                    });
                    removed += old - items.len();
                }
            }
        }
        let protected = markdown.clone();
        // Refuse imported/hand-edited secrets before any external admission
        // callback sees the stored document; cleanup must not rewrite its body.
        markdown.body = self.sanitizer.scrub(&markdown.body);
        scrub_frontmatter_strings(&mut markdown.frontmatter, &self.sanitizer);
        if markdown != protected {
            return Err(WikiError::ProtectedPageContent);
        }
        let mut ctx = AdmissionContext {
            op: AdmissionOp::WritePage,
            actor: req.actor,
            ..Default::default()
        };
        self.resolve_admission_names(req.workspace_id, req.project_id, &mut ctx)
            .await;
        if let Some(chain) = &self.admission_chain {
            chain.run(&req.path, &mut markdown, &ctx).await?;
        }
        markdown.body = self.sanitizer.scrub(&markdown.body);
        scrub_frontmatter_strings(&mut markdown.frontmatter, &self.sanitizer);
        if markdown != protected {
            return Err(WikiError::ProtectedPageContent);
        }
        // Admission may call back into the normal write path. Hold no wiki lock
        // across it, then repeat the comparison before any checkpoint or write.
        let _guard = self.mutation_lock.write().await;
        if reader
            .latest_page_id_by_ids(req.workspace_id, req.project_id, req.path.to_string())
            .await?
            != Some(req.expected_head_id)
            || self.read_page(req.workspace_id, req.project_id, &req.path)? != original
        {
            return Err(WikiError::StalePageHead);
        }
        let mut receipt = RelationCurationResult {
            page_id: req.expected_head_id,
            removed,
            applied: false,
            before_checkpoint: None,
            checkpoint: None,
        };
        if req.dry_run || removed == 0 {
            return Ok(receipt);
        }
        let before = self
            .commit_all("curation: checkpoint before exact relation cleanup")?
            .map(|id| id.to_string())
            .or_else(|| {
                self.recent_checkpoints(1)
                    .ok()
                    .and_then(|v| v.first().map(|c| c.oid.clone()))
            })
            .ok_or_else(|| ai_memory_wiki_error("curation requires a recoverable checkpoint"))?;
        receipt.before_checkpoint = Some(before);
        let emitted = emit(&markdown)?;
        receipt.page_id = self
            .persist_page_locked(
                NewPage {
                    workspace_id: req.workspace_id,
                    project_id: req.project_id,
                    path: req.path.clone(),
                    title: stored.title,
                    body: markdown.body.clone(),
                    tier: stored.tier.parse()?,
                    pinned: stored.pinned,
                    links: crate::markdown::extract_all_links(
                        &markdown.frontmatter,
                        &markdown.body,
                        &req.path,
                    ),
                    expires_at: parse_expires_at(&req.path, &markdown.frontmatter)?,
                    entities: parse_entities(&req.path, &markdown.frontmatter)?,
                    frontmatter_json: markdown.frontmatter.clone(),
                    author_id: None,
                    evidence: Vec::new(),
                },
                emitted.as_bytes(),
                Some((req.expected_head_id, req.expected_body_sha256)),
                None,
            )
            .await?;
        receipt.checkpoint = self
            .commit_all("curation: removed exact relation sentinels")?
            .map(|id| id.to_string());
        if receipt.checkpoint.is_none() {
            return Err(ai_memory_wiki_error(
                "curation changed a page without a checkpoint",
            ));
        }
        receipt.applied = true;
        drop(_guard);
        if let Some(chain) = &self.admission_chain {
            chain.dispatch_async(
                Some(req.path.as_str()),
                &markdown.frontmatter,
                &markdown.body,
                &ctx,
            );
        }
        Ok(receipt)
    }
}
