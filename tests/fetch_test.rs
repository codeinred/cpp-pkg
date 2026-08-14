//! Unit tests for `cppkg::fetch`, run against local fixtures only (no
//! network): tempdir git repos exercise tag resolution / checkout / the
//! submodule error, and locally-built archives exercise the url path's
//! post-download processing (`fetch_url_bytes` is factored out precisely so
//! these can inject bytes).
//!
//! These live in an integration-test target (not inline `#[cfg(test)]`) so
//! they compile against the library as built, independent of sibling
//! modules' inline test code.

use std::fs;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use cppkg::fetch::{
    archive_kind, extract_archive, fetch_url_bytes, git_materialize, resolve_git_tag,
    validated_commit, verify_sha256, ArchiveKind,
};

fn git_fixture(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args([
            "-c",
            "user.name=cppkg-test",
            "-c",
            "user.email=cppkg-test@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args);
    let out = cmd.output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Local repo with one commit, an annotated tag v1.2.3, and a lightweight
/// tag `lw`. Local-path transport allows fetch-by-sha regardless of
/// upload-pack config, so the shallow path is what these fixtures exercise;
/// the full-ref-fetch fallback is covered by the unfetchable-commit test.
fn fixture_repo() -> (tempfile::TempDir, String) {
    let td = tempfile::tempdir().unwrap();
    let d = td.path();
    git_fixture(d, &["init", "-q"]);
    fs::write(d.join("hello.txt"), "hello\n").unwrap();
    fs::create_dir(d.join("src")).unwrap();
    fs::write(d.join("src/lib.cpp"), "int f();\n").unwrap();
    git_fixture(d, &["add", "."]);
    git_fixture(d, &["commit", "-qm", "init"]);
    git_fixture(d, &["tag", "-a", "v1.2.3", "-m", "release"]);
    git_fixture(d, &["tag", "lw"]);
    let commit = git_fixture(d, &["rev-parse", "HEAD"]).trim().to_string();
    (td, commit)
}

#[test]
fn fetch_resolve_annotated_tag_peels_to_commit() {
    let (repo, commit) = fixture_repo();
    let url = repo.path().to_str().unwrap();
    let tag_object = git_fixture(repo.path(), &["rev-parse", "v1.2.3"]).trim().to_string();
    assert_ne!(tag_object, commit, "annotated tag object must differ from commit");
    let resolved = resolve_git_tag(url, "v1.2.3").unwrap();
    assert_eq!(resolved, commit);
}

#[test]
fn fetch_resolve_lightweight_tag() {
    let (repo, commit) = fixture_repo();
    let url = repo.path().to_str().unwrap();
    assert_eq!(resolve_git_tag(url, "lw").unwrap(), commit);
}

#[test]
fn fetch_resolve_missing_tag_errors() {
    let (repo, _) = fixture_repo();
    let url = repo.path().to_str().unwrap();
    let err = resolve_git_tag(url, "nope").unwrap_err().to_string();
    assert!(err.contains("nope"), "error should name the tag: {err}");
}

#[test]
fn fetch_git_materialize_checkout() {
    let (repo, commit) = fixture_repo();
    let url = repo.path().to_str().unwrap();
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("checkout");
    git_materialize("fmt", url, &commit, &dest).unwrap();
    assert!(dest.join("hello.txt").exists());
    assert!(dest.join("src/lib.cpp").exists());
    assert!(!dest.join(".git").exists(), ".git must be removed");
}

#[test]
fn fetch_git_submodules_hard_error() {
    let (repo, _) = fixture_repo();
    // A .gitmodules file is the detection signal, gitlinks or not.
    fs::write(
        repo.path().join(".gitmodules"),
        "[submodule \"x\"]\n\tpath = x\n\turl = https://example.invalid/x\n",
    )
    .unwrap();
    git_fixture(repo.path(), &["add", ".gitmodules"]);
    git_fixture(repo.path(), &["commit", "-qm", "add submodule file"]);
    let commit = git_fixture(repo.path(), &["rev-parse", "HEAD"]).trim().to_string();
    let url = repo.path().to_str().unwrap();
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("checkout");
    let err = git_materialize("spdlog", url, &commit, &dest).unwrap_err().to_string();
    assert!(err.contains("spdlog"), "error must name the dep: {err}");
    assert!(err.contains("submodule"), "error must mention submodules: {err}");
}

#[test]
fn fetch_git_gitlink_without_gitmodules_hard_error() {
    let (repo, _) = fixture_repo();
    // Register a gitlink (mode 160000) directly, with no .gitmodules — the
    // shape left behind when a superproject deletes .gitmodules but keeps
    // the committed submodule pointer. The pointed-to commit need not exist
    // in this repository (submodule objects live in their own repo).
    git_fixture(
        repo.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,vendored",
        ],
    );
    git_fixture(repo.path(), &["commit", "-qm", "add gitlink"]);
    let commit = git_fixture(repo.path(), &["rev-parse", "HEAD"]).trim().to_string();
    let url = repo.path().to_str().unwrap();
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("checkout");
    let err = git_materialize("curl", url, &commit, &dest).unwrap_err().to_string();
    assert!(err.contains("curl"), "error must name the dep: {err}");
    assert!(err.contains("submodule"), "error must mention submodules: {err}");
}

#[test]
fn fetch_git_unfetchable_commit_errors() {
    let (repo, _) = fixture_repo();
    let url = repo.path().to_str().unwrap();
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("checkout");
    // Nonexistent sha: exercises the whole shallow -> direct -> full-ref
    // fallback chain, all of which must fail.
    let bogus = "a".repeat(40);
    assert!(git_materialize("fmt", url, &bogus, &dest).is_err());
}

#[test]
fn fetch_validated_commit_rejects_abbreviated() {
    assert!(validated_commit("fmt", "rev", "abc123").is_err());
    assert!(validated_commit("fmt", "rev", &"g".repeat(40)).is_err());
    let ok = validated_commit("fmt", "rev", &"AB".repeat(20)).unwrap();
    assert_eq!(ok, "ab".repeat(20));
}

#[test]
fn fetch_verify_sha256_ok_and_mismatch() {
    let bytes = b"archive bytes";
    let good = hex::encode(Sha256::digest(bytes));
    verify_sha256("zlib", "https://x/z.tar.gz", bytes, &good).unwrap();
    // Case-insensitive on the declared side.
    verify_sha256("zlib", "https://x/z.tar.gz", bytes, &good.to_uppercase()).unwrap();
    let err = verify_sha256("zlib", "https://x/z.tar.gz", bytes, &"0".repeat(64))
        .unwrap_err()
        .to_string();
    assert!(err.contains("zlib") && err.contains("mismatch"), "{err}");
}

#[test]
fn fetch_archive_kind_detection() {
    assert_eq!(archive_kind("https://x/a-1.0.tar.gz?dl=1").unwrap(), ArchiveKind::TarGz);
    assert_eq!(archive_kind("https://x/a.tgz").unwrap(), ArchiveKind::TarGz);
    assert_eq!(archive_kind("https://x/a.zip#frag").unwrap(), ArchiveKind::Zip);
    assert!(archive_kind("https://x/a.tar.xz").is_err());
}

fn run_tool(cmd: &mut Command) {
    let out = cmd.output().expect("spawn archive tool");
    assert!(
        out.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn fetch_extract_targz_strips_single_topdir() {
    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("pkg-1.0");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), "a").unwrap();
    fs::write(src.join("sub/b.txt"), "b").unwrap();
    let mut tar = Command::new("tar");
    tar.arg("-czf")
        .arg(td.path().join("ar.tar.gz"))
        .arg("-C")
        .arg(td.path())
        .arg("pkg-1.0");
    run_tool(&mut tar);
    let bytes = fs::read(td.path().join("ar.tar.gz")).unwrap();

    let dest = td.path().join("out");
    extract_archive("pkg", "https://x/pkg-1.0.tar.gz", &bytes, &dest).unwrap();
    assert!(dest.join("a.txt").exists(), "top-level dir must be stripped");
    assert!(dest.join("sub/b.txt").exists());
    assert!(!dest.join("pkg-1.0").exists());
}

#[test]
fn fetch_extract_zip_multiple_toplevel_no_strip() {
    let td = tempfile::tempdir().unwrap();
    let work = td.path().join("work");
    fs::create_dir_all(work.join("bdir")).unwrap();
    fs::write(work.join("a.txt"), "a").unwrap();
    fs::write(work.join("bdir/c.txt"), "c").unwrap();
    let mut zip = Command::new("zip");
    zip.current_dir(&work).args(["-qr", "../ar.zip", "a.txt", "bdir"]);
    run_tool(&mut zip);
    let bytes = fs::read(td.path().join("ar.zip")).unwrap();

    let dest = td.path().join("out");
    extract_archive("pkg", "https://x/pkg.zip", &bytes, &dest).unwrap();
    assert!(dest.join("a.txt").exists());
    assert!(dest.join("bdir/c.txt").exists());
}

#[test]
fn fetch_extract_unsupported_extension_errors() {
    let dest = tempfile::tempdir().unwrap().path().join("out");
    let err = extract_archive("pkg", "https://x/a.tar.xz", b"junk", &dest)
        .unwrap_err()
        .to_string();
    assert!(err.contains("pkg"), "{err}");
}

#[test]
fn fetch_url_bytes_connection_error() {
    // Port 9 (discard) is closed on dev machines: fast connection refusal.
    assert!(fetch_url_bytes("http://127.0.0.1:9/x.tar.gz").is_err());
}
