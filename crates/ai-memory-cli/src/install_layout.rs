//! Release and install path basenames shared by packaging-aware CLI commands.
//!
//! These names match the shipped binary (`[[bin]] name`), release archive
//! entries, and the binary-sibling hooks bundle. They are not domain
//! constants — keep them in the CLI crate, not `ai-memory-core`.

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

#[cfg(test)]
mod tests {
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
        let exe = tmp.path().join("cargo-target").join("debug").join("ai-memory");
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
        let exe = tmp.path().join("cargo-target").join("debug").join("ai-memory");
        fs::create_dir_all(hooks.join("claude-code")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();

        assert_eq!(
            repo_root_guess(Some(&exe), Some(&nested)),
            Some(repo)
        );
    }

    #[test]
    fn repo_root_guess_falls_back_to_cargo_manifest_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("cargo-target").join("debug").join("ai-memory");
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
