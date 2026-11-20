use ai_memory_consolidate::dream::DreamMerge;
use ai_memory_core::{PageId, PagePath};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::environment::{Environment, Snapshot};

pub(super) const TOP_K: usize = 3;

#[derive(Deserialize)]
pub(super) struct Task {
    pub id: String,
    pub query: String,
    pub requires: Vec<Requirement>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct Requirement {
    pub source: String,
    pub literal: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Hit {
    pub path: PagePath,
    pub id: PageId,
}

#[derive(Clone)]
struct ContextPage {
    hit: Hit,
    body: String,
    frontmatter: Value,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct Volume {
    pub pages: usize,
    pub utf8_bytes: usize,
    pub estimated_tokens: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct EvidenceHit {
    pub requirement: Requirement,
    pub original_id: PageId,
    pub retrieved_at: Hit,
}

#[derive(Debug, Serialize)]
pub(super) struct PhaseReceipt {
    pub hits: Vec<Hit>,
    pub evidence: Vec<EvidenceHit>,
    pub missing: Vec<Requirement>,
    pub context: Volume,
}

#[derive(Serialize)]
pub(super) struct Controls {
    pub task: String,
    pub lost_literal: Vec<Requirement>,
    pub lost_source: Vec<Requirement>,
}

pub(super) struct QueryRun {
    pub receipt: PhaseReceipt,
    pages: Vec<ContextPage>,
}

pub(super) async fn run_queries(
    env: &Environment,
    tasks: &[Task],
    original: &Snapshot,
    merge: Option<&DreamMerge>,
    phase: &str,
) -> Result<Vec<QueryRun>> {
    let mut results = Vec::with_capacity(tasks.len());
    for task in tasks {
        let hits = env
            .store
            .reader
            .search_pages_for_project(env.ws, env.project, task.query.clone(), TOP_K, None)
            .await?;
        let mut pages = Vec::with_capacity(hits.len());
        for hit in hits {
            let page = env.version(hit.path.as_str(), hit.id).await?;
            pages.push(ContextPage {
                hit: Hit {
                    path: hit.path,
                    id: hit.id,
                },
                body: page.body,
                frontmatter: page.frontmatter,
            });
        }
        let receipt = score(task, &pages, original, merge);
        ensure!(
            receipt.missing.is_empty(),
            "{phase} query {} lacks evidence: {:?}",
            task.id,
            receipt.missing
        );
        results.push(QueryRun { receipt, pages });
    }
    Ok(results)
}

fn score(
    task: &Task,
    pages: &[ContextPage],
    original: &Snapshot,
    merge: Option<&DreamMerge>,
) -> PhaseReceipt {
    let mut evidence = Vec::new();
    let mut missing = Vec::new();
    for requirement in &task.requires {
        let original_id = original.heads[&requirement.source].id;
        let moved = merge.filter(|merge| merge.sources.iter().any(|s| s.id == original_id));
        let found = pages.iter().find(|page| {
            page.body.contains(&requirement.literal) && cites(page, requirement, original_id, moved)
        });
        if let Some(page) = found {
            evidence.push(EvidenceHit {
                requirement: requirement.clone(),
                original_id,
                retrieved_at: page.hit.clone(),
            });
        } else {
            missing.push(requirement.clone());
        }
    }
    let chars: usize = pages.iter().map(|page| page.body.chars().count()).sum();
    PhaseReceipt {
        evidence,
        missing,
        hits: pages.iter().map(|page| page.hit.clone()).collect(),
        context: Volume {
            pages: pages.len(),
            utf8_bytes: pages.iter().map(|page| page.body.len()).sum(),
            estimated_tokens: chars.div_ceil(4),
        },
    }
}

fn cites(
    page: &ContextPage,
    required: &Requirement,
    original: PageId,
    merge: Option<&DreamMerge>,
) -> bool {
    let Some(merge) = merge else {
        return page.hit.path.as_str() == required.source;
    };
    merge.new_survivor_id == Some(page.hit.id)
        && page.frontmatter["dream_sources"]
            .as_array()
            .is_some_and(|sources| {
                sources.iter().any(|source| {
                    source["path"] == required.source && source["id"] == json!(original)
                })
            })
}

pub(super) fn negative_controls(
    task: &Task,
    run: &QueryRun,
    original: &Snapshot,
    merge: &DreamMerge,
) -> Result<Controls> {
    let requirement = task
        .requires
        .first()
        .context("control task has no literal")?;
    let index = run
        .pages
        .iter()
        .position(|page| Some(page.hit.id) == merge.new_survivor_id)
        .context("control task did not retrieve the survivor")?;

    // Mutate only the observed query context. Both controls use the same
    // scorer as the real arms and must leave the peer's evidence supported.
    let mut pages = run.pages.clone();
    pages[index].body = pages[index].body.replacen(&requirement.literal, "", 1);
    let lost_literal = score(task, &pages, original, Some(merge)).missing;
    ensure!(
        lost_literal == [requirement.clone()],
        "literal-loss control escaped scoring"
    );

    pages[index].clone_from(&run.pages[index]);
    let entries = pages[index].frontmatter["dream_sources"]
        .as_array_mut()
        .context("candidate context has no Dream sources")?;
    entries.retain(|entry| entry["path"] != requirement.source);
    let lost_source = score(task, &pages, original, Some(merge)).missing;
    let expected: Vec<_> = task
        .requires
        .iter()
        .filter(|req| req.source == requirement.source)
        .cloned()
        .collect();
    ensure!(
        lost_source == expected,
        "source-loss control escaped scoring"
    );
    Ok(Controls {
        task: task.id.clone(),
        lost_literal,
        lost_source,
    })
}
