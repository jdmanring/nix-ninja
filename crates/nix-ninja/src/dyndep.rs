//! Ninja `dyndep` support: dependencies a build edge cannot declare until
//! another edge has run.
//!
//! WHY THIS EXISTS. Fortran expresses compile ORDER through `.mod` files -
//! compiling `mymodule.f90` produces `mymodule.mod`, and any sibling with
//! `use mymodule` must be compiled after it. Nothing in `build.ninja` can
//! say so, because the module graph is only known once the sources have
//! been scanned. Ninja's answer is `dyndep`: an edge names a file that a
//! LATER-loaded build statement fills in with the missing implicit inputs
//! and outputs. C++20 modules use the same mechanism.
//!
//! This matters more here than under ninja. nix-ninja emits one derivation
//! per translation unit, and a derivation's inputs are fixed when it is
//! written. An edge whose real inputs are only in the dyndep file gets a
//! derivation that is missing them, so the compile fails inside the sandbox
//! with `Cannot open module file 'mymodule.mod'` - measured 2026-08-26 on
//! liblapack, which blocked the whole server closure.
//!
//! WHY IT IS PARSED HERE RATHER THAN IN n2. n2's loader drops the `dyndep`
//! binding, and adding a field to `graph::Build` would be the obvious fix.
//! It is not available: `nix-ninja-task`'s `src` fileset in
//! `modules/flake/overlays.nix` globs `vendor-n2/**/*.rs`, so ANY edit
//! under `vendor-n2/` re-keys `nix-ninja-task` and with it every banked
//! per-TU output in the store (~136,000 on the day this was written).
//! `crates/nix-ninja/` is outside that fileset, so everything here is free.
//! The binding is recovered by re-parsing the ninja files with n2's own
//! public parser - the same lexer, so escapes and line continuations cannot
//! drift between the two passes.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;

use n2::parse::{Parser, Statement};

/// What a dyndep file adds to one already-declared build edge.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DyndepEntry {
    /// Files the edge also produces, e.g. the `.mod` a Fortran compile
    /// writes beside its object file.
    pub implicit_outs: Vec<String>,
    /// Files the edge also consumes.
    pub implicit_ins: Vec<String>,
    /// Ninja's `restat`: re-stat outputs after running and treat an
    /// unchanged one as not dirtying dependents. Parsed so a dyndep file
    /// carrying it is accepted rather than rejected; it needs no
    /// implementation, because content-addressed derivations give the same
    /// early cutoff by construction - an unchanged output keeps its hash
    /// and dependents resolve to the derivation they already had.
    pub restat: bool,
}

/// n2's scanner requires a NUL sentinel at the end of its input;
/// `scanner::read_file_with_nul` supplies one when reading from disk. Text
/// held in memory (a dyndep file fetched from the store, a test fixture)
/// has to be given one here, and appending unconditionally would double it
/// for callers who already did.
fn with_nul(bytes: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(bytes.len() + 1);
    v.extend_from_slice(bytes);
    if v.last() != Some(&0) {
        v.push(0);
    }
    v
}

/// Accepted values of `ninja_dyndep_version`.
///
/// CMake 4.3 writes `1.0` and ninja's manual documents `1`; both are the
/// same version and a build must not fail on the spelling. Measured
/// against a real CMake-generated file rather than taken from memory -
/// the value in front of us was `1.0`, which is not what the manual shows.
fn version_is_supported(raw: &str) -> bool {
    matches!(raw.trim(), "1" | "1.0")
}

/// Parse a dyndep file into per-output additions.
///
/// The result is keyed by the edge's single explicit output, which is how
/// ninja identifies the edge being amended.
pub fn parse_dyndep(bytes: &[u8]) -> Result<HashMap<String, DyndepEntry>> {
    let buf = with_nul(bytes);
    let mut parser = Parser::new(&buf);
    let mut entries: HashMap<String, DyndepEntry> = HashMap::new();
    let mut saw_version = false;

    loop {
        // The version binding is a plain top-level assignment, so it lands
        // in `parser.vars` rather than arriving as a statement. It has to
        // be checked as soon as it appears and before any build statement
        // is trusted: a future format we cannot read would otherwise be
        // parsed as if it were version 1 and silently produce a WRONG
        // dependency set, which is worse than refusing the file.
        if !saw_version {
            // Top-level bindings are stored already evaluated, unlike the
            // per-build ones, so this is the String itself.
            if let Some(raw) = parser.vars.get("ninja_dyndep_version") {
                if !version_is_supported(raw) {
                    bail!("unsupported ninja_dyndep_version {raw:?} (expected 1)");
                }
                saw_version = true;
            }
        }

        let stmt = match parser.read().map_err(|e| anyhow!("{e:?}"))? {
            Some(s) => s,
            None => break,
        };

        let build = match stmt {
            Statement::Build(b) => b,
            // A dyndep file is a restricted ninja file: build statements
            // and the version binding, nothing else. Refuse the rest
            // rather than ignoring it, so a file we are misreading
            // announces itself here instead of downstream as a missing
            // dependency.
            Statement::Rule(_) => bail!("dyndep file must not declare rules"),
            Statement::Pool(_) => bail!("dyndep file must not declare pools"),
            Statement::Default(_) => bail!("dyndep file must not declare defaults"),
            Statement::Include(_) | Statement::Subninja(_) => {
                bail!("dyndep file must not include other files")
            }
        };

        if build.rule != "dyndep" {
            bail!(
                "dyndep file line {}: rule must be `dyndep`, got {:?}",
                build.line,
                build.rule
            );
        }
        if build.explicit_outs != 1 {
            bail!(
                "dyndep file line {}: expected exactly one explicit output, got {}",
                build.line,
                build.explicit_outs
            );
        }
        // Ninja allows only implicit inputs here; an explicit or order-only
        // input would mean we are reading a file whose shape we do not
        // understand.
        if build.explicit_ins != 0 || build.order_only_ins != 0 || build.validation_ins != 0 {
            bail!(
                "dyndep file line {}: only implicit inputs are allowed",
                build.line
            );
        }

        // n2's `eval` module is private, so `EvalString` cannot be named
        // here; its methods are public, so calling through inference works
        // and needs no edit to the vendored tree.
        let out = build.outs[0].evaluate(&[]);
        let implicit_outs: Vec<String> =
            build.outs[1..].iter().map(|s| s.evaluate(&[])).collect();
        let implicit_ins: Vec<String> =
            build.ins.iter().map(|s| s.evaluate(&[])).collect();
        let restat = build
            .vars
            .get("restat")
            .map(|v| {
                let s = v.evaluate(&[]);
                !s.is_empty() && s != "0"
            })
            .unwrap_or(false);

        let entry = DyndepEntry {
            implicit_outs,
            implicit_ins,
            restat,
        };
        if let Some(prev) = entries.insert(out.clone(), entry) {
            bail!("dyndep file names {out:?} twice (first: {prev:?})");
        }
    }

    if !saw_version {
        bail!("dyndep file has no ninja_dyndep_version binding");
    }

    Ok(entries)
}

/// Recover the `dyndep = <path>` bindings that n2's loader drops.
///
/// Keyed by the edge's first explicit output, which is what the loaded
/// graph can be joined on. Returns an empty map for the overwhelmingly
/// common case of a project with no dyndep at all, and the caller is
/// expected to skip the whole second pass then - see `mentions_dyndep`.
pub fn scan_bindings(bytes: &[u8]) -> Result<HashMap<String, String>> {
    let buf = with_nul(bytes);
    let mut parser = Parser::new(&buf);
    let mut out = HashMap::new();

    while let Some(stmt) = parser.read().map_err(|e| anyhow!("{e:?}"))? {
        let build = match stmt {
            Statement::Build(b) => b,
            _ => continue,
        };
        let Some(binding) = build.vars.get("dyndep") else {
            continue;
        };
        // Evaluated with an EMPTY scope, which resolves a literal path and
        // nothing else. CMake, which is the only generator known to emit
        // dyndep, writes a literal. A binding that references a variable
        // would silently evaluate to a path with holes in it, and a wrong
        // path here reproduces exactly the bug this module exists to fix,
        // so refuse instead: a build that stops with this message is a
        // request to implement rule-scope evaluation, not a mystery.
        let path = binding.evaluate(&[]);
        if path.is_empty() || path.contains("//") {
            bail!(
                "line {}: dyndep binding did not resolve to a literal path \
                 (got {path:?}); rule- or variable-scoped dyndep bindings are \
                 not implemented",
                build.line
            );
        }
        if build.explicit_outs == 0 {
            bail!("line {}: edge with a dyndep binding has no output", build.line);
        }
        let key = build.outs[0].evaluate(&[]);
        out.insert(key, path);
    }

    Ok(out)
}

/// Cheap pre-check so a project with no dyndep pays nothing for this
/// feature. The second parse is only worth its cost when the word appears
/// at all, and it does not appear in the ninja files of any package this
/// project builds today except the Fortran ones.
pub fn mentions_dyndep(bytes: &[u8]) -> bool {
    bytes
        .windows(b"dyndep".len())
        .any(|w| w == b"dyndep")
}

#[cfg(test)]
mod tests {
    use super::*;

    // GROUND TRUTH, not invented. Both fixtures are verbatim copies of
    // files written by cmake 4.3.4 + ninja 1.13.2 building
    // `share/cmake-4.3/Modules/FortranCInterface`, the probe that blocked
    // liblapack. That reference build succeeds (52/52), so these are the
    // bytes a working dyndep implementation has to agree with.
    // The originals are kept in ArtNix at
    // `docs/research/dyndep-ground-truth/`; they are inlined here because
    // the flake's `src` fileset globs only `**/*.rs`, so a fixture read
    // from disk would pass under `cargo test` and vanish in the flake
    // build - the failure mode `modules/flake/overlays.nix` already warns
    // about for the vendor dir.

    /// The PRODUCER side: declares the `.mod` files as implicit OUTPUTS.
    const MYFORT_DD: &str = "\
ninja_dyndep_version = 1.0
build CMakeFiles/myfort.dir/mysub.f.o: dyndep

build CMakeFiles/myfort.dir/my_sub.f.o: dyndep

build CMakeFiles/myfort.dir/mymodule.f90.o | mymodule.mod: dyndep
  restat = 1

build CMakeFiles/myfort.dir/my_module.f90.o | my_module.mod: dyndep
  restat = 1
";

    /// The CONSUMER side: declares the `.mod` files as implicit INPUTS.
    /// This is the edge whose missing inputs produced
    /// `Cannot open module file 'mymodule.mod'`.
    const CONSUMER_DD: &str = "\
ninja_dyndep_version = 1.0
build CMakeFiles/FortranCInterface.dir/main.F.o: dyndep

build CMakeFiles/FortranCInterface.dir/call_sub.f.o: dyndep

build CMakeFiles/FortranCInterface.dir/call_mod.f90.o: dyndep | my_module.mod mymodule.mod
";

    #[test]
    fn producer_side_declares_implicit_outputs() {
        let e = parse_dyndep(MYFORT_DD.as_bytes()).unwrap();
        assert_eq!(e.len(), 4);

        let m = &e["CMakeFiles/myfort.dir/mymodule.f90.o"];
        assert_eq!(m.implicit_outs, vec!["mymodule.mod"]);
        assert!(m.implicit_ins.is_empty());
        assert!(m.restat);

        // An edge with no additions still has to be PRESENT: its absence
        // and "this edge needs nothing" are different facts, and only the
        // first is an error worth reporting.
        let plain = &e["CMakeFiles/myfort.dir/mysub.f.o"];
        assert!(plain.implicit_outs.is_empty());
        assert!(plain.implicit_ins.is_empty());
        assert!(!plain.restat);
    }

    #[test]
    fn consumer_side_declares_implicit_inputs() {
        let e = parse_dyndep(CONSUMER_DD.as_bytes()).unwrap();
        let c = &e["CMakeFiles/FortranCInterface.dir/call_mod.f90.o"];
        // Order is the file's order; both are needed and neither is
        // implied by the other.
        assert_eq!(c.implicit_ins, vec!["my_module.mod", "mymodule.mod"]);
        assert!(c.implicit_outs.is_empty());
    }

    #[test]
    fn the_bare_manual_version_is_also_accepted() {
        // Ninja's manual documents `1`; CMake writes `1.0`. Failing on
        // either spelling would be a build failure with a confusing cause.
        let dd = "ninja_dyndep_version = 1\nbuild a.o: dyndep | b.mod\n";
        let e = parse_dyndep(dd.as_bytes()).unwrap();
        assert_eq!(e["a.o"].implicit_ins, vec!["b.mod"]);
    }

    #[test]
    fn an_unknown_version_is_refused_not_guessed() {
        let dd = "ninja_dyndep_version = 2\nbuild a.o: dyndep | b.mod\n";
        assert!(parse_dyndep(dd.as_bytes()).is_err());
    }

    #[test]
    fn a_missing_version_is_refused() {
        // Without the binding this is just a ninja file, and reading one
        // as a dyndep file would attribute dependencies to the wrong edges.
        let dd = "build a.o: dyndep | b.mod\n";
        assert!(parse_dyndep(dd.as_bytes()).is_err());
    }

    #[test]
    fn a_non_dyndep_rule_is_refused() {
        let dd = "ninja_dyndep_version = 1\nbuild a.o: cc b.c\n";
        assert!(parse_dyndep(dd.as_bytes()).is_err());
    }

    #[test]
    fn escaped_spaces_in_paths_survive() {
        // The whole reason this reuses n2's lexer instead of splitting on
        // whitespace: `$ ` is an escaped space inside one path, not a
        // separator between two.
        let dd = "ninja_dyndep_version = 1\nbuild my$ obj.o: dyndep | my$ mod.mod\n";
        let e = parse_dyndep(dd.as_bytes()).unwrap();
        assert_eq!(e["my obj.o"].implicit_ins, vec!["my mod.mod"]);
    }

    #[test]
    fn bindings_are_recovered_from_a_build_statement() {
        // Shaped like the real cmake output: the dyndep file is also an
        // ORDER-ONLY input of the edge, which is what ninja requires and
        // what makes the loaded graph already schedule the producer first.
        let ninja = "\
rule cc
  command = gcc -c $in -o $out

build CMakeFiles/myfort.dir/mymodule.f90.o: cc mymodule.f90 || CMakeFiles/myfort.dir/Fortran.dd
  dyndep = CMakeFiles/myfort.dir/Fortran.dd
";
        let b = scan_bindings(ninja.as_bytes()).unwrap();
        assert_eq!(
            b["CMakeFiles/myfort.dir/mymodule.f90.o"],
            "CMakeFiles/myfort.dir/Fortran.dd"
        );
    }

    #[test]
    fn an_edge_without_a_binding_contributes_nothing() {
        let ninja = "\
rule cc
  command = gcc -c $in -o $out

build a.o: cc a.c
";
        assert!(scan_bindings(ninja.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn the_cheap_precheck_agrees_with_the_scan() {
        // The precheck is what makes this feature free for every package
        // that has no dyndep, so a disagreement between it and the scan
        // would silently skip a project that needs the work.
        let with = "build a.o: cc a.c\n  dyndep = x.dd\n";
        let without = "build a.o: cc a.c\n";
        assert!(mentions_dyndep(with.as_bytes()));
        assert!(!mentions_dyndep(without.as_bytes()));
        assert!(!scan_bindings(with.as_bytes()).unwrap().is_empty());
        assert!(scan_bindings(without.as_bytes()).unwrap().is_empty());
    }
}
