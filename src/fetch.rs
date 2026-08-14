//! Dependency acquisition into the raw download store.
//!
//! Contract (CPPKG_TOML.md lockfile section; DESIGN_CHOICES.md):
//! - git + tag: resolve tag -> commit (ls-remote or clone+rev-parse), clone
//!   at that commit into the raw store; the COMMIT SHA is the package id and
//!   integrity reference (verify via rev-parse after checkout). Tags are
//!   mutable; the lockfile pin (commit) wins over re-resolving the tag.
//! - git + rev: fetch that commit directly.
//! - url + sha256: download (ureq), verify user-declared sha256, package id
//!   is "blake3:<hex>" of the archive bytes; extract tar.gz/zip into the raw
//!   store entry (strip a single top-level directory if the archive has one).
//! - SUBMODULES: if the checkout contains a .gitmodules file -> hard error
//!   ("git submodules are not supported in v0"), do not build silently.
//! - Fresh-machine flow: lockfile commit + source URL suffice to re-download.
//!
//! Internal factoring: `ensure` is a thin store-aware shell over pure helpers
//! (`resolve_git_tag`, `git_materialize`, `fetch_url_bytes`, `verify_sha256`,
//! `extract_archive`) that take explicit paths/bytes, so unit tests exercise
//! the network-free logic against local fixtures without a live store.

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};

use crate::lockfile::LockedPackage;
use crate::schema::{DependencySpec, GitRef, SourceSpec};
use crate::store::Stores;
use crate::Result;

#[derive(Debug, Clone)]
pub struct RawPackage {
    /// Directory containing the package source tree (raw store entry).
    pub path: PathBuf,
    /// Commit sha (git) or "blake3:<hex>" (url) — feeds hashing + lockfile.
    pub package_id: String,
}

/// Ensure the dependency's source is present in the raw store; returns the
/// entry. Uses `locked` (if it matches the request) instead of re-resolving
/// mutable refs. Network is touched only when the store lacks the entry.
pub fn ensure(
    stores: &Stores,
    dep_key: &str,
    spec: &DependencySpec,
    locked: Option<&LockedPackage>,
) -> Result<RawPackage> {
    match &spec.source {
        SourceSpec::Git { url, reference } => ensure_git(stores, dep_key, url, reference, locked),
        SourceSpec::Url { url, sha256 } => ensure_url(stores, dep_key, url, sha256, locked),
    }
}

// ---------------------------------------------------------------------------
// git sources

fn ensure_git(
    stores: &Stores,
    dep_key: &str,
    url: &str,
    reference: &GitRef,
    locked: Option<&LockedPackage>,
) -> Result<RawPackage> {
    let commit = match reference {
        // An explicit rev IS the pin; the lock can only agree (the caller
        // invalidates lock entries whose `requested` no longer matches).
        GitRef::Rev(rev) => validated_commit(dep_key, "rev", rev)?,
        GitRef::Tag(tag) => match locked.and_then(|l| l.commit.as_deref()) {
            // Tags are mutable: an existing pin wins over re-resolving.
            Some(pin) => validated_commit(dep_key, "lockfile commit", pin)?,
            None => {
                let resolved = resolve_git_tag(url, tag).with_context(|| {
                    format!("dependency `{dep_key}`: resolving tag `{tag}` at {url}")
                })?;
                validated_commit(dep_key, "commit resolved from tag", &resolved)?
            }
        },
    };

    let entry = stores.raw_dir(dep_key, &commit);
    if stores.entry_complete(&entry) {
        return Ok(RawPackage { path: entry, package_id: commit });
    }

    clear_stale_entry(&entry)?;
    // Materialize in a same-filesystem temp dir and rename into place so a
    // crash never leaves a plausible-looking half-checkout at the entry path
    // (the completion marker is the real guard; this keeps debris tidy too).
    let parent = entry_parent(&entry)?;
    let tmp = fresh_tmp_dir(parent, "git")?;
    let guard = TempGuard(tmp.clone());
    git_materialize(dep_key, url, &commit, &tmp)?;
    fs::rename(&tmp, &entry)
        .with_context(|| format!("moving checkout into store entry {}", entry.display()))?;
    drop(guard);
    stores.mark_complete(&entry)?;
    Ok(RawPackage { path: entry, package_id: commit })
}

// The helpers below are `#[doc(hidden)] pub` rather than private so that
// tests/fetch_test.rs can drive them against local fixtures: they are
// implementation detail, not API — only `ensure` is the module's surface.

/// Full-hex commit ids only: fetch-by-sha requires the complete object name,
/// and prefix matching would weaken the integrity check.
#[doc(hidden)]
pub fn validated_commit(dep_key: &str, what: &str, commit: &str) -> Result<String> {
    let c = commit.trim().to_ascii_lowercase();
    // 40 = SHA-1 repos, 64 = sha256 object-format repos.
    let ok = (c.len() == 40 || c.len() == 64) && c.bytes().all(|b| b.is_ascii_hexdigit());
    if !ok {
        bail!(
            "dependency `{dep_key}`: {what} `{commit}` is not a full commit sha \
             (need 40 hex chars; abbreviated revs cannot be fetched or verified)"
        );
    }
    Ok(c)
}

/// Resolve a tag to its commit via `git ls-remote`, preferring the peeled
/// `<tag>^{{}}` line: for annotated tags the plain line is the tag object,
/// not the commit, and checkout verification needs the commit sha.
#[doc(hidden)]
pub fn resolve_git_tag(url: &str, tag: &str) -> Result<String> {
    let plain_ref = format!("refs/tags/{tag}");
    let peeled_ref = format!("{plain_ref}^{{}}");
    let out = run_git(None, [OsStr::new("ls-remote"), OsStr::new(url), peeled_ref.as_ref(), plain_ref.as_ref()])?;
    let mut plain = None;
    let mut peeled = None;
    for line in out.lines() {
        let mut fields = line.split('\t');
        let (Some(sha), Some(refname)) = (fields.next(), fields.next()) else {
            continue;
        };
        if refname == peeled_ref {
            peeled = Some(sha.to_string());
        } else if refname == plain_ref {
            plain = Some(sha.to_string());
        }
    }
    peeled
        .or(plain)
        .ok_or_else(|| anyhow::anyhow!("remote has no tag `{tag}` (ls-remote returned no matching ref)"))
}

/// Produce a verified checkout of `commit` (without its .git directory) in
/// `workdir`. Tries the cheapest transfer first; servers differ in which
/// direct-sha fetches they allow, so fall back stepwise to a full ref fetch.
#[doc(hidden)]
pub fn git_materialize(dep_key: &str, url: &str, commit: &str, workdir: &Path) -> Result<()> {
    fs::create_dir_all(workdir)
        .with_context(|| format!("creating checkout dir {}", workdir.display()))?;
    run_git(Some(workdir), ["init", "-q"])?;

    let shallow = run_git(Some(workdir), ["fetch", "-q", "--depth", "1", url, commit]);
    let fetched_head = match shallow {
        Ok(_) => true,
        Err(_) => match run_git(Some(workdir), ["fetch", "-q", url, commit]) {
            Ok(_) => true,
            Err(_) => {
                // Server refuses fetch-by-sha entirely: take every branch and
                // tag, then check the commit out from the object store.
                run_git(
                    Some(workdir),
                    [
                        "fetch",
                        "-q",
                        url,
                        "+refs/heads/*:refs/remotes/origin/*",
                        "+refs/tags/*:refs/tags/*",
                    ],
                )
                .with_context(|| {
                    format!(
                        "dependency `{dep_key}`: could not fetch commit {commit} from {url} \
                         (tried shallow fetch, direct rev fetch, and full ref fetch)"
                    )
                })?;
                false
            }
        },
    };

    let target = if fetched_head { "FETCH_HEAD" } else { commit };
    // autocrlf=false: a host global config with autocrlf enabled would
    // rewrite checked-out bytes without failing the commit-sha integrity
    // check (which verifies the commit, not the worktree).
    run_git(
        Some(workdir),
        [
            "-c",
            "advice.detachedHead=false",
            "-c",
            "core.autocrlf=false",
            "checkout",
            "-q",
            target,
        ],
    )?;

    // The commit sha is the content hash (v0 integrity model): verify the
    // worktree really is the pinned commit before anything consumes it.
    let head = run_git(Some(workdir), ["rev-parse", "HEAD"])?.trim().to_ascii_lowercase();
    if head != commit {
        bail!(
            "dependency `{dep_key}`: integrity failure after checkout from {url}: \
             HEAD is {head}, expected {commit}"
        );
    }

    // Submodule check must precede .git removal: with .git gone we could no
    // longer inspect the index, and the error must fire before an incomplete
    // tree can ever look usable. Gitlink entries (mode 160000) are the
    // ground truth — .gitmodules can be absent or live in a subdirectory
    // while a committed gitlink still checks out as an empty directory and
    // builds silently without its vendored code.
    let index = run_git(Some(workdir), ["ls-files", "-s"])?;
    let has_gitlink = index.lines().any(|l| l.starts_with("160000 "));
    if has_gitlink || workdir.join(".gitmodules").exists() {
        bail!(
            "dependency `{dep_key}`: repository at {url} uses git submodules (gitlink \
             entries or .gitmodules present at commit {commit}); git submodules are not \
             supported in v0 — use a release archive (url + sha256) or a fork with \
             vendored submodules"
        );
    }

    // The store entry is a plain source tree; the repo metadata would only
    // invite accidental mutation and bloat.
    fs::remove_dir_all(workdir.join(".git"))
        .with_context(|| format!("removing .git from {}", workdir.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// url sources

fn ensure_url(
    stores: &Stores,
    dep_key: &str,
    url: &str,
    sha256: &str,
    locked: Option<&LockedPackage>,
) -> Result<RawPackage> {
    // Only the lockfile lets us know the package id without downloading.
    if let Some(id) = locked.and_then(|l| l.content_hash.as_deref()) {
        let entry = stores.raw_dir(dep_key, id);
        if stores.entry_complete(&entry) {
            return Ok(RawPackage { path: entry, package_id: id.to_string() });
        }
    }

    let bytes = fetch_url_bytes(url)
        .with_context(|| format!("dependency `{dep_key}`: downloading {url}"))?;
    verify_sha256(dep_key, url, &bytes, sha256)?;
    let package_id = crate::hashing::blake3_bytes_labeled(&bytes);

    if let Some(pinned) = locked.and_then(|l| l.content_hash.as_deref())
        && pinned != package_id {
            bail!(
                "dependency `{dep_key}`: content of {url} changed since it was locked: \
                 lockfile pins {pinned}, downloaded bytes hash to {package_id}"
            );
        }

    let entry = stores.raw_dir(dep_key, &package_id);
    if stores.entry_complete(&entry) {
        return Ok(RawPackage { path: entry, package_id });
    }
    clear_stale_entry(&entry)?;
    extract_archive(dep_key, url, &bytes, &entry)?;
    stores.mark_complete(&entry)?;
    Ok(RawPackage { path: entry, package_id })
}

/// Download to memory. Separated from `ensure`'s post-processing so tests can
/// feed `verify_sha256`/`extract_archive` local bytes (ureq has no file://).
#[doc(hidden)]
pub fn fetch_url_bytes(url: &str) -> Result<Vec<u8>> {
    let response = ureq::get(url).call().with_context(|| format!("GET {url}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading response body of {url}"))?;
    Ok(bytes)
}

#[doc(hidden)]
pub fn verify_sha256(dep_key: &str, url: &str, bytes: &[u8], declared: &str) -> Result<()> {
    let actual = hex::encode(Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(declared.trim()) {
        bail!(
            "dependency `{dep_key}`: sha256 mismatch for {url}:\n  declared:   {declared}\n  downloaded: {actual}\n\
             the upstream file changed or the declared hash is wrong"
        );
    }
    Ok(())
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

#[doc(hidden)]
pub fn archive_kind(url: &str) -> Result<ArchiveKind> {
    // Query strings / fragments are not part of the file name.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        Ok(ArchiveKind::TarGz)
    } else if path.ends_with(".zip") {
        Ok(ArchiveKind::Zip)
    } else {
        bail!("unsupported archive type for {url}: expected .tar.gz, .tgz, or .zip")
    }
}

/// Extract `bytes` (a tar.gz or zip archive, decided from the url) into
/// `dest`, which must not exist yet. If the archive contains exactly one
/// top-level directory (the common `pkg-1.2.3/` convention), its contents
/// become the entry root. Shells out to `tar`/`unzip`: no archive crate is
/// available, and both tools ship with macOS.
#[doc(hidden)]
pub fn extract_archive(dep_key: &str, url: &str, bytes: &[u8], dest: &Path) -> Result<()> {
    let kind = archive_kind(url).with_context(|| format!("dependency `{dep_key}`"))?;
    let parent = entry_parent(dest)?;
    // All scratch space lives beside the destination so the final rename
    // never crosses a filesystem boundary.
    let scratch = fresh_tmp_dir(parent, "extract")?;
    let guard = TempGuard(scratch.clone());

    let archive = scratch.join("archive");
    fs::write(&archive, bytes)
        .with_context(|| format!("writing archive to {}", archive.display()))?;
    let tree = scratch.join("tree");
    fs::create_dir(&tree)?;

    let mut cmd = match kind {
        ArchiveKind::TarGz => {
            let mut c = Command::new("tar");
            c.arg("-xzf").arg(&archive).arg("-C").arg(&tree);
            c
        }
        ArchiveKind::Zip => {
            let mut c = Command::new("unzip");
            c.arg("-q").arg(&archive).arg("-d").arg(&tree);
            c
        }
    };
    run_ok(&mut cmd).with_context(|| format!("dependency `{dep_key}`: extracting {url}"))?;

    let entries = fs::read_dir(&tree)?.collect::<std::io::Result<Vec<_>>>()?;
    let root = match entries.as_slice() {
        [only] if only.file_type()?.is_dir() => only.path(),
        _ => tree,
    };
    fs::rename(&root, dest)
        .with_context(|| format!("moving extracted tree into {}", dest.display()))?;
    drop(guard);
    Ok(())
}

// ---------------------------------------------------------------------------
// process + filesystem plumbing

fn run_git<I, S>(cwd: Option<&Path>, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("git");
    // Repo-locating variables inherited from the invoking environment (set
    // inside git hooks, rebase scripts, filter drivers) would silently
    // redirect init/fetch/checkout at some other repository. Auth-related
    // GIT_* (GIT_SSH_COMMAND, GIT_ASKPASS, credential config) deliberately
    // survive — private-repo fetches need them.
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(var);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.args(args);
    run_ok(&mut cmd)
}

/// Run to completion, capturing output; non-zero exit becomes an error that
/// carries the command line and stderr (the only clue git/tar/unzip leave).
fn run_ok(cmd: &mut Command) -> Result<String> {
    let rendered = format!(
        "{} {}",
        cmd.get_program().to_string_lossy(),
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let out = cmd.output().with_context(|| format!("running `{rendered}`"))?;
    if !out.status.success() {
        bail!(
            "`{rendered}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn entry_parent(entry: &Path) -> Result<&Path> {
    entry
        .parent()
        .ok_or_else(|| anyhow::anyhow!("store entry path {} has no parent", entry.display()))
}

/// Reaching here means the completion marker was absent: any directory at the
/// entry path is debris from an interrupted run and must go before rebuilding.
fn clear_stale_entry(entry: &Path) -> Result<()> {
    fs::create_dir_all(entry_parent(entry)?)?;
    if entry.exists() {
        fs::remove_dir_all(entry)
            .with_context(|| format!("removing incomplete store entry {}", entry.display()))?;
    }
    Ok(())
}

/// Unique scratch directory directly under `parent` (same filesystem as the
/// final entry, so renames are cheap and atomic).
fn fresh_tmp_dir(parent: &Path, label: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for n in 0u32.. {
        let dir = parent.join(format!(".cppkg-tmp-{label}-{}-{nanos}-{n}", std::process::id()));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("creating temp dir under {}", parent.display()))
            }
        }
    }
    unreachable!("u32 counter exhausted")
}

/// Best-effort scratch cleanup on error paths; success paths rename the dir
/// away first, making the drop a no-op.
struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

