//! Snapshot and CAS extensions for bounded local dream operations.

use super::*;

/// The complete bounded source supplied to dream inference, protected at apply.
#[derive(Debug, Clone)]
pub struct DreamSource {
    /// Store version and every eligibility input observed for this source.
    pub head: ai_memory_store::DreamHead,
    /// Complete parsed source; never truncated.
    pub markdown: Markdown,
    /// SHA-256 of the exact source file, including frontmatter.
    pub file_sha256: [u8; 32],
}

/// A successful atomic dream apply and its recoverable pre-merge checkpoint.
#[derive(Debug, Clone)]
pub struct DreamApplyResult {
    /// The new heads, in request order.
    pub ids: Vec<PageId>,
    /// Full git commit OID retaining all source files before this merge.
    pub checkpoint: String,
}

pub(super) struct DreamBatchGuard {
    pub(super) sources: Vec<DreamSource>,
    pub(super) cancel: Arc<dyn Fn() -> bool + Send + Sync>,
}

pub(super) fn dream_refused(reason: &str) -> WikiError {
    ai_memory_store::StoreError::InvalidState(reason.into()).into()
}

pub(super) fn read_dream_file(abs: &Path, max_bytes: usize) -> WikiResult<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(abs)?;
    if file.metadata()?.len() > max_bytes as u64 {
        return Err(dream_refused(
            "dream source byte cap exceeded; source refused without truncation",
        ));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(dream_refused(
            "dream source byte cap exceeded; source refused without truncation",
        ));
    }
    Ok(bytes)
}

impl Wiki {
    /// Preflight dream admission with literal loopback destinations and a
    /// client that never follows redirects, proxies, retries, or external DNS.
    /// Both consolidate and ordinary page subscribers are validated because
    /// a merge contains both survivor rewrites and loser stubs.
    ///
    /// # Errors
    /// Refuses nonlocal endpoints, port 8789, and any failed local admission.
    pub async fn preflight_dream_admission(
        &self,
        workspace_id: WorkspaceId,
        project_id: ProjectId,
        path: &PagePath,
        actor: ActorContext,
        cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> WikiResult<()> {
        if cancel() {
            return Err(dream_refused("dream cancelled before admission"));
        }
        let mut local = self.dream_local_clone()?;
        if let Some(chain) = &mut local.admission_chain {
            chain.set_dream_cancel(cancel);
        }
        local
            .preflight_admission(
                workspace_id,
                project_id,
                path,
                AdmissionOp::Consolidate,
                actor,
            )
            .await
    }

    fn dream_local_clone(&self) -> WikiResult<Self> {
        let mut local = self.clone();
        local.admission_chain = self
            .admission_chain
            .as_ref()
            .map(AdmissionChain::dream_local)
            .transpose()?;
        Ok(local)
    }

    /// Snapshot complete, bounded files that match the selected indexed heads.
    ///
    /// # Errors
    /// Refuses source/index divergence, invalid metadata and byte bounds.
    pub async fn snapshot_dream_sources(
        &self,
        heads: Vec<ai_memory_store::DreamHead>,
        max_bytes: usize,
    ) -> WikiResult<Vec<DreamSource>> {
        if heads.is_empty()
            || heads.len() > 2
            || max_bytes == 0
            || max_bytes > ai_memory_store::DREAM_SOURCE_MAX_BYTES
        {
            return Err(dream_refused("dream source snapshot shape invalid"));
        }
        let _guard = self.mutation_lock.read().await;
        let keys: Vec<_> = heads
            .iter()
            .map(|h| (h.workspace_id, h.project_id, h.candidate.path.clone()))
            .collect();
        let _page_guards = self.lock_pages(&keys).await;
        let _file_locks = self.lock_page_files(&keys).await?;
        let mut sources = Vec::with_capacity(heads.len());
        for head in heads {
            self.ensure_project_scope_rows(head.workspace_id, head.project_id)
                .await?;
            if head.body_bytes > max_bytes {
                return Err(dream_refused(
                    "dream source byte cap exceeded; source refused without truncation",
                ));
            }
            let bytes = read_dream_file(
                &self.confined_page_path(
                    head.workspace_id,
                    head.project_id,
                    &head.candidate.path,
                    Prepare::Inspect,
                )?,
                max_bytes,
            )?;
            let raw = std::str::from_utf8(&bytes)
                .map_err(|_| dream_refused("dream source is not valid UTF-8"))?;
            let markdown =
                parse(raw).map_err(|_| dream_refused("dream source Markdown invalid"))?;
            let indexed_frontmatter: serde_json::Value =
                serde_json::from_str(&head.candidate.frontmatter_json)
                    .map_err(|_| dream_refused("dream indexed frontmatter invalid"))?;
            if !markdown.frontmatter.is_object()
                || ai_memory_store::dream_source_digest(markdown.body.as_bytes())
                    != head.body_sha256
                || ai_memory_core::okf::strip_generated_at(&markdown.frontmatter)
                    != ai_memory_core::okf::strip_generated_at(&indexed_frontmatter)
            {
                return Err(dream_refused(
                    "dream source file does not match indexed head",
                ));
            }
            sources.push(DreamSource {
                head,
                markdown,
                file_sha256: ai_memory_store::dream_source_digest(&bytes),
            });
        }
        Ok(sources)
    }

    /// Apply source-derived rewrites with disk and SQL CAS, a pre-merge git
    /// checkpoint, and cancellation rechecked inside the writer transaction.
    /// Every source and request must name exactly the same set of paths.
    ///
    /// # Errors
    /// Any stale source, cancellation, checkpoint/admission/write failure
    /// refuses the whole batch and restores installed files on SQL failure.
    pub async fn apply_dream_batch(
        &self,
        requests: Vec<WritePageRequest>,
        sources: &[DreamSource],
        cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> WikiResult<DreamApplyResult> {
        let request_keys: HashSet<_> = requests
            .iter()
            .map(|r| (r.workspace_id, r.project_id, r.path.clone()))
            .collect();
        let source_keys: HashSet<_> = sources
            .iter()
            .map(|s| {
                (
                    s.head.workspace_id,
                    s.head.project_id,
                    s.head.candidate.path.clone(),
                )
            })
            .collect();
        if requests.is_empty()
            || requests.len() > 2
            || request_keys.len() != requests.len()
            || sources.len() != requests.len()
            || source_keys != request_keys
        {
            return Err(dream_refused("dream source coordinates do not match batch"));
        }
        if cancel() {
            return Err(dream_refused("dream cancelled before apply"));
        }
        let mut local = self.dream_local_clone()?;
        if let Some(chain) = &mut local.admission_chain {
            chain.set_dream_cancel(cancel.clone());
        }
        let (outcome, checkpoint) = local
            .apply_batch_inner(
                requests,
                None,
                Some(DreamBatchGuard {
                    sources: sources.to_vec(),
                    cancel,
                }),
            )
            .await?;
        let checkpoint =
            checkpoint.ok_or_else(|| dream_refused("dream pre-merge checkpoint missing"))?;
        match outcome {
            ConditionalBatchWrite::Written(ids) => Ok(DreamApplyResult { ids, checkpoint }),
            ConditionalBatchWrite::Mismatch { .. } => {
                Err(dream_refused("dream head precondition changed"))
            }
        }
    }
}

// Before file installation, lock waits remain cancellable. Once files have been
// installed, the common funnel drains the writer and rollback instead.
pub(super) async fn await_lock<T>(
    future: impl std::future::Future<Output = T>,
    cancel: &Arc<dyn Fn() -> bool + Send + Sync>,
) -> WikiResult<T> {
    tokio::pin!(future);
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(25));
    loop {
        if cancel() {
            return Err(dream_refused("dream cancelled while waiting for lock"));
        }
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = poll.tick() => {},
        }
    }
}
