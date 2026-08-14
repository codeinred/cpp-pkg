//! build.ninja + compile_commands.json emission (CPP_PKG_IMPLEMENTATION.md
//! §6). Regenerated unconditionally on every `cpp-pkg build` (v0).
//!
//! Conventions (mirror CMake's battle-tested Ninja output):
//! - One rule per (language, action): cxx_compile, c_compile, archive, link.
//! - Header deps: `deps = gcc` + `-MD -MT $out -MF $out.d` (GNU dialect).
//! - Per-unit flags in build statements (via per-build variables), not rules.
//! - Archive rule must `rm -f $out` first (ar appends otherwise).
//! - Paths with spaces: ninja `$ `-escaping ('$ ', '$:', '$$').
//! - compile_commands.json: one entry per CompileUnit, "arguments" array
//!   form (not "command" string), directory = build dir. Feeds --query.
//!
//! Escaping model: ninja itself shell-escapes `$in`/`$out` when expanding a
//! rule command, so those stay bare in rule text. Everything we substitute
//! ourselves (compiler paths, per-build variables like $flags/$libs) is
//! shell-quoted first and then '$'-escaped for ninja. Paths in build
//! *statements* (outputs/inputs/implicit deps) get ninja path escaping only —
//! no shell is involved there.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};

use crate::graph::{BuildPlan, CompileUnit, LinkInput};
use crate::schema::{BuildConfig, TargetKind};
use crate::toolchain::{Driver, Lang, Toolchain};
use crate::Result;

/// Write `<build_dir>/build.ninja`.
pub fn write_ninja(
    plan: &BuildPlan,
    toolchain: &Toolchain,
    driver: &dyn Driver,
    config: BuildConfig,
    build_dir: &Path,
) -> Result<()> {
    let text = render_ninja(plan, toolchain, driver, config, build_dir)?;
    std::fs::create_dir_all(build_dir)
        .with_context(|| format!("creating build dir {}", build_dir.display()))?;
    let path = build_dir.join("build.ninja");
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Write `<build_dir>/compile_commands.json` for the same plan.
pub fn write_compile_commands(
    plan: &BuildPlan,
    toolchain: &Toolchain,
    driver: &dyn Driver,
    config: BuildConfig,
    build_dir: &Path,
) -> Result<()> {
    #[derive(serde::Serialize)]
    struct Entry {
        directory: String,
        file: String,
        output: String,
        arguments: Vec<String>,
    }

    // Tools consuming the database (clangd, --query) resolve relative object
    // paths against `directory`, so it must be absolute even when the caller
    // passed a relative build dir.
    let directory = pstr(
        &std::path::absolute(build_dir)
            .with_context(|| format!("absolutizing {}", build_dir.display()))?,
    );

    let mut entries = Vec::new();
    for target in &plan.targets {
        for unit in &target.units {
            entries.push(Entry {
                directory: directory.clone(),
                file: pstr(&unit.source),
                output: pstr(&unit.object),
                arguments: unit_argv(unit, toolchain, driver, config)?,
            });
        }
    }

    std::fs::create_dir_all(build_dir)
        .with_context(|| format!("creating build dir {}", build_dir.display()))?;
    let mut json = serde_json::to_string_pretty(&entries)?;
    json.push('\n');
    let path = build_dir.join("compile_commands.json");
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Run ninja in `build_dir` (scrubbed env, cmake_build::scrubbed_env),
/// forwarding stdout/stderr to the user. Nonzero exit -> error.
pub fn run_ninja(build_dir: &Path, targets: &[String]) -> Result<()> {
    // cmake_build::scrubbed_env owns the scrubbing policy. Modules are brought
    // up independently, so tolerate it being unimplemented (panicking) and
    // degrade to a minimal inline environment rather than aborting the build.
    let env = match std::panic::catch_unwind(crate::cmake_build::scrubbed_env) {
        Ok(env) => env,
        Err(_) => fallback_env(),
    };

    let status = Command::new("ninja")
        .current_dir(build_dir)
        .env_clear()
        .envs(&env)
        // Target names double as ninja phony aliases (or as the output node
        // itself when name and output path coincide), so they pass through.
        .args(targets)
        .status()
        .with_context(|| format!("failed to run ninja in {}", build_dir.display()))?;
    if !status.success() {
        bail!("ninja failed ({}) in {}", status, build_dir.display());
    }
    Ok(())
}

/// The exact argv ninja runs for one compile unit. Shared by build.ninja
/// emission and compile_commands.json so the two can never drift.
pub fn unit_argv(
    unit: &CompileUnit,
    toolchain: &Toolchain,
    driver: &dyn Driver,
    config: BuildConfig,
) -> Result<Vec<String>> {
    let compiler = match unit.lang {
        Lang::Cxx => &toolchain.cxx,
        Lang::C => &toolchain.cc,
    };
    let mut argv = vec![pstr(compiler)];
    argv.extend(unit_flags(unit, toolchain, driver, config)?);
    argv.extend(unit_include_args(unit, driver));
    argv.extend(unit_define_args(unit, driver));
    argv.extend(driver.depfile_args(&unit.object, &depfile_path(&unit.object)));
    argv.push("-c".into());
    argv.push(pstr(&unit.source));
    argv.push("-o".into());
    argv.push(pstr(&unit.object));
    Ok(argv)
}

// ---------------------------------------------------------------------------
// build.ninja rendering

fn render_ninja(
    plan: &BuildPlan,
    toolchain: &Toolchain,
    driver: &dyn Driver,
    config: BuildConfig,
    build_dir: &Path,
) -> Result<String> {
    let mut f = String::new();
    let _ = writeln!(f, "# Generated by cpp-pkg; regenerated on every build — do not edit.");
    let _ = writeln!(f, "ninja_required_version = 1.5");
    let _ = writeln!(f);

    let cxx = shell_word(&pstr(&toolchain.cxx));
    let cc = shell_word(&pstr(&toolchain.cc));
    let ar = shell_word(&pstr(&toolchain.ar));

    // The depfile flags in the rule come from the same driver call unit_argv
    // uses, with ninja's own $out placeholders standing in for the paths.
    let dep_tail = driver
        .depfile_args(Path::new("$out"), Path::new("$out.d"))
        .join(" ");

    for (rule, compiler, what) in [("cxx_compile", &cxx, "CXX"), ("c_compile", &cc, "CC")] {
        let _ = writeln!(f, "rule {rule}");
        let _ = writeln!(
            f,
            "  command = {compiler} $flags $includes $defines {dep_tail} -c $in -o $out"
        );
        let _ = writeln!(f, "  depfile = $out.d");
        let _ = writeln!(f, "  deps = gcc");
        let _ = writeln!(f, "  description = {what} $out");
        let _ = writeln!(f);
    }

    // `ar q` appends to an existing archive, so remove it first or stale
    // members survive. Apple's ar writes no symbol table for `qc` alone; the
    // trailing `ar s` is the ranlib step.
    let _ = writeln!(f, "rule archive");
    let _ = writeln!(f, "  command = rm -f $out && {ar} qc $out $in && {ar} s $out");
    let _ = writeln!(f, "  description = AR $out");
    let _ = writeln!(f);

    for (rule, compiler) in [("link_cxx", &cxx), ("link_c", &cc)] {
        let _ = writeln!(f, "rule {rule}");
        let _ = writeln!(f, "  command = {compiler} $link_flags -o $out $in $libs");
        let _ = writeln!(f, "  description = LINK $out");
        let _ = writeln!(f);
    }

    // Link plans may reference sibling artifacts by absolute path. Rewrite
    // those to the spelling used in the sibling's own build statement so
    // ninja connects the producing edge instead of demanding a pre-existing
    // source file on the first build.
    let mut out_alias: BTreeMap<PathBuf, String> = BTreeMap::new();
    for t in &plan.targets {
        let rel = pstr(&t.output);
        out_alias.insert(t.output.clone(), rel.clone());
        out_alias.insert(build_dir.join(&t.output), rel);
    }

    let mut defaults: Vec<String> = Vec::new();
    for target in &plan.targets {
        for unit in &target.units {
            let rule = match unit.lang {
                Lang::Cxx => "cxx_compile",
                Lang::C => "c_compile",
            };
            let _ = writeln!(
                f,
                "build {}: {} {}",
                ninja_path(&pstr(&unit.object)),
                rule,
                ninja_path(&pstr(&unit.source))
            );
            push_var(&mut f, "flags", &unit_flags(unit, toolchain, driver, config)?);
            push_var(&mut f, "includes", &unit_include_args(unit, driver));
            push_var(&mut f, "defines", &unit_define_args(unit, driver));
        }

        let mut implicit = Vec::new();
        for dep in &target.target_deps {
            let dep_target = plan
                .targets
                .iter()
                .find(|t| t.name == *dep)
                .ok_or_else(|| {
                    anyhow!(
                        "target '{}' depends on '{}', which is not in the build plan",
                        target.name,
                        dep
                    )
                })?;
            implicit.push(ninja_path(&pstr(&dep_target.output)));
        }

        let out_escaped = ninja_path(&pstr(&target.output));
        match target.kind {
            TargetKind::StaticLibrary => {
                // An archive holds only this target's own objects; dependency
                // artifacts propagate through the plan to the final link, and
                // `ar` would store nested archives/libs uselessly.
                let mut ins = Vec::new();
                for li in &target.link_inputs {
                    match li {
                        LinkInput::Object(p) => ins.push(ninja_path(&pstr(p))),
                        other => bail!(
                            "static library '{}' has non-object link input {:?}; \
                             archives take only the target's own objects",
                            target.name,
                            other
                        ),
                    }
                }
                write_build(&mut f, &out_escaped, "archive", &ins, &implicit);
            }
            TargetKind::Executable => {
                let mut ins = Vec::new();
                let mut libs: Vec<String> = Vec::new();
                for li in &target.link_inputs {
                    match li {
                        LinkInput::Object(p) => {
                            let spelled =
                                out_alias.get(p.as_path()).cloned().unwrap_or_else(|| pstr(p));
                            ins.push(ninja_path(&spelled));
                        }
                        // Archives/dylibs ride $libs, not $in: the plan's
                        // rule-5 ordering interleaves them with -l/-framework
                        // entries, and `-o $out $in $libs` would otherwise
                        // regroup every path input before every flag — wrong
                        // for single-pass linkers when a -l resolves to a
                        // static archive. They join the implicit-deps list so
                        // ninja still schedules producers and re-links on
                        // change.
                        LinkInput::Archive(p) | LinkInput::Dylib(p) => {
                            let spelled =
                                out_alias.get(p.as_path()).cloned().unwrap_or_else(|| pstr(p));
                            libs.push(spelled.clone());
                            let escaped = ninja_path(&spelled);
                            if !implicit.contains(&escaped) {
                                implicit.push(escaped);
                            }
                        }
                        LinkInput::SystemLib(name) => libs.push(format!("-l{name}")),
                        LinkInput::Framework(name) => libs.extend(driver.framework_args(name)),
                    }
                }
                let rule = match target.link_lang {
                    Lang::Cxx => "link_cxx",
                    Lang::C => "link_c",
                };
                write_build(&mut f, &out_escaped, rule, &ins, &implicit);
                push_var(&mut f, "libs", &libs);
                push_var(&mut f, "link_flags", &target.link_flags);
            }
        }

        // Alias so `ninja <target-name>` works; skipped when the output IS
        // the name (a phony whose input equals its output is a ninja error).
        if target.name != pstr(&target.output) {
            let _ = writeln!(f, "build {}: phony {}", ninja_path(&target.name), out_escaped);
            defaults.push(ninja_path(&target.name));
        } else {
            defaults.push(out_escaped);
        }
        let _ = writeln!(f);
    }

    if !defaults.is_empty() {
        let _ = writeln!(f, "default {}", defaults.join(" "));
    }
    Ok(f)
}

fn write_build(f: &mut String, out: &str, rule: &str, ins: &[String], implicit: &[String]) {
    let mut line = format!("build {out}: {rule}");
    for i in ins {
        line.push(' ');
        line.push_str(i);
    }
    if !implicit.is_empty() {
        line.push_str(" |");
        for i in implicit {
            line.push(' ');
            line.push_str(i);
        }
    }
    line.push('\n');
    f.push_str(&line);
}

/// Emit `  key = <shell-quoted words>` on nonempty; rules reference absent
/// variables as empty, so nothing is emitted for empty lists.
fn push_var(f: &mut String, key: &str, words: &[String]) {
    if words.is_empty() {
        return;
    }
    let joined = words.iter().map(|w| shell_word(w)).collect::<Vec<_>>().join(" ");
    let _ = writeln!(f, "  {key} = {joined}");
}

// ---------------------------------------------------------------------------
// Per-unit argv pieces (shared by rule/variable emission and unit_argv)

fn unit_flags(
    unit: &CompileUnit,
    toolchain: &Toolchain,
    driver: &dyn Driver,
    config: BuildConfig,
) -> Result<Vec<String>> {
    let mut flags = Vec::new();
    if let Some(std) = unit.std {
        flags.push(driver.std_flag(unit.lang, std)?);
    }
    flags.extend(driver.sysroot_args(toolchain.sdk_path.as_deref()));
    flags.extend(driver.config_compile_flags(config));
    // Last, so profile/target extras can override config defaults.
    flags.extend(unit.extra_flags.iter().cloned());
    Ok(flags)
}

fn unit_include_args(unit: &CompileUnit, driver: &dyn Driver) -> Vec<String> {
    unit.includes
        .iter()
        .flat_map(|(path, system)| driver.include_args(path, *system))
        .collect()
}

fn unit_define_args(unit: &CompileUnit, driver: &dyn Driver) -> Vec<String> {
    unit.defines
        .iter()
        .map(|(key, value)| driver.define_arg(key, value.as_deref()))
        .collect()
}

fn depfile_path(object: &Path) -> PathBuf {
    PathBuf::from(format!("{}.d", object.to_string_lossy()))
}

// ---------------------------------------------------------------------------
// Escaping

fn pstr(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Ninja path escaping for build-statement outputs/inputs.
fn ninja_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '$' => out.push_str("$$"),
            ' ' => out.push_str("$ "),
            ':' => out.push_str("$:"),
            _ => out.push(c),
        }
    }
    out
}

/// POSIX-shell quoting for one argv word.
fn shell_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./+=:@%,".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// One argv word as it appears inside a ninja variable value: shell-quoted
/// (ninja hands commands to /bin/sh), then '$'-escaped so ninja passes any
/// literal dollar through.
fn shell_word(s: &str) -> String {
    shell_quote(s).replace('$', "$$")
}

fn fallback_env() -> BTreeMap<String, String> {
    // Minimal env for running ninja + compilers: deliberately drops
    // CC/CXX/CFLAGS/CXXFLAGS/LDFLAGS/CMAKE_* so host configuration cannot
    // leak into builds. DEVELOPER_DIR/SDKROOT stay: Apple's tool shims
    // consult them.
    const KEEP: &[&str] = &[
        "PATH", "HOME", "TMPDIR", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "LOGNAME",
        "DEVELOPER_DIR", "SDKROOT",
    ];
    let mut env = BTreeMap::new();
    for key in KEEP {
        if let Ok(value) = std::env::var(key) {
            env.insert((*key).to_string(), value);
        }
    }
    env.entry("PATH".to_string())
        .or_insert_with(|| "/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    env
}

// ---------------------------------------------------------------------------

// Behavior-level tests (golden fragments, compile_commands equivalence, a
// real end-to-end ninja run) live in tests/ninja_test.rs: they exercise only
// the public API, and the integration-test target builds the lib without
// cfg(test) so they stay runnable while sibling modules' inline tests are in
// flux. The inline tests below cover the private escaping helpers.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ninja_escape_helpers() {
        assert_eq!(ninja_path("a b:c$d"), "a$ b$:c$$d");
        assert_eq!(shell_quote("-std=c++20"), "-std=c++20");
        assert_eq!(shell_quote("/inc dir"), "'/inc dir'");
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
        assert_eq!(shell_word("a$b c"), "'a$$b c'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn ninja_depfile_path_appends_d() {
        assert_eq!(depfile_path(Path::new("obj/a b.o")), Path::new("obj/a b.o.d"));
    }
}
