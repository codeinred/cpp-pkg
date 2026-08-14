# Wave-1 Implementation Plan — module bundles, interface deltas, flows

Status: **binding contract between the 9 implementer bundles + integration**
(architect, 2026-08-14). Normative semantics live in
`docs/design/wave1-extensions.md` (spec §-references below); this file fixes
*who implements what, behind which signatures*. Where a signature here and a
spec sentence disagree on semantics, the spec wins; where two implementers
disagree on an interface, this file wins. `CPPKG_TOML.md` has been updated to
the post-wave user-facing schema (same commit as this file).

Hard rules restated (they bit us before):

- **Config-hash discipline is sacred** — spec §8 table. The
  `cppkg-config-hash-v1` byte encoding is *untouched* for every input that
  exists today. The only encoding delta is the conditional `subdir` suffix
  (see store-hash bundle), which is byte-absent when `subdir` is unset, so no
  existing store entry re-keys. Patches compose into `package_id`, never into
  the encoding. Assert all of this by test.
- Each bundle modifies only its listed files. Cross-module needs = the
  signatures written here; if you need more, report it, don't reach.
- `src/lib.rs` and `Cargo.toml` are integration-agent-only. The one new
  module this wave, `src/interp.rs`, is **created by the schema bundle**; the
  integration agent adds `pub mod interp;` to lib.rs. No other new modules.
- All four `tests/projects/` stay green **unedited** (they use zero wave-1
  surface; byte-identical behavior for unmarked manifests is a spec
  requirement, §3.2).

---

## 0. Shared new vocabulary (types every bundle sees)

Defined in `src/schema.rs` (schema bundle) unless noted:

```rust
/// §2.1 — closed predicate vocabulary. Combinators are reserved (distinct
/// error carrying the quoted-key spelling).
pub enum CfgAtom { Windows, Macos, Linux, Unix, Clang, Gcc, Msvc }

/// v1 predicate = single atom. Parsed from the sub-table key; unknown atom
/// = hard error listing the vocabulary; `all(...)`/`any(...)`/`not(...)`/
/// version forms = "reserved, not available in v1" error.
pub struct CfgPredicate { pub atom: CfgAtom }

/// What the current toolchain IS — derived by toolchain.rs from
/// ToolchainIdentity (os from target_triple; clang matches AppleClang).
pub struct CfgTruth { pub os: CfgAtom /* Windows|Macos|Linux */,
                      pub compiler: CfgAtom /* Clang|Gcc|Msvc */ }

impl CfgPredicate {
    /// unix == (os is Macos|Linux). Pure; evaluated at plan time (§2.2).
    pub fn eval(&self, truth: &CfgTruth) -> bool;
}
```

```rust
/// §0.1 — the one value grammar, reused (exists in v0) for the three new
/// flag keys. No new grammars anywhere.
pub struct VisibilitySplit { pub public: Vec<String>, pub private: Vec<String> }
```

`src/interp.rs` (NEW, schema bundle) — §0.3's single resolver:

```rust
pub struct PinInfo { pub commit: String, pub requested: String }

/// Everything ${...} can resolve from. Optional fields are None when the
/// position can't legally use them; the resolver errors (never
/// empty-substitutes) if a variable's source is absent.
pub struct InterpCtx<'a> {
    pub package_name: &'a str,
    pub package_version: Option<&'a str>,
    pub pins: &'a std::collections::BTreeMap<String, PinInfo>, // by dep key; commit = BASE commit (§5.2)
    pub gen_root: Option<&'a std::path::Path>,      // ${gen} -> build/gen
    pub project_root: Option<&'a std::path::Path>,
    pub build_dir: Option<&'a std::path::Path>,
    pub install_prefix: Option<&'a str>,            // default "/usr/local"
}

/// The whitelisted positions (§0.3 table). Each admits a fixed variable
/// subset; anything else containing `${` (unescaped) is a hard error.
pub enum InterpPos { DefineValue, GenerateVarOrArgv, SourceOrIncludeEntry, RunEntryValue }

/// `$${` escapes a literal `${`. Unknown variable => error naming the
/// closed vocabulary for `pos`. `${pin.self.*}` => the §0.3 reserved error.
pub fn interpolate(text: &str, pos: InterpPos, ctx: &InterpCtx) -> crate::Result<String>;

/// True if `text` contains an unescaped `${` — schema validation uses this
/// to hard-error on `${` outside whitelisted positions (§0.3).
pub fn contains_interp(text: &str) -> bool;
```

Glob machinery with `!` negation (§0.4) lives in **graph.rs** and is exported
for shim-export reuse:

```rust
/// Union(positives) − union(negatives), post-expansion, then lexicographic
/// sort. Negative matching nothing => push a warning; positives expanding to
/// nothing => Err (existing error); all-negative list is rejected upstream
/// by schema. `base` anchors relative patterns.
pub fn expand_patterns(base: &std::path::Path, patterns: &[String],
                       warnings: &mut Vec<String>) -> crate::Result<Vec<std::path::PathBuf>>;
```

---

## 1. Bundle: schema (`src/schema.rs` + NEW `src/interp.rs` + `tests/schema_test.rs`)

Implements: §0.1–0.5 (grammar/placement/interp/exclusion/merge rules as
*load-time surface + validation*), §1.1/§1.2/§1.4 (flag surface + fence
errors, calling toolchain's classifier), §2.1–2.2 (cfg parsing/validation;
evaluation helper), §3.1–3.3 surface (dev-deps, markers, run entries),
§4.1 surface (`[generate.*]` parsing + static validation), §5.1 surface
(`patches`, `system = true`, `min-version`, `subdir` from A.5), §6.1 surface
(`[export]`, `install`, `public-headers`, `runtime-data`), §7.2
(`[target-defaults]` incl. eligibility merge at load), §0.6/§5.4 (builtin
name reservation errors), §9 (every reserved spelling gets its distinct
error).

### Type deltas

```rust
pub struct ProjectFile {
    pub package: PackageMeta,
    pub toolchains: BTreeMap<String, ToolchainPreset>,
    pub profiles: BTreeMap<String, Profile>,
    pub flags: PackageFlags,                              // NEW §1.1; empty default
    pub dependencies: BTreeMap<String, DependencySpec>,
    pub dev_dependencies: BTreeMap<String, DependencySpec>, // NEW §3.2 (key collision across tables = load error)
    pub generate: BTreeMap<String, GenerateStep>,         // NEW §4.1
    pub export: ExportMeta,                               // NEW §6.1 (defaults = package.name)
    pub targets: BTreeMap<String, TargetSpec>,
    // [target-defaults] is APPLIED at load (§7.2: before validation) and the
    // raw table retained only for `--query` display:
    pub target_defaults_raw: Option<toml::Table>,
}

pub struct PackageFlags {                                 // [flags] — no visibility split (§1.1)
    pub cxx_flags: Vec<String>,
    pub c_flags: Vec<String>,
    pub link_flags: Vec<String>,
    /// [flags.cfg.<pred>] groups, document order.
    pub cfg: Vec<(CfgPredicate, PackageFlagsGroup)>,
}
pub struct PackageFlagsGroup { pub cxx_flags: Vec<String>, pub c_flags: Vec<String>, pub link_flags: Vec<String> }

pub struct ExportMeta { pub cmake_name: String, pub namespace: String } // defaults filled from package.name

pub enum SourceSpec {
    Git { url: String, reference: GitRef },
    Url { url: String, sha256: String },
    System { min_version: Option<String> },               // NEW §5.3
}

pub struct DependencySpec {
    pub source: SourceSpec,
    pub options: BTreeMap<String, String>,
    pub needs: Vec<String>,
    pub find_package: Option<String>,
    pub exposes_namespace: Vec<String>,
    pub exposes_targets: ExposesTargets,
    pub patches: Vec<PathBuf>,          // NEW §5.2; manifest-dir-relative; dupes = error; illegal with System
    pub subdir: Option<String>,         // NEW A.5; non-empty, no leading '/', no '..'
    /// NEW §2.2: Some(pred) iff declared under [cfg.<pred>.dependencies.*]
    /// (or ...dev-dependencies...). Same key declared twice (any two
    /// branches, or branch+unconditional) = hard error.
    pub cfg: Option<CfgPredicate>,
    /// NEW §3.2: true iff the spec came from a dev table (set by loader;
    /// both tables share this one struct).
    pub dev: bool,
}

pub struct TargetSpec {
    pub kind: TargetKind,
    pub sources: Vec<String>,
    pub cxx_std: Option<u32>,
    pub c_std: Option<u32>,
    pub includes: VisibilitySplit,
    pub defines: VisibilitySplit,
    pub dependencies: VisibilitySplit,
    pub cxx_flags: VisibilitySplit,        // NEW §1.1
    pub c_flags: VisibilitySplit,          // NEW
    pub link_flags: VisibilitySplit,       // NEW (bare list == private, as ever)
    pub system_includes: Option<bool>,     // NEW §1.1 (None => kind-dependent default at use site)
    pub dev: bool,                         // NEW §3.2
    pub test: bool,                        // NEW §3.2 (implies dev; exe only)
    pub install: bool,                     // NEW §6.1 (default false; may be filled by target-defaults, eligibility-gated)
    pub public_headers: Option<PublicHeaders>, // NEW §6.4 total override
    pub runtime_data: Vec<RuntimeData>,    // NEW §6.5
    pub run: Vec<RunEntry>,                // NEW §3.2; legal only when test
    /// [targets.<t>.cfg.<pred>] groups, document order (§2.2).
    pub cfg: Vec<(CfgPredicate, TargetCfgGroup)>,
}

/// Only list-valued keys are conditionable (§2.2); anything else in a cfg
/// group = the appropriate hard error (scalars: "not in v1";
/// public-headers: the §2.2 error pointing at includes.public).
pub struct TargetCfgGroup {
    pub sources: Vec<String>,
    pub includes: VisibilitySplit,
    pub defines: VisibilitySplit,
    pub dependencies: VisibilitySplit,
    pub cxx_flags: VisibilitySplit,
    pub c_flags: VisibilitySplit,
    pub link_flags: VisibilitySplit,
    pub runtime_data: Vec<RuntimeData>,
}

pub struct PublicHeaders { pub base: String, pub patterns: Vec<String> } // §6.4; ${gen} NOT legal in base
pub struct RuntimeData { pub from: String, pub patterns: Vec<String> /* default ["**/*"] */, pub to: String /* default: last comp of from */ }

pub struct RunEntry {                       // §3.2; deny_unknown_fields
    pub name: Option<String>,               // unique per target
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub env_remove: Vec<String>,
    pub expect_failure: bool,               // default false
}

pub struct GenerateStep {                   // §4.1; name shares target charset
    pub name: String,
    pub action: GenerateAction,             // exactly one of template/command
    pub inputs: Vec<String>,                // declared extra inputs
    pub checked_in: Option<String>,         // checked-in mode target path
}
pub enum GenerateAction {
    Template { template: String, output: String, vars: BTreeMap<String, String> },
    Command  { argv: Vec<String>, stdin: Option<String>, stdout: String },
}
```

### Validation done here (load time — everything that needs no toolchain
identity and no filesystem):

- cfg: vocabulary, reserved combinator/quoted-key error, reserved placements
  (`[target-defaults.cfg.*]`, `[cfg.*.targets.*]`, `[cfg.*.generate.*]`,
  `[profiles.*.cfg.*]`), nested cfg error, empty-group lint warning,
  `when =` rejection, scalar-in-cfg error, markers non-conditionable.
- Flag fence §1.2: for every **public** bucket entry call
  `toolchain::classify_word_sequence` (see toolchain bundle) and reject
  Abi/Sanitizer/Warning/OptDebug classes with the spec's messages; ABI class
  rejected at **any** target scope (public or private, §1.4); public flags on
  executables = error; `link-flags` public bucket checks Abi/Sanitizer only.
  `-D*/-U*/-I*/-isystem/-std=*` in any flag list => warning naming the
  dedicated key. (Pure fn call — no toolchain *detection* at load.)
- Markers: `test` implies `dev`; `test=true, dev=false` = reserved error;
  `test` on non-executable = error (+hint); `run` on non-test = error; dup
  run names; `install=true` on a test target = error (§6.4).
- Dev/dep tables: key collision; regular dep `needs` naming a dev-dep =
  error; `needs` **on** a system dep = error; `patches`/`options` on
  system dep = error; patches on url/git ok; `{file, strip}` patch table =
  reserved error.
- Builtins §0.6/§5.4: `exposes-namespace`/`exposes-targets` claiming
  `Threads::Threads` (or namespace `Threads`) = hard error "builtin
  pseudo-package; delete this line" (the wave's one flag-day break).
- Interp §0.3: walk every string value; `interp::contains_interp` outside
  whitelisted positions = hard error. (Resolution happens later, at plan/gen
  time — load-time we only police placement + parse `$${` escapes.)
- §0.4: a pattern list that is all-negative = schema error. (Expansion +
  no-match handling is graph's.)
- `[target-defaults]` §7.2: accepted keys {cxx-std, c-std, defines, includes,
  system-includes, install, public-headers, runtime-data}; flags keys =
  reserved error pointing at `[flags]`; dependencies/markers/sources/type =
  error. Merge before validation: scalars fill-if-absent, list/visibility
  keys **prepend** defaults; eligibility from the target's own pre-merge
  markers/kind (install/public-headers skip dev/test; public-headers only
  onto installing libraries; runtime-data fills everywhere).
- `[export]`: defaults; charset checks.
- `[generate]`: exactly-one-of template/command; output/stdout paths
  relative, no `..`/absolute (§4.2); case-insensitive output-collision check
  across steps (plan-level check lives here since it's pure manifest);
  name charset.

### Tests (inline + `tests/schema_test.rs`)

Every error message above has a test; round-trip of the §2.3 canonical
examples; target-defaults merge order (defaults-unconditional →
target-unconditional → target-cfg — defaults-cfg reserved); v0 manifests
parse byte-identically (`Default`s everywhere).

---

## 2. Bundle: graph (`src/graph.rs`)

Implements: §1.3 layering (compile + link lines), §2.2 evaluation/merge,
§3.2 build-set selection + dev-edge rule, §4.2 step activation/ordering +
`${gen}` source/include wiring, §5.4 Threads expansion in the closure,
§6.5 build-time runtime-data staging plan, §7.1 glob negation (via
`expand_patterns`), §0.5 merge orders. Consumes `interp` for
defines/sources/includes positions.

### Interface deltas

```rust
/// How the build set was requested (§3.2). Default = all non-dev targets.
/// Named may include dev targets explicitly. Tests = test-marked targets
/// matching the filters (empty = all tests).
pub enum BuildRequest { Default, Named(Vec<String>), Tests(Vec<String>) }

pub struct PlanInputs<'a> {
    pub project: &'a ProjectFile,
    pub project_root: &'a Path,
    pub manifests: &'a BTreeMap<String, Manifest>,   // only PROVISIONED deps present
    pub config: BuildConfig,
    pub profile: &'a Profile,
    pub cfg_truth: &'a CfgTruth,                     // NEW — drives §2 evaluation
    pub request: &'a BuildRequest,                   // NEW — replaces `only`
    pub interp: &'a interp::InterpCtx<'a>,           // NEW — pins, gen root, install-prefix
}

pub fn plan(inputs: &PlanInputs) -> Result<BuildPlan>;   // signature CHANGE (cli adapts)

pub struct BuildPlan {
    pub targets: Vec<PlannedTarget>,                 // topo order, as today
    pub gen_steps: Vec<PlannedGenStep>,              // NEW — activated steps only, topo order
    pub data_stages: Vec<DataStage>,                 // NEW §6.5 — deduped copy plan
    pub warnings: Vec<String>,                       // NEW (negative-glob no-match, empty cfg groups, …)
}

/// A [generate] step the requested set activates (§4.2 laziness: something
/// in the build/test set references ${gen}). checked-in steps NEVER appear
/// here (outside the build graph; `cpp-pkg gen` handles them).
pub struct PlannedGenStep {
    pub name: String,
    pub action: GenerateAction,        // vars/argv already interpolated
    pub inputs: Vec<PathBuf>,          // project-root-relative; existence pre-checked (plan-time hard error, §4.2)
    pub output: PathBuf,               // gen-root-relative
}

pub struct DataStage {
    pub src: PathBuf,                  // absolute
    pub dest: PathBuf,                 // build-dir-relative (beside owning target's output)
    /// Targets whose build must stage this (order-only attachment).
    pub for_targets: Vec<String>,
}

pub struct PlannedTarget {
    // ... v0 fields unchanged ...
    /// NEW — export-facing effective (post-cfg, post-defaults) metadata so
    /// shim-export never re-evaluates cfg (§6.3/6.4 read these):
    pub install: bool,
    pub dev: bool, pub test: bool,
    pub public_includes: Vec<PathBuf>,         // absolute, cfg-projected
    pub public_defines: Vec<(String, Option<String>)>,
    pub public_flags: Vec<String>,             // cxx public bucket, merged order
    pub public_link_flags: Vec<String>,
    pub cxx_std: Option<u32>,
    pub public_headers: Option<PublicHeaders>, // copied through
    /// Resolved run entries (test targets; §3.2), interp applied except
    /// ${build-dir}-relative resolution which the runner finishes.
    pub run: Vec<schema::RunEntry>,
    /// External dep components in this target's closure, by dep key —
    /// shim-export turns these into find_dependency/requires rows (§6.3).
    pub external_deps: BTreeMap<String, Vec<String>>,
}

/// Which dependency keys the request demands (provisioning laziness, §3.2 +
/// §2.2): cfg-inactive deps are excluded always; dev-deps included only
/// when the request selects a dev/test target. v1 granularity: per-table ×
/// cfg, refined per-dep where references are attributable pre-fetch
/// (depkey:: prefixes, exposes-* claims). Used by cli BEFORE fetching.
pub fn required_deps(project: &ProjectFile, truth: &CfgTruth,
                     request: &BuildRequest) -> Result<BTreeSet<String>>;
```

### Behavior (the tricky parts)

- **cfg evaluation feeding everything**: build one `EffectiveTarget` per
  target = unconditional lists + matching cfg groups appended in document
  order, per key *and per visibility bucket* (§0.5, §1.3 step-6 note). All
  downstream reads (units, layering, propagation step 5, PlannedTarget
  export metadata) see only the effective view. Non-matching groups: never
  expanded, never path-checked.
- **Layering** (§1.3): compile line = driver/config defaults → ABI injection
  set → `[flags]` non-ABI + matching `[flags.cfg]` → profile → propagated
  public flags of transitive compile-visible deps (topo, dedup by
  contributing target) → own public → own private. Link line = objects →
  `[flags].link-flags` → profile link-flags → own link-flags → closure
  interleaved (member archive immediately followed by member's link-flags;
  §1.3 as-needed rationale). ABI-classified `[flags]` entries are *excluded*
  here (they arrive via toolchain-file injection; cli passes them as
  abi_flags — see store-hash).
- **Dev edges** (§3.2): non-dev target → dev target or dev-dep-owned target
  = the §3.3 error verbatim. Reference to a cfg'd-out dep's target = the
  §2.2 augmented unresolved-reference error.
- **Threads** (§5.4): dependency reference `Threads::Threads` resolves at
  ladder step 0 to the builtin; manifests' `builtin:threads` link inputs
  resolve the same way. Expansion: ask
  `toolchain::threads_expansion(truth.os)`; linux → `-pthread` appended to
  compile flags + link line of every target whose closure contains it
  (before target flags, so last-wins user overrides hold); macos → nothing.
- **`${gen}`**: `sources`/`includes` entries interpolate; a `${gen}` source
  must byte-match a declared step output (no globbing under gen root);
  units referencing gen get `references_gen = true` on CompileUnit
  (ninja adds the order-only `cppkg-gen` edge). Activation set =
  steps whose outputs are referenced by the request's sources/includes/run
  env, plus steps reachable through gen-input → gen-output edges (implicit
  ordering; cycles = plan error).
- **runtime-data** (§6.5): expand per target (from-dir missing = hard
  error), dedupe destinations byte-equal (hash file bytes at plan time),
  different bytes for one dest = hard error.
- **system-includes**: dep components' include dirs default to the manifest
  `system_includes` bucket (extraction A.1 does classification);
  `system-includes = false` on a dependency key forces `-I` at plan time;
  `system-includes = true` on a project target moves *its consumers'* view
  of its public includes to system.

Tests: layering-order goldens; cfg matrix (unix/linux/clang atoms against
synthetic truths); dev-edge violations; Threads closure; dedup-by-target
diamonds; `!` glob sets; runtime-data dedupe.

---

## 3. Bundle: ninja (`src/ninja_gen.rs` + `tests/ninja_test.rs`)

Implements: §4.2 gen edges + phony aggregate + restat, §6.5 copy edges,
consumes graph's ordered link inputs unchanged.

```rust
// write_ninja/write_compile_commands signatures unchanged — they read the
// new BuildPlan fields.
```

- New rule `cppkg-genexec`: `command = <cpp-pkg-exe> gen-exec --step <name>
  --project-root $root ...` (exact argv contract with integration agent's
  `gen-exec` subcommand: it re-reads the manifest, re-plans the single step,
  executes sandboxed, atomic-commits — ninja only needs stable argv +
  declared inputs/outputs). `restat = 1` (mtime preserved on unchanged
  bytes, §4.2). Edge inputs = declared inputs + template/stdin; output =
  `${gen}/<output>`.
- `build cppkg-gen: phony <all activated step outputs>`; every compile edge
  whose unit has `references_gen` gets `|| cppkg-gen` (order-only); depfiles
  give precision from build 2 (§4.2).
- Copy rule for `DataStage` (`cp` semantics via `cpp-pkg internal-copy` or
  plain `cp -p` — pick one, deterministic, restat-friendly); attached
  order-only to each `for_targets` member's output edge (§6.5: building the
  target always stages).
- Needs the cpp-pkg binary path: `write_ninja` gains a parameter:

```rust
pub fn write_ninja(plan: &BuildPlan, toolchain: &Toolchain, driver: &dyn Driver,
                   config: BuildConfig, build_dir: &Path,
                   cpp_pkg_exe: &Path) -> Result<()>;   // NEW last param
```

Tests: golden ninja snippets for a gen step (order-only + restat), a
runtime-data copy edge, and unchanged v0 output when the plan has no new
features (byte-stability guard).

---

## 4. Bundle: fetch-lock (`src/fetch.rs` + `src/lockfile.rs` + `tests/fetch_test.rs`)

Implements: §5.2 patch application + composed package id (via store-hash's
hasher), §5.2/§5.3 lockfile ABI additions, A.5 subdir plumbing (fetch side:
none — checkout root is returned as today; patches apply at checkout root
*before* anyone enters subdir), A.7 `.tar.xz`/`.tar.bz2`.

```rust
// fetch.rs
pub struct RawPackage {
    pub path: PathBuf,
    /// COMPOSED id when patches present: "<base>+patches:<blake3_32>" (§5.2).
    pub package_id: String,
    /// Base id (git sha / blake3:...) — feeds ${pin.*.commit} and lockfile
    /// `commit`/`content-hash` rows, which stay base-only.
    pub base_id: String,
}

/// `patch_files`: resolved absolute paths + their bytes, manifest order
/// (cli reads/validates existence; fetch verifies + applies). Applies with
/// `git apply -p1 --whitespace=nowarn` at checkout root into a temp dir,
/// atomic rename; pristine unpatched raw entry is never mutated. Hunk
/// failure => spec §5.2 error (dep, file, hunk, re-diff hint).
pub fn ensure(stores: &Stores, dep_key: &str, spec: &DependencySpec,
              locked: Option<&LockedPackage>,
              patches: &[(PathBuf, Vec<u8>)]) -> Result<RawPackage>;  // signature CHANGE
```

```rust
// lockfile.rs
pub struct LockedPackage {
    pub source: String,            // NEW legal value: "system"
    pub requested: String,         // system deps: "system" (+ constraints via min_version)
    pub commit: Option<String>,
    pub content_hash: Option<String>,
    pub patches: Vec<String>,      // NEW §5.2: "blake3:<hex>" rows, application order; empty = absent from file
    pub min_version: Option<String>, // NEW §5.3: declared constraint, machine-independent
}
```

- Grammar addendum (lock ABI, pinned): `source = "system"` rows carry
  `min-version` iff declared, and **never** resolved versions/paths/hashes
  (§5.3 coherence ruling — machine facts live in the sysdep store entry).
- Resolve re-verifies `patches` rows against current patch file bytes;
  drift → re-resolve → recompose id → rebuild (by design).
- Eager locking (§3.2): *cli* orchestrates, but `Lockfile` must accept rows
  for dev + cfg-inactive + system deps; `matching_entry` unchanged.
- A.7: extend `archive_kind`/`extract_archive` for `.tar.xz`/`.tar.bz2`
  (system tar).

Tests: patch apply happy path + failing hunk message + atomicity (failed
apply leaves no partial dir); rename-patch-file-no-rekey; lockfile
round-trip with patches + system rows; unknown-field rejection intact.

---

## 5. Bundle: store-hash (`src/store.rs` + `src/hashing.rs`)

Implements: §5.2 patch hash spine, §5.3 sysdep hash + sysdep store entries,
§8 discipline, A.8 extractor-versioned manifest cache paths, A.5 subdir hash
folding.

```rust
// hashing.rs
/// §5.2: blake3_32 over (u64-LE(len) || bytes) per patch, in order.
/// Composed id = format!("{base}+patches:{hex}").
pub fn patch_set_hash(patches: &[Vec<u8>]) -> String;
pub fn compose_patched_id(base_id: &str, patches: &[Vec<u8>]) -> String;

/// §5.3 — domain tag "cppkg-sysdep-v1". Length-prefixed canonical encoding
/// like config_hash. Header trees NOT hashed (documented gap).
pub struct SysdepHashInputs<'a> {
    pub key: &'a str,
    pub resolution_mode: &'a str,              // "cmake" in v1
    pub resolved_version: &'a str,
    pub library_paths: &'a [String],           // sorted
    pub library_hashes: &'a [String],          // blake3 of each file's bytes, same order
    pub include_dirs: &'a [String],            // sorted
}
pub fn sysdep_hash(inputs: &SysdepHashInputs) -> String;

pub struct ConfigHashInputs<'a> {
    /* all v0 fields byte-identically encoded, unchanged */
    /// NEW A.5: None => encoding is BYTE-IDENTICAL to v1-without-subdir (no
    /// count/len bytes emitted) — this is the one sanctioned conditional
    /// field, so no existing store entry re-keys. Some => append
    /// put_str("cppkg-subdir-v1"); put_str(subdir) after all v1 fields.
    pub subdir: Option<&'a str>,
}
```

```rust
// store.rs
impl Stores {
    /// Composed patched ids get a distinguishable dir: "absl-255c84da+a1b2c3d4"
    /// (first 8 of base, first 8 of patch hash). Unpatched naming unchanged.
    pub fn raw_dir(&self, dep_key: &str, package_id: &str) -> PathBuf;  // behavior extended

    /// §5.3 manifest-only system-dep entries: <root>/sysdeps/<key>-<hash8>/
    /// holding manifest.json + facts.json (resolved version/paths/hashes for
    /// re-validation). Never contains artifacts.
    pub fn sysdep_dir(&self, dep_key: &str, sysdep_hash: &str) -> PathBuf;

    /// A.8: extraction-manifest cache path inside an artifact entry now
    /// carries the extractor version: "manifest-e<EXTRACTOR_VERSION>.json".
    /// Old "manifest.json" files are simply never read again (re-derived
    /// cheaply); artifacts + config-hash keys untouched.
    pub fn manifest_path(&self, entry_dir: &Path) -> PathBuf;
}
```

Hash-discipline tests (**mandatory**): golden config-hash values for v0
inputs must not change (pin exact hex in a test); `subdir: None` ==
v0 encoding byte-for-byte; target flags / non-ABI `[flags]` / cfg / dev
markers provably absent from `ConfigHashInputs` (type-level: they simply
have no field — add a doc-test/comment asserting the §8 table); patch id
composition; sysdep hash stability.

---

## 6. Bundle: extraction (`src/manifest.rs` + `src/probe.rs` + tests)

Implements: A.1 (-isystem at ingestion), A.2 (self-edge no-op), A.3
(non-compilable INTERFACE_SOURCES skip), A.4 (config-not-found translation),
A.8 ($<BOOL:> in LINK_ONLY + extractor version), §5.3 sysdep probing, §5.4
Threads rewrite, §5.5 hermeticity scan layers.

```rust
// manifest.rs
/// A.8: bump whenever probe OUTPUT shape changes (this wave: 2).
pub const EXTRACTOR_VERSION: u32;

/// Ingestion transforms — applied inside Manifest::load AND at the end of
/// from_probe, so cached manifests written by any version get: A.1 include
/// classification into system_includes (spec: normative at READ side),
/// §5.4 Threads::Threads -> "builtin:threads" rewrite (+ attribution drop),
/// A.2 self-edge dedup. Idempotent by construction.
pub fn apply_ingestion_transforms(m: &mut Manifest);

/// §5.5 layer 1 — post-pass over link libs / include dirs / framework
/// paths of a manifest. `allow` carries store roots + declared-sysdep
/// paths. Absolute paths outside `allow` => Leak (cli errors by default;
/// --allow-undeclared-system-libs downgrades to warning).
pub struct Leak { pub component: String, pub path: PathBuf }
pub struct HermeticityAllow { pub store_roots: Vec<PathBuf>, pub sysdep_paths: Vec<PathBuf> }
pub fn scan_hermeticity(m: &Manifest, allow: &HermeticityAllow) -> Vec<Leak>;
```

```rust
// probe.rs
/// §5.3 v1 "cmake" resolution: find_package(<find-package or key>
/// [min_version]) with hermetic find restrictions OPENED for exactly this
/// package (cmake_build::find_control_args_for_sysdep). Returns the
/// interface manifest + the machine facts for the sysdep store entry.
pub struct SysdepFacts {
    pub resolved_version: String,
    pub library_paths: Vec<String>,     // sorted absolute
    pub library_hashes: Vec<String>,    // blake3, parallel
    pub include_dirs: Vec<String>,      // sorted
}
pub fn probe_system(dep_key: &str, spec: &DependencySpec, toolchain: &Toolchain)
    -> Result<(Manifest, SysdepFacts)>;
```

- A.4: not-found from a *system* dep probe emits the §5.3 both-worlds error
  (declare fetched, or `pacman -S`/`brew install`); ordinary probe
  config-not-found keeps the existing find-package hint, now documented.
- A.8 `$<BOOL:...>` evaluation inside `$<LINK_ONLY:...>`; stop replaying
  skip-notes on cache hits. §5.4 unexpected extracted Threads shapes →
  warning, literal interface kept.
- Version-rejection translation for `min-version` mirrors the existing
  find_dependency version error style.

Tests: transform idempotence; cached-manifest (old-shape JSON fixture) gains
system_includes + builtin:threads on load; leak scan fixtures (store-rooted
ok, /opt/homebrew hit, declared-sysdep allowlisted); $<BOOL> genex cases.

---

## 7. Bundle: toolchain (`src/toolchain.rs`)

Implements: §1.2 classifier (five classes + pass-through unwrapping), §2.1
truth derivation, §5.4 expansion table.

```rust
pub enum FlagClass { Abi, Sanitizer, Warning, OptDebug, Other }

pub struct ClassifiedWord {
    /// Index of the originating argv word (two-argv forms: the payload
    /// word's classification attaches to BOTH indices so schema can report
    /// the user's spelling).
    pub index: usize,
    /// The unwrapped payload actually classified (e.g. "-D_GLIBCXX_DEBUG"
    /// out of "-Wp,-D_GLIBCXX_DEBUG").
    pub payload: String,
    pub class: FlagClass,
}

/// §1.2: unwraps -Wl,/-Wa,/-Wp, (comma-split, classify each payload word)
/// and the two-argv -Xlinker/-Xpreprocessor/-Xassembler forms; -Wl,-style
/// transport itself is never Warning class. -W... (minus transports) and
/// -w => Warning; -O*, -g, -g[0-9], -ggdb*, -glldb* => OptDebug;
/// -fsanitize* => Sanitizer; existing ABI table (extended) => Abi;
/// everything else (unknown included) => Other (fail open).
pub fn classify_word_sequence(flags: &[String]) -> Vec<ClassifiedWord>;

// classify_flags (v0, profile-scope ABI extraction) remains; reimplement on
// top of classify_word_sequence so the ABI table exists once. [flags]-scope
// ABI extraction (cli) uses the same call.

impl ToolchainIdentity {
    /// §2.1: os parsed from target_triple (darwin=>Macos, linux=>Linux —
    /// gnu AND musl —, windows/msvc=>Windows); compiler from compiler_id
    /// with AppleClang => Clang. Unrecognized triple os => hard error
    /// naming the triple (closed vocabulary).
    pub fn cfg_truth(&self) -> Result<schema::CfgTruth>;
}

/// §5.4: pure function of the os axis (NOT the libc field). Linux =>
/// (compile: ["-pthread"], link: ["-pthread"]); Macos/Windows => empty.
pub fn threads_expansion(os: schema::CfgAtom) -> (&'static [&'static str], &'static [&'static str]);
```

Tests: transport unwrapping table (`-Wp,-D_GLIBCXX_DEBUG` → Abi;
`-Wl,-framework,X` → Other; `-Xlinker` pairs); AppleClang → Clang; musl
triple → Linux; classifier goldens for every §1.2 row.

---

## 8. Bundle: shim-export (`src/shim.rs`)

Implements: §6.2 staging, §6.3 emission + fixpoint, §6.4 header derivation,
§6.5 install-time runtime-data, export closure rules, patch staging.

```rust
/// Build a Manifest from the project's exported (install=true, non-dev)
/// targets, reading PlannedTarget's export metadata (public_includes/
/// defines/flags/cxx_std, link closure) — cfg already projected by graph.
/// Closure violations here: unexported local target in an exported closure,
/// dev target/dev-dep in closure, version missing while exporting a library
/// (§6.3 SameMajorVersion needs it).
pub fn manifest_from_project(project: &ProjectFile, plan: &BuildPlan) -> Result<Manifest>;

/// One staged file: src (build output / header / data / patch bytes) ->
/// prefix-relative dest. `--list` prints these without writing.
pub struct StageAction { pub src: StageSource, pub dest: PathBuf }
pub enum StageSource { File(PathBuf), Rendered(String) }   // Rendered: Config/Version/manifest.json text

pub struct InstallPlan { pub prefix: PathBuf, pub actions: Vec<StageAction> }

pub struct InstallRequest<'a> {
    pub project: &'a ProjectFile,
    pub plan: &'a BuildPlan,
    pub build_dir: &'a Path,
    pub prefix: &'a Path,               // baked into nothing absolute — relocatable emission
    pub lockfile: &'a Lockfile,         // requires rows: pins + options + patches (§6.3)
    pub patch_bytes: &'a BTreeMap<String, Vec<(String, Vec<u8>)>>, // dep key -> (blake3 id, bytes)
    pub sysdeps: &'a BTreeMap<String, /*facts summary*/ serde_json::Value>, // system requirement rows
    pub targets: &'a [String],          // empty = all exported
}
pub fn plan_install(req: &InstallRequest) -> Result<InstallPlan>;

/// Executes with DESTDIR composition: writes under <destdir><prefix> while
/// rendered content refers to <prefix>. Overwrite-by-default, idempotent,
/// never deletes (§6.2).
pub fn execute_install(plan: &InstallPlan, destdir: Option<&Path>) -> Result<()>;

/// §6.3(1,2,3): relocatable Config (_IMPORT_PREFIX), ConfigVersion
/// (SameMajorVersion), cppkg-manifest.json (@prefix@-relative; NO absolute
/// paths — hermeticity rule §6.4-end). Reuses/extends the v0 shim emitters;
/// the existing dep-shim path (provide) is untouched.
pub fn render_export_files(project: &ProjectFile, m: &Manifest, version: Option<&str>)
    -> Result<Vec<(PathBuf, String)>>;  // (lib/cmake/<Name>/..., content)
```

- Header derivation (§6.4): per exported library, walk each public include
  dir (incl. `${gen}` dirs — graph already resolved them absolute) for
  `.h .hpp .hh .hxx .inc .ipp` → `include/<rel>`; `public-headers` = total
  override via `graph::expand_patterns`; empty derivation = hard error;
  same-dest different-bytes = hard error; byte-equal dedupe; symlinks not
  followed.
- runtime-data → `share/<package>/<to>/` (same expansion the graph did —
  reuse `DataStage` inputs where possible).
- Patch staging: `lib/cmake/<CmakeName>/patches/<blake3-hex>.patch`; a
  requires row citing an id absent from the prefix = consume-time error
  (consume side lands with prefix-form deps — just stage + record now).
- Fixpoint: extend `shim_roundtrip_cmake_properties` to a project-target
  export → probe → compare-manifest test (§6.3 acceptance).
- The `install` CLI verb itself (arg parsing, build-then-stage) = integration.

---

## 9. Bundle: cmake-build (`src/cmake_build.rs`, minor)

Implements: A.5 subdir configure root, A.9 policy hint, §5.3/§5.5
find-control extensions.

```rust
pub struct DepBuildRequest<'a> {
    /* v0 fields unchanged */
    /// A.5: configure root = source_dir.join(subdir) when present (patches
    /// were applied at checkout root by fetch, before this).
    pub subdir: Option<&'a str>,
    /// §5.3/§5.5: dep keys declared `system = true` whose find_package
    /// names are allowed through the hermetic find restrictions, and whose
    /// recorded paths the leak scan must allowlist.
    pub sysdep_allow: &'a [SysdepAllow<'a>],
}
pub struct SysdepAllow<'a> { pub find_name: &'a str, pub paths: &'a [PathBuf] }

/// §5.3: find_control_args, parameterized — restrictions opened for exactly
/// the named packages (CMAKE_FIND_PACKAGE_PREFER_CONFIG etc. untouched).
pub fn find_control_args_for(sysdep_find_names: &[&str]) -> Vec<String>;

/// §5.5 layer 2: extended to *_LIBRARY / *_INCLUDE_DIR cache shapes with
/// the sysdep allowlist.
pub fn check_find_package_leaks(cache_path: &Path, allow: &[SysdepAllow]) -> Result<Vec<String>>;
```

- A.9: detect CMake ≥4's `cmake_minimum_required` refusal in configure
  output → error hint suggesting
  `options = { CMAKE_POLICY_VERSION_MINIMUM = "3.5" }`.
- A.10 misc: lint unknown dep `options` keys (compare against CMakeCache
  after configure; warning). Per-config project build dirs
  (`build/<config>/`) are a **cli/integration** concern (paths flow from
  cli); cmake_build takes dirs as given, no change.

---

## Cross-module call flows (the four tricky interactions)

### F1. cfg evaluation feeds schema validation AND graph

Load time (schema): parse + validate every group, matching or not
(vocabulary, key legality, reserved errors) — **no truth needed**. Plan time
(cli → graph): `toolchain.identity.cfg_truth()` → `PlanInputs.cfg_truth` →
graph builds effective targets (additive-append) → the same effective public
lists drive propagation (layer 5), local emission (6/7), and PlannedTarget
export metadata — one projection, read everywhere (§1.3 "one and only one
position"). Dep presence: cli filters provisioning by
`graph::required_deps(project, truth, request)`; lockfile still records
*all* declared deps (eager locking, §3.2).

### F2. generate steps feed ninja edges AND source lists

graph: interpolate step vars/argv (interp), compute activation set from the
requested targets' `${gen}` references + inter-step `${gen}` input edges,
validate activated inputs exist (plan-time error naming path), order steps
(cycle = error) → `BuildPlan.gen_steps`. ninja: one `cppkg-genexec` edge per
step (restat) + `cppkg-gen` phony + order-only deps on gen-referencing
compile edges. cli (integration): implements `gen-exec` (sandbox: macOS
sandbox-exec / Linux unshare -n, warn-and-degrade §4.2; temp-write + atomic
commit + declared-output verification) and the `gen` / `gen --check` verbs
for checked-in steps (which never enter BuildPlan).

### F3. patches feed fetch AND hashing AND lockfile AND export staging

cli reads patch files (schema-validated paths) → bytes. fetch::ensure
applies (temp dir + atomic rename; pristine raw kept) and returns
`package_id = hashing::compose_patched_id(base, bytes)` + `base_id`.
hashing: composed id enters `ConfigHashInputs.package_id` unchanged-encoding;
store::raw_dir renders the `+` suffix form. lockfile: `patches` rows =
per-file `blake3:<hex>`, order preserved; base commit stays in `commit`
(`${pin.*.commit}` reads base via interp ctx). shim-export: stages patch
bytes into the prefix and cites the ids in `cppkg-manifest.json` requires
rows.

### F4. manifest ingestion-transforms + extractor-version cache key

probe writes manifests through `manifest::from_probe` (A.2/A.3/A.8-BOOL
fixes live in probe/from_probe; EXTRACTOR_VERSION bumped to 2).
store::manifest_path embeds the version → warm stores re-derive (cli: if
versioned manifest absent but entry complete, re-probe the installed prefix
— cheap). Independently, `apply_ingestion_transforms` runs on EVERY
`Manifest::load` (A.1 -isystem, §5.4 Threads rewrite, A.2) so even
freshly-re-read old files converge; `scan_hermeticity` runs on probe output
and on every cached-manifest read (cli wires allow-list from store roots +
sysdep facts).

---

## Integration agent (cli.rs + lib.rs) — for completeness

New verbs: `test`, `install`, `gen`, `gen --check`, hidden `gen-exec`;
`build --prefix`, `--allow-undeclared-system-libs`; per-config build dirs
(A.10) `build/<config>/` with the §3.2 cwd rule anchored at `build/`;
eager-lock orchestration; sysdep provisioning (probe_system → sysdep store
entry → sysdep_hash into dependents' dep_hashes); `[flags]` ABI extraction
via classify_word_sequence feeding abi_flags; test runner (serial default,
argv spawn, env rule inherit→remove→set, expect-failure incl. signal death,
captured-output replay, `--` passthrough, FILTER-matches-nothing error).

---

## Acceptance gate (wave exit)

1. `cargo build` + full `cargo test` green; clippy clean.
2. All four `tests/projects/` green, **unedited** (v0 byte-compatibility).
3. Hash-discipline tests: pinned v0 config-hash hex unchanged; subdir=None
   encoding byte-identical; §8 table asserted feature-by-feature.
4. One smoke per feature (new inline/integration tests or scratch
   projects): target flags + fence errors; `[flags]` dedup/layering golden;
   a cfg.unix/cfg.linux source split planned on synthetic truths; dev-dep +
   `cpp-pkg test` run entry (expect-failure death test); template step +
   command step + checked-in `gen --check`; a patched dep building with
   composed store key + lockfile rows; a `system = true` dep probed and
   hashed (skip-if-uninstalled guard); `install --list` plan + fixpoint
   probe of an installed Config; `!` glob exclusion; `[target-defaults]`
   eligibility (dev target skipped by `install = true` default);
   Threads::Threads reference with zero declaration.
5. Spec §5.4 flag-day error (`exposes-targets = ["Threads::Threads"]`)
   fires with the exact fix message.
