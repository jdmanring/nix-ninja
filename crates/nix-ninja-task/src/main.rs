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
        // AN RSPFILE IS A CARRIER OF CONFIGURE-TIME ABSOLUTE PATHS, and
        // rewriting the command line does not reach inside it. Qt's syncqt
        // args file is the case that proves it: it names
        //     -sourceDir  /build/src/src/svg
        //     -headers    /build/src/src/svg/qsvghelper_p.h  ... x26
        // as the paths CMake saw when it CONFIGURED, in a different
        // derivation with a different root. Inside a task those resolve
        // nowhere, so syncqt runs, exits zero, and generates forwarding
        // headers for none of them - an include tree missing every private
        // header. Every translation unit then fails on QtSvg/private/*, with
        // nothing in the syncqt task's own log to say why.
        //
        // The mapping comes from the file itself, the way the autogen plan's
        // does. `-binaryDir` names the rspfile's OWN directory in
        // configure-time absolute form, and the rspfile's path tells us that
        // directory's actual location now. Pairing them, and then their
        // parents up the chain, converts every prefix the file can carry.
        let before = content.len();
        let content = rewrite_rsp_roots(&content, &rsp);
        // SYNCQT SILENTLY IGNORES RELATIVE PATHS, and that is the whole of
        // why no Qt module could build through this tool.
        //
        // Reproduced outside nix, same binary, same source tree, same
        // argument list, differing only in path form:
        //     absolute -> 36 files, include/<M>/<ver>/<M>/private populated
        //     relative ->  6 files, no private tree at all
        // Both exit 0 and neither prints a word. The driver's rewrite makes
        // every path in an rspfile relative - correct and necessary for a
        // compiler, which resolves them from its working directory - and
        // syncqt discards them instead of resolving them. A translation unit
        // three tasks later then fails on QtSvg/private/*, which is the
        // first visible symptom of an argument list thrown away here.
        //
        // So put them back, for this tool only. The base is where the command
        // will actually run: the cd prologue's target if it has one, the
        // build dir otherwise. Absolutizing every rspfile would be wrong -
        // a compiler's rsp is correct as it stands and store paths must not
        // move - which is why this is gated on the tool that needs it.
        let content = if cli.cmdline.contains("syncqt") {
            absolutize_rsp(&content, &cli.cmdline)
        } else {
            content
        };
        // FIRED OR NOT, once per rspfile. A rewrite that returns its input
        // unchanged and a rewrite that never ran produce the same file, and
        // the failure they cause is identical and appears three tasks later.
        println!(
            "nix-ninja-task: rsp root rewrite: {} -> {} bytes, binaryDir={:?}",
            before,
            content.len(),
            content
                .split_whitespace()
                .skip_while(|t| *t != "-binaryDir")
                .nth(1)
        );
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

    // WHAT THE COMMAND CAN SEE OF A SOURCE TREE ABOVE THE BUILD DIR. An
    // input materialised at `../src/...` and a command told to look at
    // `../../../src/...` from a subdirectory resolve to the same place only
    // if the depth compensation is right, and when it is not the tool finds
    // an EMPTY directory rather than a missing one: syncqt then generates
    // forwarding headers for nothing, exits zero, and says nothing.
    if cli.cmdline.contains("syncqt") {
        for probe in ["../src", "src"] {
            let n = fs::read_dir(probe)
                .map(|rd| rd.flatten().count())
                .unwrap_or(usize::MAX);
            println!(
                "nix-ninja-task: probe {probe}: {}",
                if n == usize::MAX { "absent".to_string() } else { format!("{n} entries") }
            );
        }
        for probe in ["../src/svg", "src/svg"] {
            let n = fs::read_dir(probe)
                .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "h")).count())
                .unwrap_or(usize::MAX);
            println!(
                "nix-ninja-task: probe {probe}: {}",
                if n == usize::MAX { "absent".to_string() } else { format!("{n} header(s)") }
            );
        }
    }

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
    // WHAT A DIRECTORY OUTPUT ACTUALLY HOLDS, before it is copied out. A
    // tree output is the only output whose CONTENTS are not knowable from the
    // graph, so it is the only one where "the task succeeded" and "the task
    // produced what its consumers need" come apart silently. syncqt exits
    // zero having written five forwarding headers or five hundred, and the
    // difference does not surface until a translation unit three tasks later
    // fails on a header nobody can trace back here.
    for output in &outputs {
        if output.build_path.is_dir() {
            let mut n = 0usize;
            let mut sample: Vec<String> = Vec::new();
            let mut stack = vec![output.build_path.clone()];
            while let Some(d) = stack.pop() {
                if let Ok(rd) = fs::read_dir(&d) {
                    for e in rd.flatten() {
                        let q = e.path();
                        match fs::metadata(&q) {
                            Ok(m) if m.is_dir() => stack.push(q),
                            Ok(_) => {
                                n += 1;
                                if sample.len() < 6 {
                                    sample.push(q.display().to_string());
                                }
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
            println!(
                "nix-ninja-task: tree output {} holds {} file(s): {}",
                output.build_path.display(),
                n,
                sample.join(" ")
            );
        }
    }
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
        // A DIRECTORY OUTPUT IS THE ONLY WAY TO CARRY WHAT A RULE DOES NOT
        // DECLARE. syncqt writes hundreds of forwarding headers whose names
        // ninja never knows, so the driver declares the TREE and this copies
        // whatever is in it. fs::copy is file-to-file and would fail with
        // EISDIR here, which reads as a permissions problem.
        if output.build_path.is_dir() {
            copy_tree(&output.build_path, &target_path)?;
            continue;
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

/// Copy a directory recursively, following nothing. Symlinks inside a build
/// tree point at materialized inputs, so copying the LINK would carry a path
/// that does not exist on the far side; copying what it resolves to is what
/// the consumer needs.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)
        .map_err(|e| anyhow::anyhow!("create_dir_all({}): {e}", dst.display()))?;
    for entry in fs::read_dir(src)
        .map_err(|e| anyhow::anyhow!("read_dir({}): {e}", src.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        // metadata() follows the link; file_type() would not.
        // A TOOL'S OWN INCREMENTAL-STATE DIRECTORY MUST NOT BE CAPTURED.
        // syncqt writes `.syncqt_staging` beside the headers it generates
        // and reads it to decide the sync is already done. Copying it into
        // the tree output makes it an INPUT on the next run, syncqt then
        // skips, and the task succeeds while generating nothing - the output
        // is byte-identical every time, which reads as determinism rather
        // than as a short circuit. Measured on qtsvg: eight files captured,
        // three of them staging, and no private forwarding headers ever.
        // Dot-directories generally, because this is what tools use for the
        // purpose and none of them belongs in a declared output.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let md = match fs::metadata(&from) {
            Ok(m) => m,
            // A DANGLING LINK IS NOT AN ERROR HERE. A build tree carries
            // links to inputs that were materialized for a different task;
            // failing on one would turn a complete output into no output.
            Err(_) => continue,
        };
        if md.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .map_err(|e| anyhow::anyhow!("copy({} -> {}): {e}",
                    from.display(), to.display()))?;
            // fs::copy carries the source mode, and a generated header is
            // often 0444. A later task that has to overwrite it dies EACCES,
            // which is the same defect the duplicate-output fix already met.
            let mut perms = fs::metadata(&to)
                .map_err(|e| anyhow::anyhow!("metadata({}): {e}", to.display()))?
                .permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = fs::set_permissions(&to, perms);
        }
    }
    Ok(())
}

/// Rewrite configure-time absolute roots in an rspfile to where the tree
/// actually sits now.
///
/// Keyed on `-binaryDir`, which names the rspfile's own directory as the
/// configure step saw it; the rspfile's current path gives the same directory
/// now. Pairing the two and walking both up together converts the build root,
/// the source root above it, and anything else the file spells with those
/// prefixes.
///
/// Returns the content unchanged when the key is absent or is not absolute -
/// an rspfile that carries no configure-time root needs no rewrite, and
/// guessing one would corrupt a file that was already correct.
fn rewrite_rsp_roots(content: &str, rsp: &Path) -> String {
    let mut toks = content.split_whitespace();
    let mut binary_dir: Option<&str> = None;
    while let Some(t) = toks.next() {
        if t == "-binaryDir" {
            binary_dir = toks.next();
            break;
        }
    }
    let (Some(bd), Some(actual)) = (binary_dir, rsp.parent()) else {
        return content.to_string();
    };
    if !bd.starts_with('/') {
        return content.to_string();
    }
    let actual = match actual.canonicalize() {
        Ok(a) => a,
        Err(_) => actual.to_path_buf(),
    };
    // Longest first, and through placeholders, because each pair's
    // replacement CONTAINS the shorter pairs' text - the same nesting that
    // made two sequential replaces corrupt each other in the autogen plan.
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut from = Some(Path::new(bd));
    let mut to = Some(actual.as_path());
    while let (Some(f), Some(t)) = (from, to) {
        if f.components().count() < 3 || t.components().count() < 3 {
            break;
        }
        pairs.push((
            f.to_string_lossy().into_owned(),
            t.to_string_lossy().into_owned(),
        ));
        from = f.parent();
        to = t.parent();
    }
    let mut out = content.to_string();
    for (i, (f, _)) in pairs.iter().enumerate() {
        out = out.replace(f.as_str(), &format!("\0NNR{i}\0"));
    }
    for (i, (_, t)) in pairs.iter().enumerate() {
        out = out.replace(&format!("\0NNR{i}\0"), t.as_str());
    }
    out
}

/// Make an rspfile's relative paths absolute against the directory the
/// command will run in. Only paths: a flag, a bare word with no separator, an
/// already-absolute path and anything that does not resolve are all left
/// alone, so a token that is not a path cannot be turned into one.
fn absolutize_rsp(content: &str, cmdline: &str) -> String {
    let base = match cd_prologue_dir(cmdline) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from("."),
    };
    let base = match base.canonicalize() {
        Ok(b) => b,
        Err(_) => return content.to_string(),
    };
    let mut out = String::with_capacity(content.len() * 2);
    let mut n = 0usize;
    // A `-...Dir` flag's VALUE is a directory syncqt may be about to CREATE,
    // so existence cannot decide for it. `-privateIncludeDir` names the tree
    // that does not exist until syncqt makes it, and leaving that one
    // relative is enough on its own to lose every private forwarding header
    // while the header list is absolute and correct - measured, 30 paths
    // absolutized and still no private tree. Positional, because the value
    // is the next token whatever it looks like.
    let mut prev_is_dir_flag = false;
    for line in content.split_inclusive('\n') {
        let tok = line.trim_end_matches(['\n', '\r']);
        let tail = &line[tok.len()..];
        let is_dir_value = prev_is_dir_flag;
        prev_is_dir_flag = tok.starts_with('-') && tok.ends_with("Dir");
        if tok.starts_with('-') || tok.starts_with('/') || !tok.contains('/') {
            out.push_str(line);
            continue;
        }
        if is_dir_value {
            let joined = lexical_normalize(&format!("{}/{}", base.display(), tok));
            out.push_str(&joined);
            out.push_str(tail);
            n += 1;
            continue;
        }
        // EXISTENCE DECIDES, as it does everywhere else in this tool. A
        // relative token that resolves to nothing is not a path we can
        // repair, and inventing an absolute spelling for it would replace a
        // silent miss with a confident wrong answer.
        let joined = lexical_normalize(&format!("{}/{}", base.display(), tok));
        if Path::new(&joined).exists() {
            out.push_str(&joined);
            out.push_str(tail);
            n += 1;
        } else {
            out.push_str(line);
        }
    }
    // WHAT THE REWRITTEN LIST ACTUALLY POINTS AT. A count of rewritten
    // tokens says the rewrite ran; it does not say the result resolves, and
    // syncqt answers both cases the same way - exit 0, no output, no headers.
    // Report the two values everything else depends on, and whether the
    // source directory it names holds anything.
    let val_after = |flag: &str| -> Option<String> {
        let mut it = out.split_whitespace();
        while let Some(t) = it.next() {
            if t == flag {
                return it.next().map(|s| s.to_string());
            }
        }
        None
    };
    for flag in ["-sourceDir", "-privateIncludeDir"] {
        match val_after(flag) {
            Some(v) => {
                let hdrs = fs::read_dir(&v)
                    .map(|rd| {
                        rd.flatten()
                            .filter(|e| e.path().extension().is_some_and(|x| x == "h"))
                            .count() as i64
                    })
                    .unwrap_or(-1);
                println!("nix-ninja-task: {flag} = {v} ({hdrs} header(s), -1 means absent)");
            }
            None => println!("nix-ninja-task: {flag} ABSENT from the rewritten rspfile"),
        }
    }
    let heads = out
        .split_whitespace()
        .skip_while(|t| *t != "-headers")
        .filter(|t| t.ends_with(".h"))
        .count();
    let heads_ok = out
        .split_whitespace()
        .skip_while(|t| *t != "-headers")
        .filter(|t| t.ends_with(".h") && Path::new(t).exists())
        .count();
    println!(
        "nix-ninja-task: absolutized {n} rspfile path(s) for syncqt; \
         {heads_ok} of {heads} header path(s) resolve"
    );
    out
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

    #[test]
    fn nested_prefixes_do_not_rewrite_each_other() {
        // THE SOURCE DIR IS A PREFIX OF THE BINARY DIR in every out-of-source
        // CMake layout, so two sequential replaces corrupt each other: the
        // binary dir's replacement CONTAINS the source dir's text. This is
        // the exact pair measured on qtsvg.
        let jb = "/build/src/build";
        let js = "/build/src";
        let actual_b = "/build/source/build/src/build";
        let actual_s = "/build/source/build/src";
        let text = "\"/build/src/build/include/QtSvg/x.h\" \"/build/src/svg/y.cpp\"";

        const HOLD: &str = "\u{0}NN_BINDIR\u{0}";
        let out = text
            .replace(jb, HOLD)
            .replace(js, actual_s)
            .replace(HOLD, actual_b);
        assert_eq!(
            out,
            "\"/build/source/build/src/build/include/QtSvg/x.h\" \"/build/source/build/src/svg/y.cpp\""
        );

        // NEGATIVE CONTROL: the naive two-replace order produces the mangled
        // path this guards against. If this ever stops differing, the
        // placeholder is unnecessary and the bug it fixes was impossible.
        let naive = text.replace(jb, actual_b).replace(js, actual_s);
        assert_ne!(naive, out);
        assert!(naive.contains("/build/source/build/source/"));
    }
}

#[cfg(test)]
mod rsp_root_tests {
    use super::rewrite_rsp_roots;
    use std::path::Path;

    #[test]
    fn maps_configure_time_roots_onto_the_current_tree() {
        let content = "-module QtSvg -sourceDir /build/src/src/svg \
-binaryDir /build/src/build/src/svg -headers /build/src/src/svg/a_p.h \
/build/src/build/include/QtSvg/x.h";
        let rsp = Path::new("/work/t/src/build/src/svg/Svg_syncqt_args");
        let out = rewrite_rsp_roots(content, rsp);
        // The build root and the source root above it both move, and neither
        // replacement is re-rewritten by the other.
        assert!(out.contains("-sourceDir /work/t/src/src/svg"), "{out}");
        assert!(out.contains("/work/t/src/src/svg/a_p.h"), "{out}");
        assert!(out.contains("/work/t/src/build/include/QtSvg/x.h"), "{out}");
        // A PRECISE NEGATIVE, not a substring hunt. "/build/src" appears
        // inside the CORRECT answer - /work/t/src/build/src/svg - so
        // asserting its absence fails on a passing rewrite. What must be
        // gone is the configure-time SOURCE root as a whole path.
        assert!(!out.contains("/build/src/src/svg"), "{out}");
        assert!(!out.contains("/build/src/build"), "{out}");

        // NEGATIVE CONTROLS: no key, and a relative key. An rspfile carrying
        // no configure-time root needs no rewrite, and inventing one would
        // corrupt a file that was already correct.
        assert_eq!(rewrite_rsp_roots("-flag value", rsp), "-flag value");
        assert_eq!(
            rewrite_rsp_roots("-binaryDir src/svg -x y", rsp),
            "-binaryDir src/svg -x y"
        );
    }
}
