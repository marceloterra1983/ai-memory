//! Reciprocal-rank fusion shared by live hybrid search and `as_of` audit.
//!
//! `as_of` does not grow a vector or graph stream. It pushes the streams it
//! has; k, the per-stream strength, and the explain fields stay here. The
//! first push that names an id keeps that stream's title and snippet.

use std::collections::HashMap;

use super::{GraphVia, PageHit, PageId, PagePath, SearchExplain};

const RRF_K: f64 = 60.0;

pub(super) struct RankedHit {
    pub id: PageId,
    pub path: PagePath,
    pub title: String,
    pub snippet: String,
    pub evidence: Evidence,
}

pub(super) enum Evidence {
    Fts { strength: f64 },
    Entity { weight: f64, matched: Vec<String> },
    Vector { cosine: f32 },
    Abstract { cosine: f32 },
    Graph { strength: f64, via: GraphVia },
}

pub(super) struct Fusion {
    explain: bool,
    hits: HashMap<PageId, Acc>,
}

struct Acc {
    path: PagePath,
    title: String,
    snippet: String,
    score: f64,
    explain: Option<SearchExplain>,
}

impl Fusion {
    pub(super) fn new(explain: bool) -> Self {
        Self {
            explain,
            hits: HashMap::new(),
        }
    }

    /// `hits` is already ordered. Position zero is rank 1.
    pub(super) fn push(&mut self, hits: impl IntoIterator<Item = RankedHit>) {
        for (rank, hit) in hits.into_iter().enumerate() {
            let strength = strength_of(&hit.evidence);
            #[allow(clippy::cast_precision_loss)]
            let contrib = strength / (RRF_K + (rank + 1) as f64);
            let RankedHit {
                id,
                path,
                title,
                snippet,
                evidence,
            } = hit;
            let explain = self.explain;
            let entry = self.hits.entry(id).or_insert_with(|| Acc {
                path,
                title,
                snippet,
                score: 0.0,
                explain: explain.then(SearchExplain::default),
            });
            entry.score += contrib;
            if let Some(details) = &mut entry.explain {
                record(details, &evidence, rank, contrib);
            }
        }
    }

    pub(super) fn into_hits(self) -> Vec<(PageHit, Option<SearchExplain>)> {
        self.hits
            .into_iter()
            .map(|(id, entry)| {
                (
                    PageHit {
                        id,
                        path: entry.path,
                        title: entry.title,
                        snippet: entry.snippet,
                        // Lower is better, matching the FTS5 convention.
                        rank: -entry.score,
                        superseded: false,
                        pinned: false,
                    },
                    entry.explain,
                )
            })
            .collect()
    }
}

/// Equal fused ranks break by path, not by hash-map iteration.
pub(super) fn sort_and_truncate(out: &mut Vec<(PageHit, Option<SearchExplain>)>, limit: usize) {
    out.sort_by(|a, b| {
        a.0.rank
            .partial_cmp(&b.0.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.path.as_str().cmp(b.0.path.as_str()))
    });
    out.truncate(limit);
}

fn strength_of(evidence: &Evidence) -> f64 {
    match evidence {
        Evidence::Fts { strength } | Evidence::Graph { strength, .. } => *strength,
        Evidence::Entity { weight, .. } => weight.clamp(0.0, 1.0),
        Evidence::Vector { cosine } | Evidence::Abstract { cosine } => {
            f64::from(*cosine).clamp(0.0, 1.0)
        }
    }
}

fn record(details: &mut SearchExplain, evidence: &Evidence, rank: usize, contrib: f64) {
    let place = rank + 1;
    match evidence {
        Evidence::Fts { strength } => {
            details.fts_rank = Some(place);
            details.fts_score = Some(-strength);
            details.rrf.fts = contrib;
        }
        Evidence::Entity { weight, matched } => {
            details.entity_rank = Some(place);
            details.entity_weight = Some(*weight);
            details.matched_entities.clone_from(matched);
            details.rrf.entity = contrib;
        }
        Evidence::Vector { cosine } => {
            details.vector_rank = Some(place);
            details.cosine = Some(*cosine);
            details.rrf.vector = contrib;
        }
        Evidence::Abstract { cosine } => {
            details.abstract_rank = Some(place);
            details.abstract_cosine = Some(*cosine);
            details.rrf.abstract_vector = contrib;
        }
        Evidence::Graph { via, .. } => {
            details.graph_rank = Some(place);
            details.graph_via = Some(via.clone());
            details.rrf.graph = contrib;
        }
    }
    details.fused += contrib;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(raw: &str) -> PagePath {
        PagePath::new(raw).unwrap()
    }

    fn fts(id: PageId, page: &str, strength: f64) -> RankedHit {
        RankedHit {
            id,
            path: path(page),
            title: page.to_string(),
            snippet: String::new(),
            evidence: Evidence::Fts { strength },
        }
    }

    #[test]
    fn fts_and_entity_add_under_k_60_and_leave_vector_empty() {
        let id = PageId::new();
        let mut fusion = Fusion::new(true);
        fusion.push([fts(id, "notes/a.md", 1.0)]);
        fusion.push([RankedHit {
            id,
            path: path("notes/a.md"),
            title: "ignored".into(),
            snippet: String::new(),
            evidence: Evidence::Entity {
                weight: 2.0,
                matched: vec!["zeta".into()],
            },
        }]);
        let mut hits = fusion.into_hits();
        assert_eq!(hits.len(), 1);
        let (hit, explain) = hits.pop().unwrap();
        let explain = explain.unwrap();
        let expected = (1.0 / 61.0) + (1.0 / 61.0);
        assert_eq!(hit.rank, -expected);
        assert_eq!(hit.title, "notes/a.md");
        assert_eq!(explain.fts_rank, Some(1));
        assert_eq!(explain.fts_score, Some(-1.0));
        assert_eq!(explain.rrf.fts, 1.0 / 61.0);
        assert_eq!(explain.entity_weight, Some(2.0));
        assert_eq!(explain.rrf.entity, 1.0 / 61.0);
        assert_eq!(explain.matched_entities, vec!["zeta".to_string()]);
        assert_eq!(explain.fused, expected);
        assert_eq!(explain.vector_rank, None);
        assert_eq!(explain.rrf.vector, 0.0);
        assert_eq!(explain.graph_rank, None);
    }

    #[test]
    fn negative_cosine_does_not_vote_and_graph_records_its_via() {
        let id = PageId::new();
        let mut fusion = Fusion::new(true);
        fusion.push([RankedHit {
            id,
            path: path("notes/v.md"),
            title: String::new(),
            snippet: String::new(),
            evidence: Evidence::Vector { cosine: -0.4 },
        }]);
        fusion.push([RankedHit {
            id,
            path: path("notes/v.md"),
            title: "from-graph".into(),
            snippet: "snip".into(),
            evidence: Evidence::Graph {
                strength: 0.5,
                via: GraphVia {
                    seed_path: "notes/seed.md".into(),
                    direction: "incoming",
                    edge: Some("causes".into()),
                },
            },
        }]);
        let (hit, explain) = fusion.into_hits().pop().unwrap();
        let explain = explain.unwrap();
        assert_eq!(hit.title, "");
        assert_eq!(hit.rank, -(0.5 / 61.0));
        assert_eq!(explain.cosine, Some(-0.4));
        assert_eq!(explain.rrf.vector, 0.0);
        assert_eq!(explain.graph_rank, Some(1));
        assert_eq!(explain.rrf.graph, 0.5 / 61.0);
        let via = explain.graph_via.unwrap();
        assert_eq!(via.seed_path, "notes/seed.md");
        assert_eq!(via.direction, "incoming");
        assert_eq!(via.edge.as_deref(), Some("causes"));
    }

    #[test]
    fn equal_ranks_sort_by_path_before_the_limit() {
        let mut out = vec![
            (
                PageHit {
                    id: PageId::new(),
                    path: path("notes/b.md"),
                    title: String::new(),
                    snippet: String::new(),
                    rank: -1.0,
                    superseded: false,
                    pinned: false,
                },
                None,
            ),
            (
                PageHit {
                    id: PageId::new(),
                    path: path("notes/a.md"),
                    title: String::new(),
                    snippet: String::new(),
                    rank: -1.0,
                    superseded: false,
                    pinned: false,
                },
                None,
            ),
        ];
        sort_and_truncate(&mut out, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.path.as_str(), "notes/a.md");
    }
}
