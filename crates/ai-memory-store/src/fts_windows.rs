//! One snapshot for the two page-FTS field windows.
//!
//! Body rank and title BM25 must observe the same page versions and grants.
//! Running them as separate autocommit queries let a supersession land between
//! the windows, so each search copied the same transaction. The merge, the
//! tie midranks, and the common-term floor live here for the same reason.

use std::hash::Hash;

use rusqlite::Connection;

use super::PageAuthority;
use crate::error::StoreResult;

/// Body window, then title window. Callers pop in reverse.
const FIELD_WINDOW_SCORES: [&str; 2] = ["pages_fts.rank", "bm25(pages_fts, 1, 0, 0)"];

/// Run both field windows on one snapshot and merge them.
///
/// `query_window` receives the snapshot connection and the score expression to
/// interpolate. It does not choose the expression or open the transaction.
/// `between_windows` runs after the body window and before the title window
/// so a concurrency probe can pause there without holding a second snapshot.
///
/// # Errors
/// Propagates a failure to open the snapshot or any error from `query_window`.
pub(super) fn merge_field_windows<T, K>(
    conn: &Connection,
    fts_query: &str,
    mut query_window: impl FnMut(&Connection, &str) -> StoreResult<Vec<(T, PageAuthority)>>,
    mut between_windows: impl FnMut(),
    key: impl Fn(&T) -> K,
    rank: impl Fn(&T) -> f64,
    set_rank: impl Fn(&mut T, f64),
) -> StoreResult<Vec<(T, PageAuthority, f64)>>
where
    K: Eq + Hash,
{
    let snapshot = conn.unchecked_transaction()?;
    let mut streams = Vec::with_capacity(FIELD_WINDOW_SCORES.len());
    for score in FIELD_WINDOW_SCORES {
        if !streams.is_empty() {
            between_windows();
        }
        streams.push(query_window(&snapshot, score)?);
    }
    let titles = streams.pop().unwrap_or_default();
    let normal = streams.pop().unwrap_or_default();
    Ok(merge_fts_candidates(
        normal,
        titles,
        floor_bound(fts_query),
        key,
        rank,
        set_rank,
    ))
}

fn floor_bound(fts_query: &str) -> f64 {
    // SQLite's IDF floor is 1e-6 and a phrase's TF contribution caps at 2.2.
    #[allow(clippy::cast_precision_loss)]
    let terms = fts_query.split_whitespace().count() as f64;
    2.2e-6 * terms
}

// Midranks preserve tied evidence without giving every tied page a full
// first-place vote. That would multiply a weak field's influence by its ties.
fn fts_tie_ranks<T>(stream: &[(T, PageAuthority)], rank: &impl Fn(&T) -> f64) -> Vec<f64> {
    let mut ranks = vec![0.0; stream.len()];
    let mut start = 0;
    while start < stream.len() {
        let score = rank(&stream[start].0);
        let end = start
            + stream[start..]
                .iter()
                .take_while(|(hit, _)| rank(hit) == score)
                .count();
        #[allow(clippy::cast_precision_loss)]
        {
            ranks[start..end].fill((start + 1 + end) as f64 / 2.0);
        }
        start = end;
    }
    ranks
}

/// Preserve strong topic matches alongside body/path evidence. Adding the
/// field scores rewards incidental overlap; the better reciprocal rank keeps
/// a long, topical page competitive without counting that overlap twice.
fn merge_fts_candidates<T, K: Eq + Hash>(
    normal: Vec<(T, PageAuthority)>,
    titles: Vec<(T, PageAuthority)>,
    floor_bound: f64,
    key: impl Fn(&T) -> K,
    rank: impl Fn(&T) -> f64,
    set_rank: impl Fn(&mut T, f64),
) -> Vec<(T, PageAuthority, f64)> {
    struct Candidate<T> {
        hit: T,
        authority: PageAuthority,
        normal_rank: f64,
        title_rank: f64,
        normal_score: f64,
        title_score: f64,
        order: usize,
    }
    let peak = normal.first().map_or(0.0, |(hit, _)| rank(hit).abs());
    let mut candidates = std::collections::HashMap::new();
    let normal_ranks = fts_tie_ranks(&normal, &rank);
    let title_ranks = fts_tie_ranks(&titles, &rank);
    let normal_count = normal.len();
    for (order, ((hit, authority), normal_rank)) in normal.into_iter().zip(normal_ranks).enumerate()
    {
        let score = rank(&hit);
        candidates.insert(
            key(&hit),
            Candidate {
                normal_score: score,
                hit,
                authority,
                normal_rank,
                title_rank: f64::INFINITY,
                title_score: 0.0,
                order,
            },
        );
    }
    for (order, ((hit, authority), title_rank)) in titles.into_iter().zip(title_ranks).enumerate() {
        // MATCH still covers all fields so both rankers share term statistics.
        // A page matched only in its body/path supplies no title evidence.
        if rank(&hit) >= 0.0 {
            continue;
        }
        let score = rank(&hit);
        candidates
            .entry(key(&hit))
            .and_modify(|candidate| {
                candidate.title_rank = title_rank;
                candidate.title_score = score;
            })
            .or_insert(Candidate {
                hit,
                authority,
                normal_rank: f64::INFINITY,
                title_rank,
                normal_score: 0.0,
                title_score: score,
                order: normal_count + order,
            });
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    for candidate in &mut candidates {
        let normal_score = if peak > floor_bound {
            1.0 / candidate.normal_rank
        } else {
            // SQLite floors IDF at 1e-6 and caps each phrase's TF contribution
            // at k1 + 1 = 2.2. Turning scores in that floor range into ordinal
            // ranks magnifies tiny frequency differences over page authority.
            candidate.normal_score.abs() / peak.max(f64::MIN_POSITIVE)
        };
        let title_score = if peak > floor_bound && candidate.title_score.abs() <= floor_bound {
            // A common-term title at SQLite's IDF floor cannot supply a full
            // ordinal vote beside informative body evidence in another scope.
            candidate.title_score.abs() / peak
        } else {
            1.0 / candidate.title_rank
        };
        set_rank(&mut candidate.hit, -normal_score.max(title_score));
    }
    candidates.sort_by(|a, b| {
        rank(&a.hit)
            .total_cmp(&rank(&b.hit))
            .then_with(|| a.title_rank.total_cmp(&b.title_rank))
            .then_with(|| a.normal_rank.total_cmp(&b.normal_rank))
            // Keep repeated queries stable without adding score evidence.
            .then_with(|| a.order.cmp(&b.order))
    });
    candidates
        .into_iter()
        .map(|candidate| {
            // Common-term IDF floors carry no calibrated magnitude: preserve
            // the existing uniform RRF vote rather than magnifying TF/length
            // differences over bounded session intent and page authority.
            let strength = if peak <= floor_bound && candidate.normal_rank.is_finite() {
                1.0
            } else {
                (-rank(&candidate.hit)).clamp(0.0, 1.0)
            };
            (candidate.hit, candidate.authority, strength)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority() -> PageAuthority {
        PageAuthority::from_stored("notes/a.md", "concept", "semantic", false, "{}")
    }

    #[test]
    fn windows_use_body_then_title_and_keep_a_negative_title_hit() {
        let conn = Connection::open_in_memory().unwrap();
        let mut scores = Vec::new();
        let mut between = 0;
        let merged = merge_field_windows(
            &conn,
            "zeta",
            |_snapshot, score| {
                scores.push(score.to_string());
                let hit = if score == "pages_fts.rank" {
                    ("body", -0.4)
                } else if score == "bm25(pages_fts, 1, 0, 0)" {
                    ("title-only", -0.2)
                } else {
                    panic!("unexpected score expression {score}");
                };
                Ok(vec![(hit, authority())])
            },
            || between += 1,
            |hit: &(&str, f64)| hit.0,
            |hit| hit.1,
            |hit, rank| hit.1 = rank,
        )
        .unwrap();
        assert_eq!(
            scores,
            vec![
                "pages_fts.rank".to_string(),
                "bm25(pages_fts, 1, 0, 0)".to_string()
            ]
        );
        assert_eq!(between, 1);
        let mut keys: Vec<_> = merged.iter().map(|(hit, _, _)| hit.0).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["body", "title-only"]);
    }

    #[test]
    fn non_negative_title_score_is_not_title_evidence() {
        let conn = Connection::open_in_memory().unwrap();
        let merged = merge_field_windows(
            &conn,
            "zeta",
            |_snapshot, score| {
                let rows = if score == "pages_fts.rank" {
                    vec![]
                } else {
                    vec![(("body-only", 0.0), authority())]
                };
                Ok(rows)
            },
            || {},
            |hit: &(&str, f64)| hit.0,
            |hit| hit.1,
            |hit, rank| hit.1 = rank,
        )
        .unwrap();
        assert!(merged.is_empty());
    }

    #[test]
    fn tied_body_scores_share_one_midrank() {
        let conn = Connection::open_in_memory().unwrap();
        let merged = merge_field_windows(
            &conn,
            "zeta",
            |_snapshot, score| {
                let rows = if score == "pages_fts.rank" {
                    vec![(1_u8, -0.5), (2, -0.5)]
                } else {
                    vec![]
                };
                Ok(rows.into_iter().map(|hit| (hit, authority())).collect())
            },
            || {},
            |hit: &(u8, f64)| hit.0,
            |hit| hit.1,
            |hit, rank| hit.1 = rank,
        )
        .unwrap();
        assert_eq!(merged.len(), 2);
        let strength = 1.0 / 1.5;
        assert_eq!(merged[0].2, strength);
        assert_eq!(merged[1].2, strength);
        assert_eq!(merged[0].0.0, 1);
        assert_eq!(merged[1].0.0, 2);
    }

    #[test]
    fn idf_floor_keeps_a_uniform_strength() {
        let conn = Connection::open_in_memory().unwrap();
        let merged = merge_field_windows(
            &conn,
            "zeta",
            |_snapshot, score| {
                let rows = if score == "pages_fts.rank" {
                    vec![(1_u8, -1e-8)]
                } else {
                    vec![]
                };
                Ok(rows.into_iter().map(|hit| (hit, authority())).collect())
            },
            || {},
            |hit: &(u8, f64)| hit.0,
            |hit| hit.1,
            |hit, rank| hit.1 = rank,
        )
        .unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].2, 1.0);
    }

    #[test]
    fn both_windows_read_one_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("windows.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer
            .execute_batch("CREATE TABLE rows (n INTEGER); INSERT INTO rows (n) VALUES (1);")
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        let mut counts = Vec::new();
        let merged = merge_field_windows(
            &reader,
            "alpha",
            |snapshot, _score| {
                let n: i64 =
                    snapshot.query_row("SELECT COUNT(*) FROM rows", [], |row| row.get(0))?;
                counts.push(n);
                Ok(Vec::<(u8, PageAuthority)>::new())
            },
            || {
                writer
                    .execute("INSERT INTO rows (n) VALUES (2)", [])
                    .unwrap();
            },
            |hit: &u8| *hit,
            |_| 0.0,
            |_, _| {},
        )
        .unwrap();
        assert!(merged.is_empty());
        assert_eq!(counts, vec![1, 1]);
        let visible: i64 = writer
            .query_row("SELECT COUNT(*) FROM rows", [], |row| row.get(0))
            .unwrap();
        assert_eq!(visible, 2);
    }
}
