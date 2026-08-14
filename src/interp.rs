//! `${...}` interpolation — the single resolver for codegen, testing, and
//! install positions (wave-1 spec §0.3; plan bundle 1).
//!
//! One grammar, closed vocabulary, whitelisted positions:
//! - `$${` escapes a literal `${`.
//! - An unknown variable is a hard error naming the vocabulary for the
//!   position — never an empty substitution.
//! - Placement policing (`${` outside whitelisted positions is an error)
//!   happens at schema load; *resolution* happens here, at plan/gen time,
//!   when the context (pins, gen root, prefixes) actually exists.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::bail;

use crate::Result;

/// Lockfile-derived identity of one dependency pin. `commit` is always the
/// BASE commit (never the patch-composed package id — spec §5.2: version
/// stamping wants upstream identity). `requested` is the human ref
/// (`v1.9.5` for `tag:`, the sha for `rev:`, `sha256:<hex>` for url deps).
#[derive(Debug, Clone)]
pub struct PinInfo {
    pub commit: String,
    pub requested: String,
}

/// Everything `${...}` can resolve from. Optional fields are `None` when the
/// caller's position can't legally use them; the resolver errors (never
/// empty-substitutes) if a variable's source is absent.
#[derive(Debug, Clone)]
pub struct InterpCtx<'a> {
    pub package_name: &'a str,
    pub package_version: Option<&'a str>,
    /// By dependency key.
    pub pins: &'a BTreeMap<String, PinInfo>,
    /// `${gen}` -> the generated-output root (build/gen).
    pub gen_root: Option<&'a Path>,
    pub project_root: Option<&'a Path>,
    pub build_dir: Option<&'a Path>,
    /// `${install-prefix}`; `None` uses the spec default `/usr/local`.
    pub install_prefix: Option<&'a str>,
}

/// The whitelisted positions (spec §0.3 table). Each admits a fixed variable
/// subset; anything else containing an unescaped `${` is a hard error
/// (schema polices placement at load, this enum polices vocabulary here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpPos {
    /// Define *values* (`KEY=VALUE` entries, after the `=`).
    DefineValue,
    /// `[generate.<name>]` template vars: package/pin identity only — the
    /// §0.3 table grants `${gen}` to generate *argv*, not vars.
    GenerateVar,
    /// `[generate.<name>]` command argv words (and stdin, which shares the
    /// argv vocabulary).
    GenerateArgv,
    /// `sources` / `includes` list entries — and `[generate.*].inputs`,
    /// which are file paths with the same `${gen}`-only vocabulary.
    SourceOrIncludeEntry,
    /// Run-entry `args` / `cwd` / `env` values.
    RunEntryValue,
}

impl InterpPos {
    /// Human-readable closed vocabulary for error messages.
    fn vocabulary(self) -> &'static str {
        match self {
            InterpPos::DefineValue => {
                "${package.name}, ${package.version}(.major/.minor/.patch), \
                 ${pin.<dep>.commit}, ${pin.<dep>.requested}, ${install-prefix}"
            }
            InterpPos::GenerateVar => {
                "${package.name}, ${package.version}(.major/.minor/.patch), \
                 ${pin.<dep>.commit}, ${pin.<dep>.requested}"
            }
            InterpPos::GenerateArgv => {
                "${package.name}, ${package.version}(.major/.minor/.patch), \
                 ${pin.<dep>.commit}, ${pin.<dep>.requested}, ${gen}"
            }
            InterpPos::SourceOrIncludeEntry => "${gen}",
            InterpPos::RunEntryValue => "${gen}, ${project-root}, ${build-dir}",
        }
    }
}

/// True if `text` contains an unescaped `${`. Schema validation uses this to
/// hard-error on `${` outside whitelisted positions.
pub fn contains_interp(text: &str) -> bool {
    let mut rest = text;
    loop {
        match rest.find("${") {
            None => return false,
            Some(i) => {
                // `$${` escapes: the `${` we found is escaped iff preceded
                // by an odd... no — the escape is exactly the 3-byte `$${`
                // sequence, so a `$` immediately before the match consumes it.
                if i > 0 && rest.as_bytes()[i - 1] == b'$' {
                    rest = &rest[i + 2..];
                } else {
                    return true;
                }
            }
        }
    }
}

/// Resolve every `${...}` in `text` for position `pos`. `$${` becomes a
/// literal `${`. Unknown variables, variables illegal for the position, and
/// absent context sources are hard errors. `${pin.self.*}` is reserved.
pub fn interpolate(text: &str, pos: InterpPos, ctx: &InterpCtx) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(i) = rest.find("${") else {
            out.push_str(rest);
            return Ok(out);
        };
        if i > 0 && rest.as_bytes()[i - 1] == b'$' {
            // `$${` -> literal `${`. Everything up to (excluding) the extra
            // `$`, then the literal braces.
            out.push_str(&rest[..i - 1]);
            out.push_str("${");
            rest = &rest[i + 2..];
            continue;
        }
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            bail!("unterminated '${{' in \"{text}\"");
        };
        let var = &after[..end];
        out.push_str(&resolve(var, pos, ctx, text)?);
        rest = &after[end + 1..];
    }
}

fn resolve(var: &str, pos: InterpPos, ctx: &InterpCtx, whole: &str) -> Result<String> {
    // `${pin.self.*}` is reserved in every position (spec §0.3): erroring
    // before the position check gives the better message.
    if var == "pin.self" || var.starts_with("pin.self.") {
        bail!(
            "${{{var}}} is reserved, not implemented: it is only meaningful \
             when built as a dependency; use ${{package.version}} in a root \
             build"
        );
    }

    let unknown = || -> anyhow::Error {
        anyhow::anyhow!(
            "unknown interpolation variable '${{{var}}}' in \"{whole}\"; \
             this position accepts: {}",
            pos.vocabulary()
        )
    };

    let allowed_package_and_pin = matches!(
        pos,
        InterpPos::DefineValue | InterpPos::GenerateVar | InterpPos::GenerateArgv
    );

    match var {
        "package.name" => {
            if !allowed_package_and_pin {
                return Err(unknown());
            }
            Ok(ctx.package_name.to_string())
        }
        "package.version" => {
            if !allowed_package_and_pin {
                return Err(unknown());
            }
            match ctx.package_version {
                Some(v) => Ok(v.to_string()),
                None => bail!(
                    "${{package.version}}: [package].version is not set; add \
                     version = \"...\" to [package]"
                ),
            }
        }
        "package.version.major" | "package.version.minor" | "package.version.patch" => {
            if !allowed_package_and_pin {
                return Err(unknown());
            }
            let Some(v) = ctx.package_version else {
                bail!(
                    "${{{var}}}: [package].version is not set; add \
                     version = \"...\" to [package]"
                );
            };
            let idx = match var {
                "package.version.major" => 0,
                "package.version.minor" => 1,
                _ => 2,
            };
            let component = v.split('.').nth(idx);
            match component.and_then(|c| c.parse::<u64>().ok()) {
                Some(n) => Ok(n.to_string()),
                None => bail!(
                    "${{{var}}}: version \"{v}\" has no integer component at \
                     that position"
                ),
            }
        }
        "gen" => {
            let ok = matches!(
                pos,
                InterpPos::GenerateArgv
                    | InterpPos::SourceOrIncludeEntry
                    | InterpPos::RunEntryValue
            );
            if !ok {
                return Err(unknown());
            }
            match ctx.gen_root {
                Some(p) => Ok(p.display().to_string()),
                None => bail!("${{gen}}: no generated-output root in this context"),
            }
        }
        "project-root" => {
            if pos != InterpPos::RunEntryValue {
                return Err(unknown());
            }
            match ctx.project_root {
                Some(p) => Ok(p.display().to_string()),
                None => bail!("${{project-root}}: no project root in this context"),
            }
        }
        "build-dir" => {
            if pos != InterpPos::RunEntryValue {
                return Err(unknown());
            }
            match ctx.build_dir {
                Some(p) => Ok(p.display().to_string()),
                None => bail!("${{build-dir}}: no build dir in this context"),
            }
        }
        "install-prefix" => {
            if pos != InterpPos::DefineValue {
                return Err(unknown());
            }
            Ok(ctx.install_prefix.unwrap_or("/usr/local").to_string())
        }
        _ => {
            if let Some(rest) = var.strip_prefix("pin.") {
                if !allowed_package_and_pin {
                    return Err(unknown());
                }
                let Some((key, field)) = rest.rsplit_once('.') else {
                    return Err(unknown());
                };
                if field != "commit" && field != "requested" {
                    return Err(unknown());
                }
                let Some(pin) = ctx.pins.get(key) else {
                    bail!(
                        "${{{var}}}: no locked pin for dependency '{key}' — is \
                         it declared, and has `cpp-pkg` resolved the lockfile?"
                    );
                };
                return Ok(match field {
                    "commit" => pin.commit.clone(),
                    _ => pin.requested.clone(),
                });
            }
            Err(unknown())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx<'a>(pins: &'a BTreeMap<String, PinInfo>) -> InterpCtx<'a> {
        InterpCtx {
            package_name: "vtz",
            package_version: Some("1.4.0"),
            pins,
            gen_root: None,
            project_root: None,
            build_dir: None,
            install_prefix: None,
        }
    }

    #[test]
    fn interp_contains_interp_and_escape() {
        assert!(contains_interp("${gen}/x"));
        assert!(contains_interp("a${b}c"));
        assert!(!contains_interp("plain"));
        assert!(!contains_interp("$${gen}")); // escaped
        // A `$` immediately before `${` always escapes the brace pair, so
        // `$$${gen}` is the literal `$${gen}` — never an interpolation.
        assert!(!contains_interp("$$${gen}"));
    }

    #[test]
    fn interp_escape_produces_literal() {
        let pins = BTreeMap::new();
        let got = interpolate("a$${gen}b", InterpPos::RunEntryValue, &ctx(&pins)).unwrap();
        assert_eq!(got, "a${gen}b");
    }

    #[test]
    fn interp_package_vars_in_define() {
        let pins = BTreeMap::new();
        let c = ctx(&pins);
        assert_eq!(
            interpolate("v${package.version}", InterpPos::DefineValue, &c).unwrap(),
            "v1.4.0"
        );
        assert_eq!(
            interpolate("${package.name}", InterpPos::DefineValue, &c).unwrap(),
            "vtz"
        );
        assert_eq!(
            interpolate("${package.version.major}", InterpPos::DefineValue, &c).unwrap(),
            "1"
        );
        assert_eq!(
            interpolate("${package.version.patch}", InterpPos::DefineValue, &c).unwrap(),
            "0"
        );
    }

    #[test]
    fn interp_version_absent_or_noninteger() {
        let pins = BTreeMap::new();
        let mut c = ctx(&pins);
        c.package_version = None;
        let e = format!(
            "{:#}",
            interpolate("${package.version}", InterpPos::DefineValue, &c).unwrap_err()
        );
        assert!(e.contains("version is not set"), "{e}");

        c.package_version = Some("1.x.0");
        let e = format!(
            "{:#}",
            interpolate("${package.version.minor}", InterpPos::DefineValue, &c).unwrap_err()
        );
        assert!(e.contains("integer"), "{e}");
    }

    #[test]
    fn interp_pin_vars() {
        let mut pins = BTreeMap::new();
        pins.insert(
            "date".to_string(),
            PinInfo { commit: "abc123".into(), requested: "v3.0.4".into() },
        );
        let c = ctx(&pins);
        assert_eq!(
            interpolate("${pin.date.commit}", InterpPos::GenerateArgv, &c).unwrap(),
            "abc123"
        );
        assert_eq!(
            interpolate("${pin.date.requested}", InterpPos::DefineValue, &c).unwrap(),
            "v3.0.4"
        );
        let e = format!(
            "{:#}",
            interpolate("${pin.ghost.commit}", InterpPos::DefineValue, &c).unwrap_err()
        );
        assert!(e.contains("'ghost'"), "{e}");
    }

    #[test]
    fn interp_pin_self_reserved_everywhere() {
        let pins = BTreeMap::new();
        let c = ctx(&pins);
        for pos in [InterpPos::DefineValue, InterpPos::GenerateArgv] {
            let e = format!(
                "{:#}",
                interpolate("${pin.self.requested}", pos, &c).unwrap_err()
            );
            assert!(e.contains("reserved"), "{e}");
            assert!(e.contains("${package.version}"), "hint missing: {e}");
        }
    }

    #[test]
    fn interp_position_gating() {
        let pins = BTreeMap::new();
        let mut c = ctx(&pins);
        let gen_root = PathBuf::from("/p/build/gen");
        c.gen_root = Some(&gen_root);

        // ${gen} legal in sources/includes, run values, generate argv...
        assert_eq!(
            interpolate("${gen}/src", InterpPos::SourceOrIncludeEntry, &c).unwrap(),
            "/p/build/gen/src"
        );
        // ...but not in define values.
        let e = format!(
            "{:#}",
            interpolate("${gen}/x", InterpPos::DefineValue, &c).unwrap_err()
        );
        assert!(e.contains("${install-prefix}"), "should list vocabulary: {e}");

        // package vars are not source/include material.
        let e = format!(
            "{:#}",
            interpolate("${package.name}.cpp", InterpPos::SourceOrIncludeEntry, &c)
                .unwrap_err()
        );
        assert!(e.contains("${gen}"), "{e}");
    }

    #[test]
    fn interp_run_entry_paths() {
        let pins = BTreeMap::new();
        let mut c = ctx(&pins);
        let root = PathBuf::from("/proj");
        let bd = PathBuf::from("/proj/build");
        c.project_root = Some(&root);
        c.build_dir = Some(&bd);
        assert_eq!(
            interpolate("${project-root}/x", InterpPos::RunEntryValue, &c).unwrap(),
            "/proj/x"
        );
        assert_eq!(
            interpolate("${build-dir}", InterpPos::RunEntryValue, &c).unwrap(),
            "/proj/build"
        );
    }

    #[test]
    fn interp_install_prefix_defaults() {
        let pins = BTreeMap::new();
        let c = ctx(&pins);
        assert_eq!(
            interpolate("${install-prefix}/share", InterpPos::DefineValue, &c).unwrap(),
            "/usr/local/share"
        );
        let mut c2 = ctx(&pins);
        c2.install_prefix = Some("/opt/x");
        assert_eq!(
            interpolate("${install-prefix}", InterpPos::DefineValue, &c2).unwrap(),
            "/opt/x"
        );
    }

    #[test]
    fn interp_unknown_and_unterminated() {
        let pins = BTreeMap::new();
        let c = ctx(&pins);
        let e = format!(
            "{:#}",
            interpolate("${cwd}", InterpPos::RunEntryValue, &c).unwrap_err()
        );
        assert!(e.contains("unknown interpolation variable"), "{e}");
        let e = format!(
            "{:#}",
            interpolate("${gen", InterpPos::RunEntryValue, &c).unwrap_err()
        );
        assert!(e.contains("unterminated"), "{e}");
    }
}
