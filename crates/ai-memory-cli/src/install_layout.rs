//! Release and install path basenames shared by packaging-aware CLI commands.
//!
//! These names match the shipped binary (`[[bin]] name`), release archive
//! entries, and the binary-sibling hooks bundle. They are not domain
//! constants — keep them in the CLI crate, not `ai-memory-core`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Shipped binary basename (`[[bin]] name`, `/proc/*/comm`).
///
/// Release archives use [`shipped_binary_name`] instead — Windows zips ship
/// `ai-memory.exe`.
pub const BINARY_NAME: &str = "ai-memory";

/// Sibling hooks bundle dir in release archives and install prefixes.
pub const HOOKS_DIR_NAME: &str = "hooks";

/// Walk up from `start` (inclusive) until an ancestor contains a `hooks/`
/// directory. Used for cwd / `CARGO_MANIFEST_DIR` probes when the binary
/// itself is not under the checkout.
pub fn repo_root_from_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(HOOKS_DIR_NAME).is_dir() {
            return Some(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => return None,
        }
    }
}

/// Walk up from the binary's directory until an ancestor contains a `hooks/`
/// directory. Replaces the old fixed `parent×3` guess, which broke under
/// `target/<triple>/<profile>/` and when `CARGO_TARGET_DIR` points outside the
/// checkout (the walk then correctly returns `None`; callers fall through to
/// cwd / manifest probes and the binary-sibling candidate).
pub fn repo_root_from_exe(exe: &Path) -> Option<PathBuf> {
    repo_root_from_dir(exe.parent()?)
}

/// Compile-time crate directory for this CLI package.
///
/// Present for any binary built by Cargo (including release CI builds). The
/// path only resolves on the machine that compiled it — callers must probe
/// with [`repo_root_from_dir`] and skip when absent, so installed release
/// binaries are unaffected.
pub fn cargo_manifest_dir() -> Option<&'static Path> {
    option_env!("CARGO_MANIFEST_DIR").map(Path::new)
}

/// Dev-checkout root for hook discovery when the binary may live outside the
/// tree (`CARGO_TARGET_DIR`, cross-compile `target/<triple>/…`).
///
/// Order: walk from the executable, then from `cwd`, then from the compile-time
/// `CARGO_MANIFEST_DIR`. The first ancestor that contains `hooks/` wins.
pub fn repo_root_guess(exe: Option<&Path>, cwd: Option<&Path>) -> Option<PathBuf> {
    if let Some(exe) = exe {
        if let Some(root) = repo_root_from_exe(exe) {
            return Some(root);
        }
    }
    if let Some(cwd) = cwd {
        if let Some(root) = repo_root_from_dir(cwd) {
            return Some(root);
        }
    }
    if let Some(manifest) = cargo_manifest_dir() {
        if let Some(root) = repo_root_from_dir(manifest) {
            return Some(root);
        }
    }
    None
}

/// On-disk / archive basename for the release binary on this host.
///
/// Matches `release.yml`: Unix tarballs ship `ai-memory`; the Windows zip
/// ships `ai-memory.exe`.
pub fn shipped_binary_name() -> &'static str {
    #[cfg(windows)]
    {
        "ai-memory.exe"
    }
    #[cfg(not(windows))]
    {
        BINARY_NAME
    }
}

/// Scoop's per-app directory name for ai-memory (`scoop install ai-memory`).
pub const SCOOP_APP_NAME: &str = "ai-memory";

/// Scoop's version-independent directory name: `<root>\apps\<app>\current` is
/// a junction to the active `<root>\apps\<app>\<version>`, and the shims on
/// `PATH` run the binary through it.
const SCOOP_CURRENT_DIR: &str = "current";

/// The `current` links for a file inside a versioned Scoop app directory,
/// `<root>/apps/<app>/<version>/<rest>` → `<root>/apps/<app>/current/<rest>`.
///
/// `std::fs::canonicalize` resolves the `current` junction, so a path recorded
/// from it names the version that `scoop update` and `scoop cleanup` remove.
/// The match is lexical, one candidate per `apps` component, and callers
/// confirm a candidate exists before using it. A path already under `current`
/// yields nothing.
pub(crate) fn scoop_current_links(path: &Path) -> Vec<PathBuf> {
    let parts: Vec<_> = path.components().map(|part| part.as_os_str()).collect();
    scoop_app_indexes(&parts)
        .filter(|&index| !eq_ignore_case(parts[index + 2], SCOOP_CURRENT_DIR))
        .map(|index| {
            let mut link: PathBuf = parts[..index + 2].iter().collect();
            link.push(SCOOP_CURRENT_DIR);
            link.extend(&parts[index + 3..]);
            link
        })
        .collect()
}

/// Whether `path` names a file inside Scoop's `<root>/apps/<app>/<dir>/` for
/// `app`, through either a version directory or the `current` link.
pub(crate) fn is_scoop_app_path(path: &Path, app: &str) -> bool {
    let parts: Vec<_> = path.components().map(|part| part.as_os_str()).collect();
    scoop_app_indexes(&parts).any(|index| eq_ignore_case(parts[index + 1], app))
}

/// Indexes of `apps` components followed by `<app>/<dir>/<file...>`.
fn scoop_app_indexes<'a>(parts: &'a [&'a OsStr]) -> impl Iterator<Item = usize> + 'a {
    parts
        .iter()
        .enumerate()
        .filter(move |(index, part)| eq_ignore_case(part, "apps") && parts.len() > index + 3)
        .map(|(index, _)| index)
}

/// Windows paths, where Scoop lives, compare without case.
fn eq_ignore_case(part: &OsStr, name: &str) -> bool {
    part.to_str()
        .is_some_and(|part| part.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(parts: &[&str]) -> PathBuf {
        parts.iter().collect()
    }

    #[test]
    fn scoop_version_dir_maps_to_current_link() {
        let root = path(&["/", "Users", "me", "scoop"]);
        let versioned = root.join("apps/ai-memory/3.4.0/ai-memory.exe");
        assert_eq!(
            scoop_current_links(&versioned),
            vec![root.join("apps/ai-memory/current/ai-memory.exe")]
        );
        let nested = root.join("apps/ai-memory/3.4.0/bin/ai-memory.exe");
        assert_eq!(
            scoop_current_links(&nested),
            vec![root.join("apps/ai-memory/current/bin/ai-memory.exe")]
        );
    }

    #[test]
    fn scoop_current_link_and_unrelated_paths_have_no_links() {
        for unrelated in [
            "/Users/me/scoop/apps/ai-memory/current/ai-memory.exe",
            "/Users/me/scoop/apps/ai-memory/CURRENT/ai-memory.exe",
            "/Users/me/.local/bin/ai-memory",
            // `apps/<app>/<file>` has no version directory.
            "/Users/me/scoop/apps/ai-memory/ai-memory.exe",
            "/usr/local/bin/ai-memory",
        ] {
            assert!(
                scoop_current_links(Path::new(unrelated)).is_empty(),
                "{unrelated}"
            );
        }
    }

    #[test]
    fn scoop_match_ignores_case_like_windows() {
        let versioned = Path::new("/Users/me/Scoop/Apps/AI-Memory/3.4.0/ai-memory.exe");
        assert_eq!(
            scoop_current_links(versioned),
            vec![PathBuf::from(
                "/Users/me/Scoop/Apps/AI-Memory/current/ai-memory.exe"
            )]
        );
        assert!(is_scoop_app_path(versioned, SCOOP_APP_NAME));
    }

    #[test]
    fn scoop_app_path_names_only_the_given_app() {
        for scoop in [
            "/Users/me/scoop/apps/ai-memory/3.4.0/ai-memory.exe",
            "/Users/me/scoop/apps/ai-memory/current/ai-memory.exe",
            "/ProgramData/scoop/apps/ai-memory/3.4.0/ai-memory.exe",
        ] {
            assert!(
                is_scoop_app_path(Path::new(scoop), SCOOP_APP_NAME),
                "{scoop}"
            );
        }
        for other in [
            "/Users/me/scoop/apps/other-tool/1.0/ai-memory.exe",
            "/Users/me/scoop/apps/ai-memory/ai-memory.exe",
            "/Users/me/.local/bin/ai-memory",
        ] {
            assert!(
                !is_scoop_app_path(Path::new(other), SCOOP_APP_NAME),
                "{other}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn scoop_links_keep_a_windows_drive_prefix() {
        let versioned = Path::new(r"C:\Users\me\scoop\apps\ai-memory\3.4.0\ai-memory.exe");
        assert_eq!(
            scoop_current_links(versioned),
            vec![PathBuf::from(
                r"C:\Users\me\scoop\apps\ai-memory\current\ai-memory.exe"
            )]
        );
        let verbatim = Path::new(r"\\?\C:\Users\me\scoop\apps\ai-memory\3.4.0\ai-memory.exe");
        assert_eq!(
            scoop_current_links(verbatim),
            vec![PathBuf::from(
                r"\\?\C:\Users\me\scoop\apps\ai-memory\current\ai-memory.exe"
            )]
        );
        assert!(is_scoop_app_path(verbatim, SCOOP_APP_NAME));
    }
}

#[cfg(test)]
mod hook_layout_tests {
    use super::*;
    use std::fs;

    #[test]
    fn repo_root_from_exe_finds_hooks_under_cargo_target_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let hooks = repo.join(HOOKS_DIR_NAME);
        let exe = repo.join("target").join("debug").join("ai-memory");
        fs::create_dir_all(hooks.join("claude-code")).unwrap();
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();

        assert_eq!(repo_root_from_exe(&exe), Some(repo));
    }

    #[test]
    fn repo_root_from_exe_finds_hooks_under_target_triple_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let hooks = repo.join(HOOKS_DIR_NAME);
        let exe = repo
            .join("target")
            .join("x86_64-unknown-linux-gnu")
            .join("release")
            .join("ai-memory");
        fs::create_dir_all(hooks.join("claude-code")).unwrap();
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();

        assert_eq!(repo_root_from_exe(&exe), Some(repo));
    }

    #[test]
    fn repo_root_from_exe_returns_none_when_target_outside_tree() {
        let tmp = tempfile::tempdir().unwrap();
        // Simulate CARGO_TARGET_DIR outside the checkout: binary under
        // an external target dir with no hooks/ ancestor.
        let exe = tmp
            .path()
            .join("cargo-target")
            .join("debug")
            .join("ai-memory");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();

        assert_eq!(repo_root_from_exe(&exe), None);
    }

    #[test]
    fn repo_root_from_exe_finds_hooks_beside_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let release = tmp.path().join("ai-memory-linux-x86_64");
        let hooks = release.join(HOOKS_DIR_NAME);
        let exe = release.join("ai-memory");
        fs::create_dir_all(hooks.join("claude-code")).unwrap();
        fs::write(&exe, b"").unwrap();

        assert_eq!(repo_root_from_exe(&exe), Some(release));
    }

    #[test]
    fn repo_root_guess_uses_cwd_when_exe_is_outside_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let nested = repo.join("crates").join("ai-memory-cli");
        let hooks = repo.join(HOOKS_DIR_NAME);
        let exe = tmp
            .path()
            .join("cargo-target")
            .join("debug")
            .join("ai-memory");
        fs::create_dir_all(hooks.join("claude-code")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();

        assert_eq!(repo_root_guess(Some(&exe), Some(&nested)), Some(repo));
    }

    #[test]
    fn repo_root_guess_falls_back_to_cargo_manifest_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp
            .path()
            .join("cargo-target")
            .join("debug")
            .join("ai-memory");
        let foreign_cwd = tmp.path().join("foreign-home");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::create_dir_all(&foreign_cwd).unwrap();
        fs::write(&exe, b"").unwrap();

        let root = repo_root_guess(Some(&exe), Some(&foreign_cwd))
            .expect("compile-time CARGO_MANIFEST_DIR must reach the checkout hooks/");
        assert!(
            root.join(HOOKS_DIR_NAME).is_dir(),
            "manifest fallback must land on a real hooks/ ancestor: {root:?}"
        );
    }
}
