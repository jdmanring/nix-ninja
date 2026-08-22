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

    // Encoded derived files to prepare the source directory. Optional
    // because at scale the value arrives via nix's passAsFile as a
    // NIX_NINJA_INPUTSPath file instead: a multi-megabyte env var dies
    // at exec with "Argument list too long" (measured on Chromium's
    // graph, where every task inherits the configure-generated set).
    #[arg(long, env = "NIX_NINJA_INPUTS")]
    pub inputs: Option<String>,

    // Encoded derived files that build outputs should be copied to.
    // Same passAsFile fallback as inputs.
    #[arg(long, env = "NIX_NINJA_OUTPUTS")]
    pub outputs: Option<String>,

    // Command to run.
    pub cmdline: String,
}

/// Resolve an attr that may arrive inline or via passAsFile. Absence of
/// BOTH is an error, not an empty list: a task with no inputs at all is
/// a malformed derivation, and treating it as empty would let a plumbing
/// regression read as a clean build.
fn inline_or_pass_as_file(inline: Option<String>, name: &str) -> Result<String> {
    if let Some(v) = inline {
        return Ok(v);
    }
    let path_var = format!("{name}Path");
    match env::var(&path_var) {
        Ok(p) => Ok(fs::read_to_string(&p)
            .map_err(|e| anyhow::anyhow!("reading {path_var}={p}: {e}"))?),
        Err(_) => Err(anyhow::anyhow!(
            "neither {name} nor {path_var} is set; the driving nix-ninja and this nix-ninja-task disagree about attr passing"
        )),
    }
}

fn leading_ups(p: &std::path::Path) -> usize {
    p.components()
        .take_while(|c| matches!(c, std::path::Component::ParentDir))
        .count()
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let raw_inputs = inline_or_pass_as_file(cli.inputs.clone(), "NIX_NINJA_INPUTS")?;
    let raw_outputs = inline_or_pass_as_file(cli.outputs.clone(), "NIX_NINJA_OUTPUTS")?;

    let mut inputs = Vec::new();
    for encoded in raw_inputs.split_whitespace() {
        // println!("Processing input {}", encoded);
        let input = DerivedFile::from_encoded(&cli.store_dir, encoded)?;
        inputs.push(input);
    }

    let mut outputs = Vec::new();
    for encoded in raw_outputs.split_whitespace() {
        // println!("Processing output {}", encoded);
        let output = DerivedFile::from_encoded(&cli.store_dir, encoded)?;
        outputs.push(output);
    }

    // SELF-DEFENDING DEPTH: the driver computes a mirrored build dir from
    // the inputs it can see, but discovered and dynamically-added inputs
    // climb too, and every driver path that under-computed the depth sent
    // a `..`-heavy input escaping the writable tree (mkdir /src, EACCES).
    // The task knows its FINAL input list, so it deepens its own build
    // dir with synthetic components until every climb stays under /build.
    let build_dir = {
        let max_up = inputs
            .iter()
            .map(|i| leading_ups(&i.build_path))
            .max()
            .unwrap_or(0);
        let below_build = cli
            .build_dir
            .strip_prefix("/build")
            .map(|r| r.components().count())
            .unwrap_or(usize::MAX);
        let mut b = cli.build_dir.clone();
        if below_build != usize::MAX && max_up > below_build {
            for i in 0..(max_up - below_build) {
                b.push(format!("nnd{i}"));
            }
            println!(
                "nix-ninja-task: deepened build dir to {} (inputs climb {max_up}, caller gave {below_build})",
                b.display()
            );
        }
        b
    };

    fs::create_dir_all(&build_dir)
        .map_err(|e| anyhow::anyhow!("create_dir_all({}): {e}", build_dir.display()))?;
    std::env::set_current_dir(&build_dir)
        .map_err(|e| anyhow::anyhow!("set_current_dir({}): {e}", build_dir.display()))?;

    // Python resolves a script SYMLINK when computing sys.path[0]
    // (getpath realpaths it), so a wrapper script materialized as a
    // symlink to a single-file store object cannot import its sibling
    // modules - python looks inside the store object's directory, which
    // holds one file. Prepend every .py input's build-dir-relative
    // directory to PYTHONPATH; the symlink directories hold ALL the
    // sibling symlinks, so imports resolve there.
    let py_parents: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        for input in &inputs {
            if input.build_path.extension().is_some_and(|e| e == "py") {
                if let Some(parent) = input.build_path.parent() {
                    if !v.contains(&parent.to_path_buf()) {
                        v.push(parent.to_path_buf());
                    }
                }
            }
        }
        v
    };

    // The source directory of the derivation needs to have all build inputs
    // symlinked while preserving the original directory hierarchy of the
    // sources. This ensures relative includes and other path-dependent
    // references remain valid.
    create_symlinks(&build_dir, &cli.store_dir, inputs, false)?;
    println!(
        "nix-ninja-task: Setup source directory in {}",
        build_dir.display()
    );

    // PYTHONPATH is assembled AFTER the tree is materialized, because the
    // package-root climb reads __init__.py off the filesystem - run
    // before create_symlinks it sees an empty tree and every check
    // answers false (measured: mojom/parse/ stayed on the path and its
    // ast.py shadowed stdlib ast under python 3.14). Never put a
    // package's INTERNALS on sys.path: a dir with __init__.py climbs to
    // its package root first, which is also the semantically correct
    // entry - `import jinja2` wants third_party/, never
    // third_party/jinja2/.
    {
        let mut py_dirs: Vec<String> = Vec::new();
        for parent in &py_parents {
            // A PACKAGE INTERNAL contributes nothing here. Adding it
            // directly shadowed stdlib ast (mojom/parse/ carries an
            // ast.py); climbing to its package root instead exposed the
            // root's child as an importable name and shadowed a
            // NAMESPACE package elsewhere (flatbuffers' src/python, a
            // regular package, beat perfetto's python/ namespace
            // package, measured). Scripts are COPIED into the tree, so
            // sys.path[0] and each script's own sys.path inserts cover
            // package imports; only plain script directories belong on
            // the path.
            if parent.join("__init__.py").is_file() {
                continue;
            }
            let d = parent.to_string_lossy().into_owned();
            if !d.is_empty() && !py_dirs.contains(&d) {
                py_dirs.push(d);
            }
        }
        if !py_dirs.is_empty() {
            let mut pp = py_dirs.join(":");
            if let Ok(existing) = env::var("PYTHONPATH") {
                pp.push(':');
                pp.push_str(&existing);
            }
            env::set_var("PYTHONPATH", &pp);
            println!("nix-ninja-task: PYTHONPATH={pp}");
        }
    }

    // Outputs are written to the same directory structure as the build
    // directory because if the output is a shared library the filename must
    // match the soname and it must be in a directory to add to the linking
    // binary's RUNPATH.
    create_output_dirs(&outputs)?;

    // Ninja rspfile: the runner contract says this file exists before
    // the command runs. Content may arrive inline or via passAsFile
    // (rsp files exist because their content outgrows command lines).
    if let Ok(rsp_path) = env::var("NIX_NINJA_RSPFILE_PATH") {
        let content = inline_or_pass_as_file(
            env::var("NIX_NINJA_RSPFILE_CONTENT").ok(),
            "NIX_NINJA_RSPFILE_CONTENT",
        )?;
        let rsp = PathBuf::from(&rsp_path);
        if let Some(parent) = rsp.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!("create_dir_all({}) for rspfile: {e}", parent.display())
                })?;
            }
        }
        fs::write(&rsp, &content)
            .map_err(|e| anyhow::anyhow!("writing rspfile {rsp_path}: {e}"))?;
        println!("nix-ninja-task: wrote rspfile {rsp_path} ({} bytes)", content.len());
    }

    if let Some(desc) = cli.description {
        println!("nix-ninja-task: {desc}");
    }

    // CMake's AUTOGEN plan carries CONFIGURE-TIME ABSOLUTE PATHS, and no
    // rewrite reaches them. See rewrite_autogen_info.
    rewrite_autogen_info(&cli.cmdline, &build_dir)?;

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

fn copy_outputs_to_placeholders(store_dir: &StoreDir, outputs: &[DerivedFile]) -> Result<()> {
    for output in outputs {
        let target_path = output.absolute_path(store_dir);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!("create_dir_all({}) for output: {e}", parent.display())
            })?;
        }
        fs::copy(&output.build_path, &target_path).map_err(|e| {
            anyhow::anyhow!(
                "copy({} -> {}): {e}",
                output.build_path.display(),
                target_path.display()
            )
        })?;
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
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "create_dir_all({}) for output {}: {e}",
                    parent.display(),
                    output.build_path.display()
                )
            })?;
            dirs.push(parent);
        }
    }
    Ok(())
}

/// The directory a `cd <dir> && ...` prologue names, when it is relative.
/// Returns None for a command with no prologue and for an absolute target,
/// which is not ours to create.
fn cd_prologue_dir(cmdline: &str) -> Option<&str> {
    let after = cmdline.strip_prefix("cd ")?;
    let (dir, _) = after.split_once(" && ")?;
    if dir.is_empty() || dir.starts_with('/') {
        return None;
    }
    Some(dir)
}

/// A `cd` TARGET IS NOT AN INPUT, SO NOTHING MATERIALIZES IT. The task
/// creates directories for its declared outputs and materializes the files
/// it declares as inputs; a directory that merely has to EXIST for the shell
/// to enter it is in neither set. CMake's out-of-source layout produces
/// exactly that: a custom command cds into a source subdirectory while every
/// file it touches lives under the build tree, so the target holds no input
/// and is never created.
///
/// The failure is `/bin/sh: cd: can't cd to ../src/svg: No such file or
/// directory`, which reads as a missing SOURCE tree - a packaging problem -
/// rather than as a directory nobody was asked to make.
///
/// Creating it is safe because the command's own paths are already rewritten
/// to resolve from it: the cwd's CONTENT is not what the command reads, only
/// its position. An absolute target is left alone - that is a path outside
/// the task's tree and creating it would be the sandbox escape the path
/// rewriting exists to prevent.
fn ensure_cd_target(cmdline: &str) -> Result<()> {
    if let Some(dir) = cd_prologue_dir(cmdline) {
        if !Path::new(dir).is_dir() {
            fs::create_dir_all(dir).map_err(|e| {
                anyhow::anyhow!("create_dir_all({dir}) for a cd prologue: {e}")
            })?;
        }
    }
    Ok(())
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
        let mut out = text;
        // Deepest first: whichever of the two is longer contains the other.
        let mut pairs = vec![(jb, actual_b), (js, actual_s)];
        pairs.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        for (from, to) in &pairs {
            out = out.replace(from.as_str(), to.as_str());
        }
        // The materialised input is a read-only store symlink; replace it.
        let _ = fs::remove_file(&path);
        fs::write(&path, &out)
            .map_err(|e| anyhow::anyhow!("rewriting {}: {e}", path.display()))?;
        println!(
            "nix-ninja-task: rewrote autogen plan {} ({} bytes)",
            path.display(),
            out.len()
        );
    }
    Ok(())
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

fn spawn_process(cmdline: &str) -> Result<i32> {
    ensure_cd_target(cmdline)?;
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", cmdline])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .envs(env::vars());

    let output = cmd.status()?;
    Ok(output.code().unwrap_or(1))
}

#[cfg(test)]
mod cd_prologue_tests {
    use super::cd_prologue_dir;

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

#[cfg(test)]
mod autogen_rewrite_tests {
    use super::{json_string_value, lexical_normalize, pathdiff_lexical};

    #[test]
    fn reads_a_key_and_maps_the_source_dir_across() {
        let text = r#"{
  "CMAKE_SOURCE_DIR" : "/build/src",
  "CMAKE_BINARY_DIR" : "/build/src/build",
  "HEADERS" : [ [ "/build/src/build/include/QtSvg/x.h", "Mu", null ] ]
}"#;
        assert_eq!(json_string_value(text, "CMAKE_BINARY_DIR"), Some("/build/src/build"));
        assert_eq!(json_string_value(text, "CMAKE_SOURCE_DIR"), Some("/build/src"));
        // NEGATIVE CONTROL: an absent key must return None, because the
        // caller leaves the file untouched on None and half-rewriting it
        // would be worse than not rewriting it at all.
        assert_eq!(json_string_value(text, "CMAKE_NOT_A_KEY"), None);

        // The source dir keeps its relationship to the binary dir.
        assert_eq!(pathdiff_lexical("/build/src", "/build/src/build"), "..");
        assert_eq!(
            lexical_normalize("/a/b/src/build/.."),
            "/a/b/src"
        );
        // And a sibling layout, which is the other shape cmake produces.
        assert_eq!(pathdiff_lexical("/w/src", "/w/build"), "../src");
    }
}
