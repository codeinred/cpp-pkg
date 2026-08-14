//! Dependency acquisition into the raw download store.
//!
//! Contract (CPPKG_TOML.md lockfile section; DESIGN_CHOICES.md):
//! - git + tag: resolve tag -> commit (ls-remote or clone+rev-parse), clone
//!   at that commit into the raw store; the COMMIT SHA is the package id and
//!   integrity reference (verify via rev-parse after checkout). Tags are
//!   mutable; the lockfile pin (commit) wins over re-resolving the tag.
//! - git + rev: fetch that commit directly.
//! - url + sha256: download (ureq), verify user-declared sha256, package id
//!   is "blake3:<hex>" of the archive bytes; extract tar.gz/tar.xz/tar.bz2/
//!   zip into the raw store entry (strip a single top-level directory if the
//!   archive has one).
//! - SUBMODULES: gitlink entries (mode 160000) in the checkout's index ->
//!   hard error ("git submodules are not supported"), do not build silently.
//!   Gitlinks are the ground truth; a `.gitmodules` file alone (possibly
//!   0-byte, the json-tui false positive) declares nothing checked out.
//! - PATCHES (wave 1, spec §5.2): declared patch bytes are applied in
//!   manifest order via `git apply -p1 --whitespace=nowarn` at the checkout
//!   root, into a temp dir renamed into a SEPARATE store entry on success —
//!   the pristine raw entry is never mutated. The composed package id is
//!   `<base>+patches:<blake3_32 of len-prefixed patch bytes>`; patched
//!   sources ARE different sources, so identity lives in the package id and
//!   the `cppkg-config-hash-v1` encoding is untouched.
//! - Fresh-machine flow: lockfile commit + source URL suffice to re-download.
//! - System deps (wave 1, §5.3) are never fetched: they resolve from the
//!   machine via the provisioning probe; `ensure` refuses them defensively.
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
    /// Identity of the tree at `path`: commit sha (git) or "blake3:<hex>"
    /// (url), COMPOSED to "<base>+patches:<hex>" when patches were applied.
    /// Feeds config hashing — patched sources are different sources.
    pub package_id: String,
    /// Upstream identity: always the base commit / archive hash, never a
    /// composed id. Feeds `${pin.<dep>.commit}` and the lockfile
    /// `commit`/`content-hash` rows (version-stamping wants upstream truth).
    pub base_id: String,
}

/// Ensure the dependency's source is present in the raw store; returns the
/// entry. Uses `locked` (if it matches the request) instead of re-resolving
/// mutable refs. Network is touched only when the store lacks the entry.
///
/// `patches`: the dep's declared patch files as (path-as-declared, bytes) in
/// manifest order. The caller reads and validates existence (missing file /
/// duplicates are resolve-time schema errors); this module hashes and
/// applies the BYTES — renaming a patch file without changing bytes does
/// not re-key.
pub fn ensure(
    stores: &Stores,
    dep_key: &str,
    spec: &DependencySpec,
    locked: Option<&LockedPackage>,
    patches: &[(PathBuf, Vec<u8>)],
) -> Result<RawPackage> {
    let base = match &spec.source {
        SourceSpec::Git { url, reference } => ensure_git(stores, dep_key, url, reference, locked)?,
        SourceSpec::Url { url, sha256 } => ensure_url(stores, dep_key, url, sha256, locked)?,
        // Reaching here is a caller bug — provisioning system deps is the
        // probe's job (§5.3) — but a clear refusal beats a silent wrong
        // build.
        SourceSpec::System { .. } => bail!(
            "dependency `{dep_key}`: system dependencies resolve from the machine \
             (system = true) and are provisioned by the system probe, never fetched"
        ),
    };
    if patches.is_empty() {
        return Ok(base);
    }
    ensure_patched(stores, dep_key, &base, patches)
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
        return Ok(RawPackage { path: entry, package_id: commit.clone(), base_id: commit });
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
    Ok(RawPackage { path: entry, package_id: commit.clone(), base_id: commit })
}

// ---------------------------------------------------------------------------
// patch application (wave 1, spec §5.2)

/// Composed package id for a patched source (`hashing::compose_patched_id`
/// over the BYTES, in declared order — paths are carried only for error
/// messages and never hashed, so renaming a patch file does not re-key).
#[doc(hidden)]
pub fn composed_package_id(base_id: &str, patches: &[(PathBuf, Vec<u8>)]) -> String {
    let bytes: Vec<Vec<u8>> = patches.iter().map(|(_, b)| b.clone()).collect();
    crate::hashing::compose_patched_id(base_id, &bytes)
}

/// Materialize the patched variant of `base` as its own raw store entry.
/// The pristine base entry is never touched: the tree is copied to a temp
/// dir, patched there, and renamed into place only on full success.
fn ensure_patched(
    stores: &Stores,
    dep_key: &str,
    base: &RawPackage,
    patches: &[(PathBuf, Vec<u8>)],
) -> Result<RawPackage> {
    let package_id = composed_package_id(&base.package_id, patches);
    let entry = stores.raw_dir(dep_key, &package_id);
    // Layout invariant, not a user error: raw_dir must render composed ids
    // distinguishably ("absl-255c84da+a1b2c3d4"). If it does not, an
    // entry_complete hit below would silently return the PRISTINE tree as
    // the patched one — fail loudly instead.
    if entry == stores.raw_dir(dep_key, &base.package_id) {
        bail!(
            "store layout bug: raw_dir renders the patched package id for `{dep_key}` \
             identically to its unpatched id — patched and pristine entries would collide"
        );
    }
    if stores.entry_complete(&entry) {
        return Ok(RawPackage { path: entry, package_id, base_id: base.base_id.clone() });
    }
    clear_stale_entry(&entry)?;
    materialize_patched(dep_key, &base.path, &base.base_id, patches, &entry)?;
    stores.mark_complete(&entry)?;
    Ok(RawPackage { path: entry, package_id, base_id: base.base_id.clone() })
}

/// Copy `base_tree` to a temp dir beside `dest`, apply every patch in order
/// with `git apply -p1 --whitespace=nowarn` at the tree root, and atomically
/// rename into `dest`. Any failure leaves no partial entry at `dest`.
#[doc(hidden)]
pub fn materialize_patched(
    dep_key: &str,
    base_tree: &Path,
    base_id: &str,
    patches: &[(PathBuf, Vec<u8>)],
    dest: &Path,
) -> Result<()> {
    let parent = entry_parent(dest)?;
    fs::create_dir_all(parent)?;
    let tmp = fresh_tmp_dir(parent, "patch")?;
    let guard = TempGuard(tmp.clone());

    // Copy the pristine tree, skipping store control files at the root
    // (".cppkg-*": the completion marker, stray temp debris) — they belong
    // to the base entry's lifecycle, not to the source tree.
    for item in fs::read_dir(base_tree)
        .with_context(|| format!("reading base tree {}", base_tree.display()))?
    {
        let item = item?;
        if item.file_name().to_string_lossy().starts_with(".cppkg-") {
            continue;
        }
        copy_tree_entry(&item.path(), &tmp.join(item.file_name()))?;
    }

    for (patch_path, bytes) in patches {
        // `git apply` works in non-repo directories (url tarballs share this
        // code path) and rejects `../` escapes even there. Exact context,
        // zero fuzz, offset drift tolerated; binary patches allowed.
        let applied = run_git_stdin(
            &tmp,
            &["apply", "-p1", "--whitespace=nowarn"],
            bytes,
        );
        if let Err(e) = applied {
            // The git stderr inside `e` names the failed file and hunk.
            return Err(e.context(format!(
                "dependency `{dep_key}`: applying patch `{}` failed \
                 (hint: re-diff the patch against {base_id})",
                patch_path.display()
            )));
        }
    }

    fs::rename(&tmp, dest)
        .with_context(|| format!("moving patched tree into store entry {}", dest.display()))?;
    drop(guard);
    Ok(())
}

/// Recursively copy one directory entry (dir, file, or symlink). `fs::copy`
/// preserves permission bits (execute bits on scripts matter to builds);
/// symlinks are recreated, not followed — a checkout's internal links must
/// survive as links or `git apply` and builds would diverge from upstream.
fn copy_tree_entry(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)
        .with_context(|| format!("stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            let target = fs::read_link(src)?;
            std::os::unix::fs::symlink(&target, dst)
                .with_context(|| format!("recreating symlink {}", dst.display()))?;
            return Ok(());
        }
        #[cfg(not(unix))]
        bail!("cannot copy symlink {} on this platform", src.display());
    }
    if meta.is_dir() {
        fs::create_dir(dst).with_context(|| format!("creating {}", dst.display()))?;
        for item in fs::read_dir(src)? {
            let item = item?;
            copy_tree_entry(&item.path(), &dst.join(item.file_name()))?;
        }
        return Ok(());
    }
    fs::copy(src, dst)
        .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
    Ok(())
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
    // builds silently without its vendored code. `.gitmodules` presence
    // alone deliberately does NOT trip the guard (wave-1 fix A.6): a stale
    // or 0-byte .gitmodules with no gitlink — the json-tui false positive —
    // means nothing is actually vendored via submodules at this commit.
    let index = run_git(Some(workdir), ["ls-files", "-s"])?;
    let has_gitlink = index.lines().any(|l| l.starts_with("160000 "));
    if has_gitlink {
        bail!(
            "dependency `{dep_key}`: repository at {url} uses git submodules (gitlink \
             entries present at commit {commit}); git submodules are not \
             supported — use a release archive (url + sha256) or a fork with \
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
            return Ok(RawPackage {
                path: entry,
                package_id: id.to_string(),
                base_id: id.to_string(),
            });
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
        return Ok(RawPackage { path: entry, package_id: package_id.clone(), base_id: package_id });
    }
    clear_stale_entry(&entry)?;
    extract_archive(dep_key, url, &bytes, &entry)?;
    stores.mark_complete(&entry)?;
    Ok(RawPackage { path: entry, package_id: package_id.clone(), base_id: package_id })
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
    TarXz,
    TarBz2,
    Zip,
}

#[doc(hidden)]
pub fn archive_kind(url: &str) -> Result<ArchiveKind> {
    // Query strings / fragments are not part of the file name.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        Ok(ArchiveKind::TarGz)
    } else if path.ends_with(".tar.xz") {
        Ok(ArchiveKind::TarXz)
    } else if path.ends_with(".tar.bz2") {
        Ok(ArchiveKind::TarBz2)
    } else if path.ends_with(".zip") {
        Ok(ArchiveKind::Zip)
    } else {
        bail!(
            "unsupported archive type for {url}: expected .tar.gz, .tgz, .tar.xz, \
             .tar.bz2, or .zip"
        )
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
        // Explicit decompression flags per kind, not `-xf` auto-detection:
        // a mislabeled archive should fail loudly, not silently succeed as
        // some other format. bsdtar (macOS) and GNU tar (Linux) both take
        // -z / -J / -j with these spellings.
        ArchiveKind::TarGz | ArchiveKind::TarXz | ArchiveKind::TarBz2 => {
            let flag = match kind {
                ArchiveKind::TarGz => "-xzf",
                ArchiveKind::TarXz => "-xJf",
                ArchiveKind::TarBz2 => "-xjf",
                ArchiveKind::Zip => unreachable!(),
            };
            let mut c = Command::new("tar");
            c.arg(flag).arg(&archive).arg("-C").arg(&tree);
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

/// A `git` Command with hostile-environment hygiene applied. Repo-locating
/// variables inherited from the invoking environment (set inside git hooks,
/// rebase scripts, filter drivers) would silently redirect init/fetch/
/// checkout at some other repository. Auth-related GIT_* (GIT_SSH_COMMAND,
/// GIT_ASKPASS, credential config) deliberately survive — private-repo
/// fetches need them.
fn git_command(cwd: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
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
    cmd
}

fn run_git<I, S>(cwd: Option<&Path>, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = git_command(cwd);
    cmd.args(args);
    run_ok(&mut cmd)
}

/// Like `run_git`, but feeds `input` on stdin (patch bytes to `git apply` —
/// bytes are the hashed identity, so they go to git verbatim, never through
/// an intermediate file that something could normalize). Sets
/// GIT_CEILING_DIRECTORIES to the parent so behavior is "outside a repo"
/// even when the store happens to live under someone's working tree —
/// non-repo mode is the documented semantics for both git checkouts (whose
/// .git we removed) and extracted tarballs.
fn run_git_stdin(cwd: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut cmd = git_command(Some(cwd));
    if let Some(parent) = cwd.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let rendered = format!("git {}", args.join(" "));
    let mut child = cmd.spawn().with_context(|| format!("running `{rendered}`"))?;
    // Defer the write error: git may reject a malformed patch and exit
    // before draining stdin (EPIPE here) — its stderr is the message worth
    // reporting, not our broken pipe.
    let write_res = child.stdin.take().expect("stdin was piped").write_all(input);
    let out = child.wait_with_output().with_context(|| format!("running `{rendered}`"))?;
    if !out.status.success() {
        bail!(
            "`{rendered}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    write_res.with_context(|| format!("writing stdin of `{rendered}`"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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

