//! Bounded dream snapshots and compare-and-swap within the single writer.

use crate::{DecayCandidate, ReaderPool, StoreError, StoreResult, StoredEmbedding};
use ai_memory_core::{NewPage, PageId, ProjectId, Tier, WorkspaceId};
use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Hard upper bound for one dream scope's head metadata and vectors.
pub const DREAM_MAX_HEADS: usize = 4096;
/// Hard upper bound for source files and materialized frontmatter bytes.
pub const DREAM_SOURCE_MAX_BYTES: usize = 6000;
// Together with the head cap this bounds materialized vectors to 128 MiB.
const DREAM_MAX_VECTOR_DIM: u32 = 8192;

/// SHA-256 of complete bounded source bytes; shared with the wiki CAS boundary.
#[must_use]
pub fn dream_source_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// A metadata-only head snapshot; the identifier and update instant are its version.
#[derive(Debug, Clone)]
pub struct DreamHead {
    /// Exact workspace coordinate.
    pub workspace_id: WorkspaceId,
    /// Exact project coordinate.
    pub project_id: ProjectId,
    /// All inputs to the shared cold-page scoring function.
    pub candidate: DecayCandidate,
    /// SHA-256 of the complete source body, as stored by the writer.
    pub body_sha256: [u8; 32],
    /// Complete source body size in bytes; never a truncated estimate.
    pub body_bytes: usize,
    /// Distinct actor count used when breadth scoring is enabled.
    pub access_breadth: u32,
    /// Digest of the exact metadata, including every eligibility input.
    pub eligibility_sha256: [u8; 32],
}

const HEAD_SELECT: &str = "SELECT p.id,p.path,p.tier,p.pinned,p.updated_at,p.access_count,\
 p.last_accessed_at,p.frontmatter_json,p.expires_at,p.salience,p.compacted_at,\
 p.body_sha256,length(CAST(p.body AS BLOB)),\
 (SELECT COUNT(*) FROM page_access a WHERE a.page_id=p.id),p.created_at FROM pages p";

fn invalid(reason: &str) -> StoreError {
    StoreError::InvalidState(reason.into())
}

fn check_cap(cap: usize) -> StoreResult<()> {
    if cap == 0 || cap > DREAM_MAX_HEADS {
        return Err(invalid("dream head cap must be 1..=4096"));
    }
    Ok(())
}

fn head_from_row(
    row: &rusqlite::Row<'_>,
    ws: WorkspaceId,
    proj: ProjectId,
) -> rusqlite::Result<StoreResult<DreamHead>> {
    let candidate = crate::reader::row_to_decay_candidate(row)?;
    let digest_bytes: Vec<u8> = row.get(11)?;
    let body_bytes: i64 = row.get(12)?;
    let breadth: i64 = row.get(13)?;
    let created: i64 = row.get(14)?;
    let raw_access: i64 = row.get(5)?;
    let raw_pinned: i64 = row.get(3)?;
    Ok((|| {
        let candidate = candidate?;
        let metadata = serde_json::to_vec(&(
            &candidate,
            &digest_bytes,
            body_bytes,
            breadth,
            created,
            raw_access,
            raw_pinned,
        ))?;
        let body_sha256 = digest_bytes
            .try_into()
            .map_err(|_| invalid("dream source digest is malformed"))?;
        Ok(DreamHead {
            workspace_id: ws,
            project_id: proj,
            candidate,
            body_sha256,
            body_bytes: usize::try_from(body_bytes)
                .map_err(|_| invalid("dream source length is malformed"))?,
            access_breadth: u32::try_from(breadth)
                .map_err(|_| invalid("dream access breadth is malformed"))?,
            eligibility_sha256: Sha256::digest(metadata).into(),
        })
    })())
}

impl ReaderPool {
    /// Read bounded head metadata without bodies or vectors. Cap+1 refuses the
    /// entire scope before frontmatter materialization; no sampling is allowed.
    ///
    /// # Errors
    /// Rejects an exceeded cap, oversized frontmatter, or invalid store data.
    pub async fn dream_heads_bounded(
        &self,
        ws: WorkspaceId,
        proj: ProjectId,
        cap: usize,
    ) -> StoreResult<Vec<DreamHead>> {
        check_cap(cap)?;
        self.with_conn(move |conn| {
            let conn = conn.unchecked_transaction()?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM (SELECT id FROM pages \
                 WHERE workspace_id=?1 AND project_id=?2 AND is_latest=1 LIMIT ?3)",
                params![ws.as_bytes(), proj.as_bytes(), cap as i64 + 1],
                |r| r.get(0),
            )?;
            if count > cap as i64 {
                return Err(invalid(
                    "dream head cap exceeded; scope refused without sampling",
                ));
            }
            let oversized: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pages WHERE workspace_id=?1 \
                 AND project_id=?2 AND is_latest=1 \
                 AND length(CAST(frontmatter_json AS BLOB))>?3)",
                params![
                    ws.as_bytes(),
                    proj.as_bytes(),
                    DREAM_SOURCE_MAX_BYTES as i64
                ],
                |r| r.get(0),
            )?;
            if oversized {
                return Err(invalid("dream frontmatter byte cap exceeded"));
            }
            let sql = format!(
                "{HEAD_SELECT} WHERE p.workspace_id=?1 AND p.project_id=?2 \
                 AND p.is_latest=1 ORDER BY p.path LIMIT ?3"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![ws.as_bytes(), proj.as_bytes(), cap as i64 + 1],
                |r| head_from_row(r, ws, proj),
            )?;
            let mut heads = Vec::new();
            for row in rows {
                if heads.len() == cap {
                    return Err(invalid(
                        "dream head cap exceeded; scope refused without sampling",
                    ));
                }
                heads.push(row??);
            }
            Ok(heads)
        })
        .await
    }

    /// Load bounded matching vectors after the caller has bounded cold metadata.
    /// Vector count and complete blob lengths are checked before loading blobs.
    ///
    /// # Errors
    /// Rejects cap+1, dimensions outside 1..=8192, and malformed vector lengths
    /// instead of sampling. The hard ceilings bound vectors to 128 MiB.
    pub async fn load_embeddings_bounded(
        &self,
        ws: WorkspaceId,
        proj: ProjectId,
        provider: String,
        model: String,
        dim: u32,
        cap: usize,
    ) -> StoreResult<Vec<StoredEmbedding>> {
        check_cap(cap)?;
        if dim == 0 || dim > DREAM_MAX_VECTOR_DIM {
            return Err(invalid("dream vector dimensions must be 1..=8192"));
        }
        self.with_conn(move |conn| {
            let conn = conn.unchecked_transaction()?;
            let from = "FROM page_embeddings e JOIN pages p ON p.id=e.page_id \
                        WHERE p.workspace_id=?1 AND p.project_id=?2 AND p.is_latest=1 \
                        AND e.provider=?3 AND e.model=?4 AND e.dim=?5";
            let count: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM (SELECT e.page_id {from} LIMIT {})",
                    cap + 1
                ),
                params![ws.as_bytes(), proj.as_bytes(), provider, model, dim],
                |r| r.get(0),
            )?;
            if count > cap as i64 {
                return Err(invalid(
                    "dream vector cap exceeded; scope refused without sampling",
                ));
            }
            let malformed: bool = conn.query_row(
                &format!("SELECT EXISTS(SELECT 1 {from} AND length(e.vector)!=?6)"),
                params![
                    ws.as_bytes(),
                    proj.as_bytes(),
                    provider,
                    model,
                    dim,
                    i64::from(dim) * 4
                ],
                |r| r.get(0),
            )?;
            if malformed {
                return Err(invalid("dream vector dimensions are malformed"));
            }
            let mut stmt = conn.prepare(&format!(
                "SELECT e.page_id,e.vector,p.path {from} ORDER BY p.path LIMIT {}",
                cap + 1
            ))?;
            let mut rows = stmt.query(params![
                ws.as_bytes(),
                proj.as_bytes(),
                provider,
                model,
                dim
            ])?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                if result.len() == cap {
                    return Err(invalid(
                        "dream vector cap exceeded; scope refused without sampling",
                    ));
                }
                let bytes: Vec<u8> = row.get(1)?;
                if bytes.len() != dim as usize * 4 {
                    return Err(invalid("dream vector dimensions changed during read"));
                }
                result.push(StoredEmbedding {
                    id: PageId::from_slice(&row.get::<_, Vec<u8>>(0)?)?,
                    path: ai_memory_core::PagePath::new(row.get::<_, String>(2)?)?,
                    vector: bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect(),
                });
            }
            Ok(result)
        })
        .await
    }
}

pub(crate) fn upsert_dream_pages_batch(
    conn: &mut Connection,
    pages: &[NewPage],
    heads: &[DreamHead],
    cancel: &(dyn Fn() -> bool + Send + Sync),
) -> StoreResult<Vec<PageId>> {
    if cancel() {
        return Err(invalid("dream cancelled before transaction"));
    }
    if pages.is_empty() || pages.len() != heads.len() || pages.len() > 2 {
        return Err(invalid("dream batch/source shape invalid"));
    }
    let keys: HashSet<_> = heads
        .iter()
        .map(|h| (h.workspace_id, h.project_id, h.candidate.path.clone()))
        .collect();
    let request_keys: HashSet<_> = pages
        .iter()
        .map(|p| (p.workspace_id, p.project_id, p.path.clone()))
        .collect();
    if keys.len() != heads.len() || request_keys != keys {
        return Err(invalid("dream source coordinates do not match batch"));
    }
    // A survivor cannot represent facts with different retention deadlines.
    // Compare the stored instants, including absence, rather than date text.
    let shared_expiry = heads[0].candidate.expires_at_us;
    if heads
        .iter()
        .any(|h| h.candidate.expires_at_us != shared_expiry)
        || pages
            .iter()
            .any(|p| p.expires_at.map(|t| t.as_microsecond()) != shared_expiry)
    {
        return Err(invalid("dream requires unchanged identical source expiry"));
    }
    let now = Timestamp::now().as_microsecond();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for expected in heads {
        let size_ok: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pages WHERE workspace_id=?1 AND project_id=?2 \
             AND path=?3 AND is_latest=1 AND length(CAST(frontmatter_json AS BLOB))<=?4)",
            params![
                expected.workspace_id.as_bytes(),
                expected.project_id.as_bytes(),
                expected.candidate.path.as_str(),
                DREAM_SOURCE_MAX_BYTES as i64
            ],
            |r| r.get(0),
        )?;
        if !size_ok {
            return Err(invalid(
                "dream source is missing or metadata exceeds byte cap",
            ));
        }
        let sql = format!(
            "{HEAD_SELECT} WHERE p.workspace_id=?1 AND p.project_id=?2 AND p.path=?3 AND p.is_latest=1"
        );
        let actual = tx
            .query_row(
                &sql,
                params![
                    expected.workspace_id.as_bytes(),
                    expected.project_id.as_bytes(),
                    expected.candidate.path.as_str()
                ],
                |r| head_from_row(r, expected.workspace_id, expected.project_id),
            )
            .optional()?
            .transpose()?
            .ok_or_else(|| invalid("dream source is no longer current"))?;
        let c = &actual.candidate;
        let fm: serde_json::Value = serde_json::from_str(&c.frontmatter_json)?;
        if actual.candidate.id != expected.candidate.id
            || actual.body_sha256 != expected.body_sha256
            || actual.eligibility_sha256 != expected.eligibility_sha256
            || c.tier != Tier::Episodic
            || c.pinned
            || c.compacted_at_us.is_some()
            || c.expires_at_us.is_some_and(|ttl| ttl <= now)
            || !fm.is_object()
            || fm.get("pinned").and_then(serde_json::Value::as_bool) == Some(true)
        {
            return Err(invalid(
                "dream source changed or became ineligible; batch cancelled",
            ));
        }
    }
    let mut ids = Vec::with_capacity(pages.len());
    for page in pages {
        ids.push(crate::ops::upsert_page_in_tx(&tx, page, now)?);
    }
    if cancel() {
        return Err(invalid("dream cancelled before commit"));
    }
    tx.commit()?;
    Ok(ids)
}
