use anyhow::Result;
use clap::Parser;
use harmonia_store_path::StoreDir;
use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};
use nix_ninja_task::patchelf;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(author, disable_version_flag = true)]
pub struct Cli {
    /// Specify the Nix store directory.
    #[arg(long = "store-dir", env = "NIX_STORE", default_value = "/nix/store")]
    pub store_dir: StoreDir,

    /// Directory prefix to recreate sources via symlinks.
    #[arg(long = "build-dir", default_value = "/build/source/build")]
    pub build_dir: PathBuf,

    /// Optional build target description.
    #[arg(long)]
    pub description: Option<String>,

    // Encoded derived files to prepare the source directory.
    #[arg(long, env = "NIX_NINJA_INPUTS")]
    pub inputs: String,

    // Encoded derived files that build outputs should be copied to.
    #[arg(long, env = "NIX_NINJA_OUTPUTS")]
    pub outputs: String,

    // Command to run.
    pub cmdline: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    fs::create_dir_all(&cli.build_dir)?;
    std::env::set_current_dir(&cli.build_dir)?;

    let mut inputs = Vec::new();
    for encoded in cli.inputs.split_whitespace() {
        // println!("Processing input {}", encoded);
        let input = DerivedFile::from_encoded(&cli.store_dir, encoded)?;
        inputs.push(input);
    }

    let mut outputs = Vec::new();
    for encoded in cli.outputs.split_whitespace() {
        // println!("Processing output {}", encoded);
        let output = DerivedFile::from_encoded(&cli.store_dir, encoded)?;
        outputs.push(output);
    }

    // The source directory of the derivation needs to have all build inputs
    // symlinked while preserving the original directory hierarchy of the
    // sources. This ensures relative includes and other path-dependent
    // references remain valid.
    create_symlinks(&cli.build_dir, &cli.store_dir, inputs, false)?;
    println!(
        "nix-ninja-task: Setup source directory in {}",
        cli.build_dir.display()
    );

    // Outputs are written to the same directory structure as the build
    // directory because if the output is a shared library the filename must
    // match the soname and it must be in a directory to add to the linking
    // binary's RUNPATH.
    create_output_dirs(&outputs)?;

    if let Some(desc) = cli.description {
        println!("nix-ninja-task: {desc}");
    }

    // CMake's autogen plan carries absolute configure-time paths that
    // nothing else rewrites. Done here, after inputs are materialised and
    // before the command runs, because it REPLACES the materialised file.
    rewrite_autogen_info(&cli.cmdline, &cli.build_dir)?;

    // Spawn cmdline process via sh like ninja upstream does.
    println!("nix-ninja-task: Running: /bin/sh -c \"{}\"", cli.cmdline);
    let exit_code = spawn_process(&cli.cmdline)?;
    if exit_code != 0 {
        println!("nix-ninja-task: Failed with exit code {exit_code}");
        std::process::exit(exit_code);
    }

    // Fix ELF RPATH to ensure it's linked against /nix/store paths rather
    // than relative path binaries in the build dir.
    patchelf::fix_rpaths(cli.store_dir.to_path(), &outputs)?;

    // Outputs must be created in build directory and then copied out because
    // ninja build rules can have implicit outputs that we have no way of
    // knowing. For example, a custom command that doesn't leverage the `$out`
    // implicit variable in the ninja evaluation context.
    println!(
        "nix-ninja-task: Finished! Copying {} build outputs to derivation output paths",
        outputs.len(),
    );
    copy_outputs_to_placeholders(&cli.store_dir, &outputs)?;

    Ok(())
}

/// The directory a `cd <dir> && ...` prologue names, when it is relative.
/// Returns None for a command with no prologue and for an absolute target,
/// which is not ours to create.
fn cd_prologue_dir(cmdline: &str) -> Option<&str> {
    let after = cmdline.strip_prefix("cd ")?;
    let (dir, _) = after.split_once(" && ")?;
    // An absolute target is a sandbox escape UNLESS it is under /build,
    // which is this task's own writable tree: under the exact mirror the
    // driver ships the original command line, whose cd target is the
    // configure-time `/build/<src>/src/...` (qtsvg, 2026-08-23: "can't cd
    // to /build/.../src/plugins/iconengines/svgiconengine").
    if dir.is_empty() || (dir.starts_with('/') && !dir.starts_with("/build/")) {
        return None;
    }
    Some(dir)
}

/// The value of a top-level `"KEY" : "value"` in a JSON text, without a JSON
/// parser. CMake writes one key per line and never escapes a path, so this is
/// exact for the file it is used on and deliberately narrow: it returns None
/// the moment the shape is not what it expects, and the caller then leaves the
/// file alone rather than half-rewriting it.
fn json_string_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let after = &text[text.find(&needle)? + needle.len()..];
    let after = after.trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let after = after.strip_prefix('"')?;
    let end = after.find('"')?;
    Some(&after[..end])
}

/// Resolve `.` and `..` textually in an absolute path.
fn lexical_normalize(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    format!("/{}", out.join("/"))
}

/// The `../` chain and remainder from `base` to `path`, both absolute and
/// both already normalised by cmake. Falls back to `path` itself when they
/// share no root, which the caller then normalises to an absolute result.
fn pathdiff_lexical(path: &str, base: &str) -> String {
    let pc: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    let bc: Vec<&str> = base.split('/').filter(|c| !c.is_empty()).collect();
    let common = pc.iter().zip(bc.iter()).take_while(|(a, b)| a == b).count();
    let mut out: Vec<String> = vec!["..".into(); bc.len() - common];
    out.extend(pc[common..].iter().map(|s| (*s).to_string()));
    if out.is_empty() {
        ".".into()
    } else {
        out.join("/")
    }
}

/// CMAKE'S AUTOGEN PLAN IS A THIRD CARRIER OF ABSOLUTE PATHS, and until this
/// existed nothing rewrote it.
///
/// nix-ninja rewrites absolute ancestor prefixes in the COMMAND LINE, and it
/// rewrites and re-ships RSPFILE CONTENT. A Qt module's automoc step reads
/// neither: cmake writes `AutogenInfo.json` at CONFIGURE time with the paths
/// it saw then, and a task materialises the same tree at a different root, so
/// every path in the file dangles. cmake reports it as
///
///     The header file "SRC:/build/include/QtSvg/qtsvgexports.h" does not exist.
///
/// which reads as a missing generated header while the header is present in
/// the task's own tree under a different prefix.
///
/// Absolute-to-absolute, not absolute-to-relative: the file's consumer is
/// cmake's own autogen driver, which resolves these against nothing in
/// particular, and a relative spelling would depend on a working directory
/// this code does not own.
///
/// The mapping comes from the file itself. `CMAKE_BINARY_DIR` is what the
/// build dir was called at configure time and `build_dir` is what it is now;
/// `CMAKE_SOURCE_DIR` keeps its relationship to the binary dir, so the same
/// relative step from the new binary dir lands on the new source dir. Deepest
/// prefix first, because a shallower one is a prefix of it.
///
/// The rewritten file REPLACES the materialised symlink, so the command line
/// needs no change and nothing else has to learn a second spelling.
fn rewrite_autogen_info(cmdline: &str, build_dir: &Path) -> Result<()> {
    if !cmdline.contains("cmake_autogen") {
        return Ok(());
    }
    // The path is written to resolve from the command's own cwd, which is the
    // cd prologue's target when there is one.
    let base = match cd_prologue_dir(cmdline) {
        Some(d) => build_dir.join(d),
        None => build_dir.to_path_buf(),
    };
    for tok in cmdline.split_whitespace() {
        let tok = tok.trim_matches('"');
        if !tok.ends_with("AutogenInfo.json") {
            continue;
        }
        let path = base.join(tok);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            // Not ours to diagnose: the command is about to run and will
            // report a missing plan far better than this would.
            Err(_) => continue,
        };
        let (jb, js) = match (
            json_string_value(&text, "CMAKE_BINARY_DIR"),
            json_string_value(&text, "CMAKE_SOURCE_DIR"),
        ) {
            (Some(b), Some(s)) if b.starts_with('/') && s.starts_with('/') => {
                (b.to_string(), s.to_string())
            }
            _ => {
                println!(
                    "nix-ninja-task: {} has no absolute CMAKE_BINARY_DIR/CMAKE_SOURCE_DIR pair, \
                     leaving it alone",
                    path.display()
                );
                continue;
            }
        };
        let actual_b = build_dir.to_string_lossy().into_owned();
        // The source dir's relationship to the binary dir is preserved.
        let rel = pathdiff_lexical(&js, &jb);
        let actual_s = lexical_normalize(&format!("{actual_b}/{rel}"));
        // ONE PASS, VIA A PLACEHOLDER, because two sequential replaces
        // corrupt each other here. The source dir is a PREFIX of the binary
        // dir (`/build/src` and `/build/src/build`), and the replacement for
        // the binary dir CONTAINS the source dir's text - measured:
        //     /build/src/build/include/x.h
        //   -> /build/source/build/src/build/include/x.h   (binary dir done)
        //   -> /build/source/build/source/build/src/build/include/x.h
        // once the source-dir replace runs over the result. cmake then
        // reports the header as missing relative to a mangled binary dir,
        // which reads as the rewrite not having happened at all.
        // The placeholder cannot occur in a path, so the second replace
        // cannot see the first's output.
        const HOLD: &str = "\u{0}NN_BINDIR\u{0}";
        let out = text
            .replace(jb.as_str(), HOLD)
            .replace(js.as_str(), actual_s.as_str())
            .replace(HOLD, actual_b.as_str());
        // The materialised input is a read-only store symlink; replace it.
        let _ = fs::remove_file(&path);
        fs::write(&path, &out).map_err(|e| anyhow::anyhow!("rewriting {}: {e}", path.display()))?;
        println!(
            "nix-ninja-task: rewrote autogen plan {} ({} bytes)",
            path.display(),
            out.len()
        );
    }
    Ok(())
}

fn copy_outputs_to_placeholders(store_dir: &StoreDir, outputs: &[DerivedFile]) -> Result<()> {
    for output in outputs {
        let target_path = output.absolute_path(store_dir);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&output.build_path, &target_path)?;
    }
    Ok(())
}

fn create_output_dirs(outputs: &Vec<DerivedFile>) -> Result<()> {
    let mut dirs: Vec<&std::path::Path> = Vec::new();
    for output in outputs {
        if let Some(parent) = output.build_path.parent() {
            if dirs.contains(&parent) {
                continue;
            }
            std::fs::create_dir_all(parent)?;
            dirs.push(parent);
        }
    }
    Ok(())
}

fn spawn_process(cmdline: &str) -> Result<i32> {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", cmdline])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .envs(env::vars());

    let output = cmd.status()?;
    Ok(output.code().unwrap_or(1))
}

#[cfg(test)]
mod autogen_tests {
    use super::{cd_prologue_dir, json_string_value, lexical_normalize, pathdiff_lexical};

    #[test]
    fn reads_a_key_and_maps_the_source_dir_across() {
        let text = r#"{
  "CMAKE_SOURCE_DIR" : "/build/src",
  "CMAKE_BINARY_DIR" : "/build/src/build",
  "HEADERS" : [ [ "/build/src/build/include/QtSvg/x.h", "Mu", null ] ]
}"#;
        assert_eq!(
            json_string_value(text, "CMAKE_BINARY_DIR"),
            Some("/build/src/build")
        );
        assert_eq!(
            json_string_value(text, "CMAKE_SOURCE_DIR"),
            Some("/build/src")
        );
        // NEGATIVE CONTROL: an absent key must return None, because the
        // caller leaves the file untouched on None and half-rewriting it
        // would be worse than not rewriting it at all.
        assert_eq!(json_string_value(text, "CMAKE_NOT_A_KEY"), None);

        // The source dir keeps its relationship to the binary dir.
        assert_eq!(pathdiff_lexical("/build/src", "/build/src/build"), "..");
        assert_eq!(lexical_normalize("/a/b/src/build/.."), "/a/b/src");
        // And a sibling layout, which is the other shape cmake produces.
        assert_eq!(pathdiff_lexical("/w/src", "/w/build"), "../src");
    }

    /// THE ONE-PASS PLACEHOLDER, which is the whole reason this function is
    /// not two `replace` calls.
    ///
    /// THE FIXTURE IS THE TEST. The collision only happens when the NEW
    /// build dir CONTAINS the OLD source dir as a substring - that is when
    /// the binary dir's replacement text carries the source dir's text, so a
    /// second sequential replace runs over the first's output. A temp dir
    /// like `/tmp/x` reproduces nothing, and the first version of this test
    /// used one: both mutants below survived it. The build dir here is
    /// deliberately built to end in the old source dir's own spelling.
    #[test]
    fn rewrites_both_roots_without_the_second_pass_eating_the_first() {
        use std::fs;
        let root = std::env::temp_dir().join(format!("nn-agi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // Contains "/build/src", which is the old SOURCE dir below.
        let build_dir = root.join("build/src/build");
        fs::create_dir_all(&build_dir).unwrap();
        let plan = build_dir.join("AutogenInfo.json");
        let text = r#"{
  "CMAKE_SOURCE_DIR" : "/build/src",
  "CMAKE_BINARY_DIR" : "/build/src/build",
  "HEADERS" : [ "/build/src/build/include/x.h", "/build/src/svg/y.cpp" ]
}"#;
        fs::write(&plan, text).unwrap();

        super::rewrite_autogen_info("cmake_autogen AutogenInfo.json", &build_dir).unwrap();
        let got = fs::read_to_string(&plan).unwrap();

        let b = build_dir.to_string_lossy().into_owned();
        let src = super::lexical_normalize(&format!("{b}/.."));
        // EXACT, not `contains`. The new build dir ends in the OLD source
        // dir's own spelling here, so any substring assertion is satisfied
        // by a mangled result too. Only the whole document pins it - and it
        // is the whole document the placeholder exists to protect: with two
        // sequential replaces the source-dir pass re-expands the prefix
        // INSIDE what the binary-dir pass just wrote.
        let want = format!(
            "{{\n  \"CMAKE_SOURCE_DIR\" : \"{src}\",\n  \"CMAKE_BINARY_DIR\" : \"{b}\",\n  \"HEADERS\" : [ \"{b}/include/x.h\", \"{src}/svg/y.cpp\" ]\n}}"
        );
        assert_eq!(got, want);

        // THE SIBLING LAYOUT, which pins the WIRING and not just the
        // helper. In the in-tree case above the source dir is exactly one
        // `..` from the binary dir, so hardcoding ".." passes it - measured,
        // that mutant survived. Here source and build are siblings and the
        // answer is "../src", so the call to pathdiff_lexical has to be real.
        let sib_build = root.join("w/build");
        fs::create_dir_all(&sib_build).unwrap();
        let sib_plan = sib_build.join("AutogenInfo.json");
        let sib_text = r#"{
  "CMAKE_SOURCE_DIR" : "/w/src",
  "CMAKE_BINARY_DIR" : "/w/build",
  "HEADERS" : [ "/w/src/a.cpp" ]
}"#;
        fs::write(&sib_plan, sib_text).unwrap();
        super::rewrite_autogen_info("cmake_autogen AutogenInfo.json", &sib_build).unwrap();
        let sib_got = fs::read_to_string(&sib_plan).unwrap();
        let sb = sib_build.to_string_lossy().into_owned();
        let ss = super::lexical_normalize(&format!("{sb}/../src"));
        assert_eq!(
            sib_got,
            format!(
                "{{\n  \"CMAKE_SOURCE_DIR\" : \"{ss}\",\n  \"CMAKE_BINARY_DIR\" : \"{sb}\",\n  \"HEADERS\" : [ \"{ss}/a.cpp\" ]\n}}"
            )
        );

        // THE AUTOGEN GATE, and it needs a file that WOULD be rewritten.
        // Asserting on an empty `{}` proves nothing: with the gate removed
        // the function still leaves it alone, because it has no roots to
        // map. This file has roots.
        fs::write(&plan, text).unwrap();
        super::rewrite_autogen_info("cc -c a.cpp AutogenInfo.json", &build_dir).unwrap();
        assert_eq!(
            fs::read_to_string(&plan).unwrap(),
            text,
            "a command that is not an autogen step must not touch the plan"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_a_relative_target_and_refuses_the_rest() {
        assert_eq!(
            cd_prologue_dir("cd ../src/svg && cmake -P x.cmake"),
            Some("../src/svg")
        );
        assert_eq!(cd_prologue_dir("cd src/core && tool a"), Some("src/core"));
        // NEGATIVE CONTROLS, and the absolute one is the important half: a
        // path outside the task's tree is not ours to create, and doing so
        // would be the sandbox escape the path rewriting exists to prevent.
        assert_eq!(cd_prologue_dir("/nix/store/x/bin/cc -c a.cpp"), None);
        assert_eq!(cd_prologue_dir("cd /work/qt/src && tool a"), None);
        // A `cd` with no `&&` is not a prologue - it is the whole command,
        // and it changes nothing for the process that follows it.
        assert_eq!(cd_prologue_dir("cd src/svg"), None);
    }
}
