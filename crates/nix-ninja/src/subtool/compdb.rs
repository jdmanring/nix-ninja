//! `nix-ninja -t compdb` - a JSON compilation database on stdout.
//!
//! This existed as a silent no-op until 2026-08-30, sharing an arm with the
//! tools that really are no-ops here. `compdb` is not one of them: it is the
//! only subtool besides `drv` whose whole purpose is to WRITE DATA to stdout,
//! and returning `Ok(())` without printing gave every caller an empty
//! `compile_commands.json` and an exit status of zero - the exact shape the
//! logging contract in `CLAUDE.md` warns about, where silence reads as data.
//!
//! It had already cost a package a workaround.
//! `modules/flake/examples/nix/default.nix` blanks out upstream nix's
//! `clean_compdb.py` with the comment "nix-ninja does not generate a
//! compile-commands.json, which causes clean_compdb.py to fail". That hack is
//! the acceptance test for this file: it should be removable.
//!
//! No daemon is needed. The graph alone carries every command, so this runs
//! off `load_file` and never opens an RPC connection.

use anyhow::Result;
use n2::graph::Graph;
use std::path::Path;

/// One `compile_commands.json` entry, in the fields the format requires.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub directory: String,
    pub command: String,
    pub file: String,
}

/// Build the entries for every edge that runs a command.
///
/// Phony edges carry no `cmdline` and are skipped, which is also how ninja
/// treats them.
///
/// `ninja -t compdb` takes optional RULE names to filter by, and n2 does not
/// retain rule names on a `Build`, so we cannot honour them. Emitting a
/// superset is harmless for every consumer of this format - they look entries
/// up by `file` - but ACCEPTING an argument and quietly not applying it is
/// not, so `run` says so on stderr rather than letting the caller believe a
/// filter took effect.
pub fn entries(graph: &Graph, build_dir: &Path) -> Vec<Entry> {
    let directory = build_dir.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for id in graph.builds.all_ids() {
        let build = &graph.builds[id];
        let Some(cmdline) = build.cmdline.as_ref() else {
            continue;
        };
        // Only edges that ask for DEPENDENCY SCANNING. `depfile`/`deps` are
        // set by the generator when a command discovers its own includes,
        // which in practice only compilers do, and that is exactly the set a
        // compilation database is for.
        //
        // Emitting every edge instead was measured on libvmaf and is not
        // merely noisy: 40 of 176 entries were archives (`gcc-ar`), links,
        // `xxd` codegen and meson's own regen rules, and SEVEN of them had a
        // `file` of `PHONY` or `all` - not files at all. A consumer indexing
        // that gets entries it cannot act on.
        //
        // ninja takes rule NAMES to make this selection, and n2 does not
        // retain rule names on a `Build`, so this is the same choice made
        // from what the graph does keep.
        if build.depfile.is_none() && build.deps.is_none() {
            continue;
        }
        // ninja keys an entry on the edge's first EXPLICIT input: the
        // translation unit. Implicit inputs are headers and tools, and an
        // entry keyed on one of those would point a consumer at the wrong
        // file.
        let Some(first) = build.dependencies.explicit_ins().first() else {
            continue;
        };
        out.push(Entry {
            directory: directory.clone(),
            command: cmdline.clone(),
            file: graph.file(*first).path().to_string_lossy().into_owned(),
        });
    }
    out
}

/// Serialise entries as the JSON array `compile_commands.json` is defined to
/// be. Written through `serde_json` so quoting and escaping in a command line
/// are the library's problem rather than ours.
pub fn to_json(entries: &[Entry]) -> Result<String> {
    let vals: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "directory": e.directory,
                "command": e.command,
                "file": e.file,
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&serde_json::Value::Array(
        vals,
    ))?)
}

/// Print the database. `rules` are the trailing arguments ninja would treat as
/// rule names to filter by.
pub fn run(graph: &Graph, build_dir: &Path, rules: &[String], expand_rspfile: bool) -> Result<()> {
    if expand_rspfile {
        // DEFER(the version string goes to 1.9 or above, at which point meson
        // starts sending this on every configure): -x asks for the rspfile's
        // contents to be spliced into the emitted command, so an entry for an
        // edge that uses one is INCOMPLETE without it. Saying so is the
        // difference between a known gap and a wrong answer sitting in a file
        // a tool will trust.
        eprintln!(
            "nix-ninja: -t compdb -x accepted, but rspfile contents are NOT              expanded; entries for edges using an rspfile are incomplete"
        );
    }
    if !rules.is_empty() {
        eprintln!(
            "nix-ninja: -t compdb cannot filter by rule name ({}), so every \
             edge that requests dependency scanning is emitted instead; \
             entries are keyed by file, so a superset is safe to consume",
            rules.join(" ")
        );
    }
    let entries = entries(graph, build_dir);
    eprintln!("nix-ninja: -t compdb emitted {} entries", entries.len());
    println!("{}", to_json(&entries)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use n2::load;
    use std::path::PathBuf;

    /// Load a `build.ninja` from bytes. `Loader::parse` wants a path only for
    /// error messages, so nothing needs to exist on disk and this avoids a
    /// dev-dependency that would write a line into `Cargo.lock` - which is
    /// inside `nix-ninja-task`'s fileset, and so inside its derivation key.
    /// The NUL is what `scanner::read_file_with_nul` appends; the scanner
    /// relies on it as a terminator.
    fn graph_of(text: &str) -> load::Loader {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        let mut loader = load::Loader::new();
        loader.parse(PathBuf::from("build.ninja"), &bytes).unwrap();
        loader
    }

    const NINJA: &str = "
rule cc
  command = cc -MD -MF $out.d -c $in -o $out
  depfile = $out.d
rule link
  command = cc $in -o $out

build a.o: cc a.c | header.h
build b.o: cc b.c
build c.o: cc | only_implicit.h
build prog: link a.o b.o
build all: phony prog
";

    /// The entry is keyed on the TRANSLATION UNIT, never on an implicit
    /// input.
    ///
    /// `c.o` is what gives this test teeth, and the first version had none.
    /// Asserting only that `a.o` (`cc a.c | header.h`) keys on `a.c` SURVIVES
    /// swapping `explicit_ins` for `dirtying_ins`: explicit inputs sort first
    /// in that slice, so `.first()` returns the same id either way. The
    /// mutation was run, and it lived. The distinction is observable only on
    /// an edge with NO explicit inputs, where `explicit_ins` yields nothing
    /// and the edge is skipped, while `dirtying_ins` yields the header and
    /// emits an entry telling a consumer that a `.h` is a translation unit.
    #[test]
    fn keys_on_the_first_explicit_input() {
        let loader = graph_of(NINJA);
        let es = entries(&loader.graph, &PathBuf::from("/build"));
        let a = es.iter().find(|e| e.command.contains("a.c")).unwrap();
        assert_eq!(a.file, "a.c");
        assert!(
            !es.iter().any(|e| e.file == "only_implicit.h"),
            "an implicit input became the entry's file: {es:#?}"
        );
    }

    /// Phony edges carry no command and must not appear. `all` is phony here,
    /// and an entry for it would name a file that is never compiled.
    #[test]
    fn emits_only_dependency_scanning_edges() {
        let loader = graph_of(NINJA);
        let es = entries(&loader.graph, &PathBuf::from("/build"));
        assert!(
            !es.iter().any(|e| e.file == "prog"),
            "phony edge emitted: {es:#?}"
        );
        // a.o and b.o. `prog` LINKS - no depfile, so not a compile - and
        // `all` is phony.
        assert_eq!(es.len(), 2, "{es:#?}");
        assert!(
            !es.iter().any(|e| e.command.starts_with("cc a.o")),
            "a link edge reached the database: {es:#?}"
        );
    }

    /// The directory is the build directory for every entry, because that is
    /// the directory the commands are relative to.
    #[test]
    fn directory_is_the_build_dir() {
        let loader = graph_of(NINJA);
        let es = entries(&loader.graph, &PathBuf::from("/somewhere/build"));
        assert!(es.iter().all(|e| e.directory == "/somewhere/build"));
    }

    /// The output has to be a JSON ARRAY of objects with exactly the three
    /// keys the format defines, and a command containing quotes must survive
    /// the trip. This is the guard against hand-rolled string building.
    #[test]
    fn emits_a_parseable_array_with_the_required_keys() {
        let es = vec![Entry {
            directory: "/build".into(),
            command: r#"cc -DMSG="a \"quoted\" thing" -c x.c"#.into(),
            file: "x.c".into(),
        }];
        let parsed: serde_json::Value = serde_json::from_str(&to_json(&es).unwrap()).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["file"], "x.c");
        assert_eq!(arr[0]["directory"], "/build");
        assert_eq!(arr[0]["command"], es[0].command);
    }
}
