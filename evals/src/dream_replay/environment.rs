use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use ai_memory_consolidate::dream::{DreamCancel, DreamConfig, DreamReport, run_dream_pass};
use ai_memory_consolidate::sweep::EmbeddingCoord;
use ai_memory_core::{
    ActorContext, NewUser, PageId, PagePath, ProjectId, Tier, UserRole, WorkspaceId,
};
use ai_memory_llm::{Embedder, SyntheticEmbedder};
use ai_memory_store::{DecayParams, PageVersionBody, Store};
use ai_memory_wiki::{Markdown, Wiki, WritePageRequest};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use sha2::Sha256;

use super::{Fixture, sha256};

pub(super) const SEED_TIMESTAMP: &str = "2025-05-01T00:00:00Z";
const FIXTURE_CAP: usize = 8;

pub(super) struct Environment {
    pub store: Store,
    pub wiki: Wiki,
    pub ws: WorkspaceId,
    pub project: ProjectId,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub heads: BTreeMap<String, Head>,
    pub checkpoints: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Head {
    pub id: PageId,
    pub markdown: Markdown,
    pub file_sha256: String,
    pub eligibility_sha256: String,
    pub updated_at_us: i64,
    pub history: Vec<Version>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Version {
    pub id: PageId,
    pub supersedes: Option<PageId>,
    pub author: Option<String>,
    pub is_latest: bool,
}

impl Environment {
    pub async fn pair(root: &Path, fixture: &Fixture) -> Result<(Self, Self, EmbeddingCoord)> {
        let baseline_dir = root.join("baseline");
        let candidate_dir = root.join("candidate");
        let store = Store::open(&baseline_dir)?;
        let ws = store.writer.get_or_create_workspace("dream-replay").await?;
        let project = store
            .writer
            .get_or_create_project(ws, "fixture", None)
            .await?;
        let embedder = Arc::new(SyntheticEmbedder::new(256));
        let coord = EmbeddingCoord {
            provider: embedder.provider().into(),
            model: embedder.model().into(),
            dim: embedder.dim(),
        };
        let wiki = Self::wiki(&baseline_dir, &store)?.with_embedder(embedder);
        for page in &fixture.pages {
            let mut author = NewUser {
                username: page.author.clone(),
                name: None,
                email: None,
            };
            author.validate()?;
            let author_id = store
                .writer
                .create_human_user(author, UserRole::User, None, false)
                .await?;
            wiki.write_page(WritePageRequest {
                workspace_id: ws,
                project_id: project,
                path: PagePath::new(&page.path)?,
                frontmatter: json!({"title": page.title}),
                body: page.body.clone(),
                tier: Tier::Episodic,
                pinned: false,
                title: None,
                admission_ctx: None,
                author_id: Some(author_id),
                actor: ActorContext {
                    user: Some(page.author.clone()),
                    ..ActorContext::anonymous()
                },
                evidence: Vec::new(),
            })
            .await?;
        }

        let db_path = store.db_path().to_owned();
        store.close().await;
        // The normal write API dates new pages at now. Age only the fixture
        // rows with the writer stopped, before either arm of the replay opens.
        let updated_at = SEED_TIMESTAMP.parse::<jiff::Timestamp>()?.as_microsecond();
        let changed = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .execute(
                "UPDATE pages SET created_at=?1, updated_at=?1, valid_from=?1 \
                 WHERE workspace_id=?2 AND project_id=?3 AND is_latest=1",
                params![updated_at, ws.as_bytes(), project.as_bytes()],
            )?;
        ensure!(
            changed == fixture.pages.len(),
            "unexpected fixture page count during aging"
        );

        let baseline = Self::open_snapshot(&baseline_dir, ws, project)?;
        let candidate_db = candidate_dir.join(db_path.strip_prefix(&baseline_dir)?);
        fs::create_dir_all(
            candidate_db
                .parent()
                .context("fixture database directory")?,
        )?;
        baseline.store.reader.snapshot_to(candidate_db).await?;
        copy_seeded_wiki(wiki.root(), &candidate_dir.join("wiki"))?;
        let candidate = Self::open_snapshot(&candidate_dir, ws, project)?;
        Ok((baseline, candidate, coord))
    }

    fn wiki(root: &Path, store: &Store) -> Result<Wiki> {
        Ok(Wiki::new(root, store.writer.clone())?.with_store_reader(store.reader.clone()))
    }

    fn open_snapshot(root: &Path, ws: WorkspaceId, project: ProjectId) -> Result<Self> {
        let store = Store::open_snapshot(root)?;
        let wiki = Self::wiki(root, &store)?;
        Ok(Self {
            ws,
            project,
            store,
            wiki,
        })
    }

    pub async fn dream(&self, cfg: &DreamConfig, dry_run: bool) -> Result<DreamReport> {
        Ok(run_dream_pass(
            &self.store.reader,
            &self.wiki,
            None,
            self.ws,
            self.project,
            &DecayParams::default(),
            0.0,
            cfg,
            &DreamCancel::new(),
            dry_run,
        )
        .await?)
    }

    pub async fn version(&self, path: &str, id: PageId) -> Result<PageVersionBody> {
        self.store
            .reader
            .page_version_body(self.ws, self.project, path.to_owned(), id)
            .await?
            .with_context(|| format!("cannot resolve {path} at version {id}"))
    }

    pub async fn snapshot(&self) -> Result<Snapshot> {
        let mut heads = BTreeMap::new();
        for head in self
            .store
            .reader
            .dream_heads_bounded(self.ws, self.project, FIXTURE_CAP)
            .await?
        {
            let path = head.candidate.path;
            let id = head.candidate.id;
            let stored = self.version(path.as_str(), id).await?;
            let markdown = self.wiki.read_page(self.ws, self.project, &path)?;
            let strip = ai_memory_core::okf::strip_generated_at;
            ensure!(
                stored.body == markdown.body
                    && strip(&stored.frontmatter) == strip(&markdown.frontmatter),
                "wiki and index differ for {path}"
            );
            let history = self
                .store
                .reader
                .page_versions(self.ws, self.project, path.to_string(), FIXTURE_CAP, 0)
                .await?;
            ensure!(
                history.total == history.versions.len() as u64,
                "truncated history for {path}"
            );
            let file = fs::read(self.wiki.abs_path(self.ws, self.project, &path))?;
            heads.insert(
                path.to_string(),
                Head {
                    id,
                    markdown,
                    file_sha256: sha256(file),
                    eligibility_sha256: format!(
                        "{:x}",
                        sha2::digest::Output::<Sha256>::from(head.eligibility_sha256)
                    ),
                    updated_at_us: head.candidate.updated_at_us,
                    history: history
                        .versions
                        .into_iter()
                        .map(|version| Version {
                            id: version.id,
                            supersedes: version.supersedes,
                            author: version.author.map(|author| author.username),
                            is_latest: version.is_latest,
                        })
                        .collect(),
                },
            );
        }
        let checkpoints = self
            .wiki
            .recent_checkpoints(FIXTURE_CAP)?
            .into_iter()
            .map(|c| c.oid)
            .collect();
        Ok(Snapshot { heads, checkpoints })
    }
}

fn copy_seeded_wiki(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let destination = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_seeded_wiki(&entry.path(), &destination)?;
        } else {
            ensure!(
                kind.is_file(),
                "unexpected entry in the seeded wiki: {:?}",
                entry.path()
            );
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}
