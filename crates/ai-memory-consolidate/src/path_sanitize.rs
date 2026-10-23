//! Shared model-path sanitization for LLM-produced wiki paths.
//!
//! Bootstrap (#847), per-session consolidation (#848), and auto-improve
//! review all accept a page path straight from LLM structured output.
//! Bootstrap and consolidation hand it to `Wiki::apply_batch`, which is
//! atomic: a single path that fails `PagePath::ensure_portable` at write
//! time aborts every page in that batch, not just its own. Auto-improve
//! stages the path and only hits `ensure_portable` on approve, so a bad
//! path becomes an unapplyable proposal. `PagePath::new` is deliberately
//! tolerant (see its doc comment) and does not catch this, so callers must
//! sanitize the raw model path themselves before constructing a `PagePath`.

/// Filename characters Windows refuses, mirroring
/// `ai_memory_core::ids`'s reserved-char set, plus `\` — `PagePath::new`
/// already rejects a literal backslash anywhere in the raw path (it reads as
/// a separator), so a component containing one must be cleaned before
/// `PagePath::new` ever sees it, not after.
pub(crate) const PATH_ILLEGAL_CHARS: &[char] = &['<', '>', ':', '"', '|', '?', '*', '\\'];

/// Clean a model-produced page path so it survives `PagePath::new` and
/// `ensure_portable`.
///
/// The LLM sometimes echoes free text — a conventional-commit subject like
/// `build(sandbox): orchestrate` — straight into a page path. That passes
/// `PagePath::new` (deliberately tolerant; see its doc comment) but fails
/// `ensure_portable`, which `Wiki::apply_batch` enforces atomically: one bad
/// path there aborts every page in the batch, not just its own (#847, #848).
/// Auto-improve review stages the path and hits the same check on approve.
/// Replace every Windows-illegal character and ASCII control byte in each
/// `/`-separated component with `-`, keeping the `dir/subdir/name.md` shape
/// intact so the model's intended layout survives.
///
/// Dot segments (`.` / `..`), empty segments from `//` or a leading `/`,
/// trailing dots or spaces, DOS device stems (`CON.md`, `aux.md`), and
/// Git-reserved directory names (`.git`, `git~1`) abort the same batch.
/// Those are rewritten in place rather than dropped, so two different model
/// paths cannot collapse onto one page. An empty path and a trailing slash
/// (`decisions/`) stay unchanged: there is no filename to invent, and the
/// caller still reports `invalid_path`.
///
/// The model also omits the `.md` extension on multi-page paths (`decisions/
/// smart-model-luna` instead of `.../smart-model-luna.md`, #885). The rule
/// branch in `build_update` always appends `.md` itself, but the non-rule
/// branch — and bootstrap and auto-improve — route straight through this
/// helper, so a bare path would be stored extensionless and read back as a
/// non-portable wiki page. Append `.md` to the final component when it lacks
/// it (case-insensitive), which is idempotent: a path already ending in `.md`
/// is left untouched. Reserved-name checks run after that append, because
/// `CON` stays a device as `CON.md`, while `git~1` stops being reserved once
/// it is `git~1.md`.
pub(crate) fn slugify_page_path(raw: &str) -> String {
    let parts: Vec<&str> = raw.split('/').collect();
    let last_index = parts.len() - 1;
    let mut segments = Vec::with_capacity(parts.len());
    for (index, segment) in parts.iter().enumerate() {
        let mut cleaned: String = segment
            .chars()
            .map(|c| {
                if PATH_ILLEGAL_CHARS.contains(&c) || (c as u32) < 0x20 {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        let is_last = index == last_index;
        if cleaned.is_empty() {
            // Keep only a trailing empty component (`decisions/`) so upstream
            // still rejects a missing filename. Drop leading and middle
            // empties (`/abs.md`, `a//b.md`), which `PagePath::new` rejects.
            if is_last {
                segments.push(String::new());
            }
            continue;
        }
        if cleaned == "." || cleaned == ".." {
            cleaned = if cleaned.len() == 1 {
                "dot".to_owned()
            } else {
                "dotdot".to_owned()
            };
        } else {
            let trimmed = cleaned.trim_end_matches(['.', ' ']);
            cleaned = if trimmed.is_empty() {
                "blank".to_owned()
            } else {
                trimmed.to_owned()
            };
        }
        // Append `.md` before the reserved-name check (#885). `NAME.MD` is
        // recognized case-insensitively, so the append stays idempotent.
        if is_last && !cleaned.to_ascii_lowercase().ends_with(".md") {
            cleaned.push_str(".md");
        }
        segments.push(neutralize_reserved_component(&cleaned));
    }
    segments.join("/")
}

/// Prefix a component Windows or Git will refuse. `PagePath::ensure_portable`
/// matches DOS names on the stem (`CON.md`) and Git names on the whole
/// component (`.git`, exactly `git~1`..=`git~4`).
fn neutralize_reserved_component(component: &str) -> String {
    let stem = component.split('.').next().unwrap_or(component);
    if ai_memory_core::is_dos_device_name(stem)
        || ai_memory_core::is_git_reserved_component(component)
    {
        format!("file-{component}")
    } else {
        component.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::slugify_page_path;

    #[test]
    fn slugify_page_path_replaces_illegal_chars_and_keeps_slashes() {
        assert_eq!(
            slugify_page_path("concepts/build(sandbox): orchestrate the run.md"),
            "concepts/build(sandbox)- orchestrate the run.md"
        );
        assert_eq!(
            slugify_page_path("a/b<c>d:e\"f|g?h*i\\j.md"),
            "a/b-c-d-e-f-g-h-i-j.md"
        );
        assert_eq!(
            slugify_page_path("concepts/clean-path.md"),
            "concepts/clean-path.md",
            "an already-portable path must be left unchanged"
        );
    }

    #[test]
    fn slugify_page_path_appends_md_when_the_model_omits_it() {
        // #885: multi-page consolidation echoes a bare model path with no
        // extension; the helper must land it as a portable `.md` page.
        let slug = slugify_page_path("decisions/smart-model-luna");
        assert!(
            slug.ends_with("-luna.md"),
            "bare model path must gain a .md extension, got {slug:?}"
        );
        assert_eq!(slug, "decisions/smart-model-luna.md");
    }

    #[test]
    fn slugify_page_path_extension_append_is_idempotent() {
        // A path already ending in `.md` (any casing) keeps its single
        // extension — the helper never double-extends.
        assert_eq!(
            slugify_page_path("decisions/smart-model-luna.md"),
            "decisions/smart-model-luna.md"
        );
        assert_eq!(
            slugify_page_path("decisions/SHOUTING.MD"),
            "decisions/SHOUTING.MD",
            "an existing .MD extension is recognized case-insensitively"
        );
    }

    #[test]
    fn slugify_page_path_does_not_fabricate_a_filename_from_an_empty_path() {
        // #885 regression: appending `.md` to an empty final component would
        // turn a missing/invalid path into `.md` (or `dir/.md`), which slips
        // past a caller's own empty-path rejection. An empty path stays empty,
        // and a trailing slash keeps its empty final component, so the missing
        // filename is still rejected upstream (auto-improve `invalid_path`).
        assert_eq!(slugify_page_path(""), "");
        assert_eq!(slugify_page_path("decisions/"), "decisions/");
    }

    /// These shapes pass `PagePath::new`'s tolerant constructor and then fail
    /// `ensure_portable` (or fail `new` on `.` / `..` / empty segments). One
    /// of them aborts `Wiki::apply_batch` for every other page.
    #[test]
    fn slugify_page_path_neutralizes_segments_that_abort_the_batch() {
        use ai_memory_core::PagePath;

        let cases = [
            ("../secrets.md", "dotdot/secrets.md"),
            ("a//b.md", "a/b.md"),
            ("/leading.md", "leading.md"),
            ("notes/trailing./x.md", "notes/trailing/x.md"),
            ("notes/trailing /x.md", "notes/trailing/x.md"),
            ("notes/end.md.", "notes/end.md"),
            ("CON.md", "file-CON.md"),
            ("notes/aux.md", "notes/file-aux.md"),
            ("notes/LPT9.txt", "notes/file-LPT9.txt.md"),
            ("notes/.git/sub.md", "notes/file-.git/sub.md"),
            ("notes/.GIT/sub.md", "notes/file-.GIT/sub.md"),
            ("notes/git~1/foo.md", "notes/file-git~1/foo.md"),
            ("notes/git~1", "notes/git~1.md"),
        ];
        for (raw, expect) in cases {
            let slug = slugify_page_path(raw);
            assert_eq!(slug, expect, "{raw:?}");
            let path =
                PagePath::new(&slug).unwrap_or_else(|err| panic!("{raw:?} -> {slug:?}: {err}"));
            path.ensure_portable()
                .unwrap_or_else(|err| panic!("{raw:?} -> {slug:?}: {err}"));
        }
        assert_eq!(slugify_page_path(""), "");
        assert_eq!(slugify_page_path("decisions/"), "decisions/");
    }
}
