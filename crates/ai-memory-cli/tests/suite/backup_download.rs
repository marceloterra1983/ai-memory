//! A failed backup download must leave the previous archive intact.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const BIN: &str = env!("CARGO_BIN_EXE_ai-memory");
const PREVIOUS: &[u8] = b"previous synthetic backup\n";

async fn response(body: Vec<u8>, declared_len: usize) -> (String, tokio::task::JoinHandle<()>) {
    response_with_status("200 OK", body, declared_len).await
}

async fn response_with_status(
    status: &str,
    body: Vec<u8>,
    declared_len: usize,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let status = status.to_owned();
    let server = tokio::spawn(async move {
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0 && request.len() + read <= 8192);
            request.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("POST /fixture/admin/backup HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-token\r\n")
        );
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/gzip\r\nContent-Length: {declared_len}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    (format!("http://{address}/fixture"), server)
}

#[tokio::test]
async fn unsuccessful_response_preserves_previous_backup() {
    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    std::fs::write(&dest, PREVIOUS).unwrap();
    let body = b"synthetic server refusal";
    let (url, server) =
        response_with_status("503 Service Unavailable", body.to_vec(), body.len()).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("503"));
    assert_eq!(std::fs::read(&dest).unwrap(), PREVIOUS);
    assert_no_staging_files(&dir);
}

fn configure(command: &mut Command, root: &Path, url: &str, dest: &Path) {
    command
        .env_clear()
        .current_dir(root)
        .env("AI_MEMORY_HOME", root.join("home"))
        .env("AI_MEMORY_DATA_DIR", root.join("data"))
        .env("AI_MEMORY_SERVER_URL", url)
        .env("AI_MEMORY_AUTH_TOKEN", "fixture-token")
        .env("NO_PROXY", "*")
        .arg("--config")
        .arg(root.join("config.toml"))
        .arg("backup")
        .arg("--to")
        .arg(dest);
}

async fn download(root: &Path, url: &str, dest: &Path) -> Output {
    let mut command = Command::new(BIN);
    configure(&mut command, root, url, dest);
    run_command(command).await
}

async fn run_command(command: Command) -> Output {
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .unwrap()
        .unwrap()
}

fn output_dir(root: &Path) -> std::path::PathBuf {
    let dir = root.join("output");
    std::fs::create_dir(&dir).unwrap();
    dir
}

fn assert_no_staging_files(dir: &Path) {
    let names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from("backup.tar.gz")]);
}

#[tokio::test]
async fn interrupted_body_preserves_previous_backup() {
    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    std::fs::write(&dest, PREVIOUS).unwrap();
    let (url, server) = response(b"incomplete replacement".to_vec(), 128).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reading response chunk"));
    assert_eq!(std::fs::read(&dest).unwrap(), PREVIOUS);
    assert_no_staging_files(&dir);
}

#[cfg(unix)]
#[tokio::test]
async fn write_failure_preserves_previous_backup() {
    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    std::fs::write(&dest, PREVIOUS).unwrap();
    let body = vec![b'z'; 16_384];
    let (url, server) = response(body.clone(), body.len()).await;
    // Limit only the child: a real filesystem write returns EFBIG instead of
    // exhausting the machine's disk or changing the test runner's limits.
    let mut command = Command::new("bash");
    command.args([
        "--noprofile",
        "--norc",
        "-c",
        "trap '' XFSZ; ulimit -f 1; exec \"$@\"",
        "backup-write-limit",
        BIN,
    ]);
    configure(&mut command, root.path(), &url, &dest);

    let output = run_command(command).await;
    server.await.unwrap();

    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("writing response chunk") || error.contains("flushing"),
        "{error}"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), PREVIOUS);
    assert_no_staging_files(&dir);
}

#[tokio::test]
async fn complete_body_replaces_previous_backup() {
    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    std::fs::write(&dest, PREVIOUS).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let body = b"complete synthetic replacement";
    let (url, server) = response(body.to_vec(), body.len()).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    assert_no_staging_files(&dir);
}

#[cfg(unix)]
#[tokio::test]
async fn complete_body_creates_private_backup() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    let body = b"new synthetic backup";
    let (url, server) = response(body.to_vec(), body.len()).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(
        std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_no_staging_files(&dir);
}

#[cfg(unix)]
#[tokio::test]
async fn complete_body_follows_existing_symlink() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let target = root.path().join("previous.tar.gz");
    let dest = dir.join("backup.tar.gz");
    std::fs::write(&target, PREVIOUS).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    symlink("../previous.tar.gz", &dest).unwrap();
    let body = b"complete synthetic replacement";
    let (url, server) = response(body.to_vec(), body.len()).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(&dest).unwrap(),
        Path::new("../previous.tar.gz")
    );
    assert_eq!(std::fs::read(&target).unwrap(), body);
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_no_staging_files(&dir);
}

#[cfg(unix)]
#[tokio::test]
async fn hardlinked_destination_is_refused_and_preserved() {
    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    let alias = root.path().join("previous.tar.gz");
    std::fs::write(&dest, PREVIOUS).unwrap();
    std::fs::hard_link(&dest, &alias).unwrap();
    let body = b"complete synthetic replacement";
    let (url, server) = response(body.to_vec(), body.len()).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("hardlink"));
    assert_eq!(std::fs::read(&dest).unwrap(), PREVIOUS);
    assert_eq!(std::fs::read(&alias).unwrap(), PREVIOUS);
    assert_no_staging_files(&dir);
}

#[tokio::test]
async fn interrupted_body_does_not_create_output() {
    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    let (url, server) = response(b"incomplete replacement".to_vec(), 128).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(!output.status.success());
    assert!(!dest.exists());
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn interrupted_body_preserves_symlink_and_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let target = root.path().join("previous.tar.gz");
    let dest = dir.join("backup.tar.gz");
    std::fs::write(&target, PREVIOUS).unwrap();
    symlink("../previous.tar.gz", &dest).unwrap();
    let (url, server) = response(b"incomplete replacement".to_vec(), 128).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_link(&dest).unwrap(),
        Path::new("../previous.tar.gz")
    );
    assert_eq!(std::fs::read(&target).unwrap(), PREVIOUS);
    assert_no_staging_files(&dir);
    assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ai-memory-download-")
    }));
}

#[cfg(unix)]
#[tokio::test]
async fn readonly_destination_is_refused_and_preserved() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let dir = output_dir(root.path());
    let dest = dir.join("backup.tar.gz");
    std::fs::write(&dest, PREVIOUS).unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o440)).unwrap();
    // Elevated test runners can write despite mode bits; exercise the actual
    // access guard only where the OS denies an ordinary writable open.
    if std::fs::OpenOptions::new().write(true).open(&dest).is_ok() {
        return;
    }
    let body = b"complete synthetic replacement";
    let (url, server) = response(body.to_vec(), body.len()).await;

    let output = download(root.path(), &url, &dest).await;
    server.await.unwrap();

    assert!(!output.status.success());
    assert_eq!(std::fs::read(&dest).unwrap(), PREVIOUS);
    assert_eq!(
        std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o440
    );
    assert_no_staging_files(&dir);
}
