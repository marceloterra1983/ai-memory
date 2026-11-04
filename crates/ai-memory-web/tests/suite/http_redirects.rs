//! Evaluate each shipped fetch expression using Node's real HTTP transport.

#[test]
fn web_requests_refuse_redirects_with_credentials_and_private_bodies() {
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("web-redirects.mjs");
    std::fs::write(&fixture, include_str!("../fixtures/web_redirects.mjs")).unwrap();
    let output = std::process::Command::new("node")
        .arg(&fixture)
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates"))
        .output()
        .expect("Node is required to exercise the shipped web requests");
    assert!(
        output.status.success(),
        "web redirect runtime failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
