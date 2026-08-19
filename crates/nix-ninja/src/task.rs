use crate::local;
use crate::relative_from::relative_from;
use crate::subtool::dynamic_task;
use anyhow::{anyhow, Context, Error, Result};
use deps_infer::c_include_parser;
use harmonia_store_content_address::ContentAddressMethodAlgorithm;
use harmonia_store_derivation::derivation::{Derivation, DerivationOutput};
use harmonia_store_derivation::derived_path::{OutputName, SingleDerivedPath};
use harmonia_store_derivation::placeholder::Placeholder;
use harmonia_store_path::{StoreDir, StorePath};
use n2::{
    canon,
    graph::{self, Build, BuildDependencies, BuildId, File, FileId},
};
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::DerivedFile;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Condvar, Mutex},
};
use walkdir::WalkDir;
use which::which;

#[derive(Clone)]
pub struct Tools {
    pub cc: StorePath,
    pub coreutils: StorePath,
    pub nix: StorePath,
    pub nix_ninja: StorePath,
    pub nix_ninja_task: StorePath,
    pub patchelf: StorePath,
}

impl Tools {
    pub fn new(store_dir: &StoreDir) -> Result<Self> {
        Ok(Tools {
            cc: which_store_path(store_dir, "cc")?,
            coreutils: which_store_path(store_dir, "coreutils")?,
            nix: which_store_path(store_dir, "nix")?,
            nix_ninja: which_store_path(store_dir, "nix-ninja")?,
            nix_ninja_task: which_store_path(store_dir, "nix-ninja-task")?,
            patchelf: which_store_path(store_dir, "patchelf")?,
        })
    }
}

/// Task represents a fully evaluated Ninja build target.
///
/// A task contains all the context to generate a Nix derivation for the build
/// target.
#[derive(Clone)]
struct Task {
    system: String,
    wrapper_vars: HashMap<String, String>,
    input_srcs: Vec<StorePath>,

    build_dir: PathBuf,
    build_deps: BuildDependencies,
    store_dir: StoreDir,

    cmdline: Option<String>,
    desc: Option<String>,
    deps: Option<String>,
    // Ninja rspfile: (path, content) the runner must write before the
    // command runs. GN emits one per action rule; a task without it gets
    // an empty argument where the command expects the file.
    rspfile: Option<(PathBuf, String)>,

    files: HashMap<FileId, File>,
    inputs: Vec<DerivedFile>,
    outputs: Vec<PathBuf>,
}

impl Deref for Task {
    type Target = BuildDependencies;

    fn deref(&self) -> &Self::Target {
        &self.build_deps
    }
}

/// BuildResult is the output of a Task.
pub struct BuildResult {
    pub bid: BuildId,
    pub derived_path: Option<SingleDerivedPath>,
    pub derived_files: Vec<DerivedFile>,
    pub err: Option<Error>,
}

#[derive(Clone)]
pub struct RunnerConfig {
    pub system: String,
    pub build_dir: PathBuf,
    pub store_dir: StoreDir,
    pub is_output_derivation: bool,
    /// Maximum tasks in flight. The `-j` flag parsed this and nothing
    /// consumed it; the runner spawned one thread per ready task,
    /// unbounded. On a graph with codegen fan-out (Chromium: one TU's
    /// order-only deps expand to hundreds of generated-header tasks)
    /// that produced a load average of 553 on a 24-thread machine.
    pub jobs: usize,
}

/// Counting semaphore bounding concurrent tasks. Permits release on
/// Drop, so a panicking task thread cannot leak a slot.
struct JobPermits {
    inner: Arc<(Mutex<usize>, Condvar)>,
    cap: usize,
}

struct JobPermit {
    inner: Arc<(Mutex<usize>, Condvar)>,
}

impl JobPermits {
    fn new(cap: usize) -> Self {
        JobPermits {
            inner: Arc::new((Mutex::new(0), Condvar::new())),
            cap,
        }
    }

    /// Blocks until a slot frees. Blocking in the scheduler's start()
    /// is deliberate backpressure: running task threads complete and
    /// release without needing the main loop, so this cannot deadlock.
    fn acquire(&self) -> JobPermit {
        let (lock, cvar) = &*self.inner;
        let mut count = lock.lock().unwrap();
        while *count >= self.cap {
            count = cvar.wait(count).unwrap();
        }
        *count += 1;
        JobPermit {
            inner: self.inner.clone(),
        }
    }
}

impl Drop for JobPermit {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.inner;
        let mut count = lock.lock().unwrap();
        *count -= 1;
        cvar.notify_one();
    }
}

/// Runner is an async runtime that spawns threads for each task.
pub struct Runner {
    pub derived_files: HashMap<FileId, DerivedFile>,
    build_dir_inputs: HashMap<FileId, DerivedFile>,
    // A phony build produces no artifact: it is an alias for its inputs.
    // CMake emits one per target (`cmake_object_order_depends_target_X:
    // phony || .`) as an order-only input of every TU, so without this
    // map every CMake-generated graph is unbuildable. Maps each phony
    // output FileId to the phony's input FileIds; dependents expand these
    // (transitively) in new_task instead of demanding a derivation.
    phony_aliases: HashMap<FileId, Vec<FileId>>,
    // Stamp-edge outputs -> that edge's own inputs. See the stamp comment
    // in new_task's expansion loop. Detection is by the one stamp script
    // measured in the wild (perfetto's touch_file.py); widen when a second
    // convention shows up.
    stamp_inputs: HashMap<FileId, Vec<FileId>>,

    tx: mpsc::Sender<BuildResult>,
    rx: mpsc::Receiver<BuildResult>,
    tools: Tools,
    rpc_client: Arc<BuilderRpcClient>,
    config: RunnerConfig,
    wrapper_vars: HashMap<String, String>,
    wrapper_store_paths: Vec<StorePath>,
    store_regex: Regex,
    permits: JobPermits,
}

impl Runner {
    pub fn new(
        tools: Tools,
        rpc_client: Arc<BuilderRpcClient>,
        config: RunnerConfig,
    ) -> Result<Self> {
        let store_dir_str = config.store_dir.to_string();
        let pattern = format!(
            r"{}\/[a-z0-9]{{32}}-[0-9a-zA-Z\+\-\._\?=]+",
            regex::escape(&store_dir_str)
        );
        let store_regex = Regex::new(&pattern)?;

        let mut wrapper_vars = HashMap::new();
        for (key, value) in env::vars() {
            if key.starts_with("NIX_CFLAGS_COMPILE")
                || key.starts_with("NIX_LDFLAGS")
                || key.starts_with("NIX_CC_WRAPPER")
                || key.starts_with("NIX_BINTOOLS_WRAPPER")
            {
                wrapper_vars.insert(key, value);
            }
        }

        // Remove -frandom-seed from NIX_CFLAGS_COMPILE* as we'll calculate it
        // per task derivation. Otherwise this will be different every time
        // breaking incrementality.
        for (key, value) in wrapper_vars.iter_mut() {
            if key.starts_with("NIX_CFLAGS_COMPILE") {
                *value = remove_frandom_seed(value);
            }
        }

        // Extract store paths from wrapper variables once
        let mut wrapper_store_paths = Vec::new();
        for value in wrapper_vars.values() {
            let found_store_paths = extract_store_paths(&config.store_dir, &store_regex, value)?;
            wrapper_store_paths.extend(found_store_paths);
        }

        let (tx, rx) = mpsc::channel();
        let permits = JobPermits::new(config.jobs.max(1));
        Ok(Runner {
            derived_files: HashMap::new(),
            build_dir_inputs: HashMap::new(),
            phony_aliases: HashMap::new(),
            stamp_inputs: HashMap::new(),
            permits,
            tx,
            rx,
            tools,
            rpc_client,
            config,
            wrapper_vars,
            wrapper_store_paths,
            store_regex,
        })
    }

    // Build systems like Meson may generate files via `configure_file` that are
    // not listed as implicit inputs in the build.ninja file. So we must read
    // the build directory and consider them implict inputs for all tasks.
    pub fn read_build_dir(&mut self, files: &mut graph::GraphFiles) -> Result<()> {
        for entry in WalkDir::new(&self.config.build_dir)
            .into_iter()
            .filter_entry(|e| {
                // Skip directories that start with "meson-" as they contain
                // non-deterministic internal data from meson
                !e.file_name().to_string_lossy().starts_with("meson-")
            })
        {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.into_path();
            let derived_file =
                new_opaque_file(&self.rpc_client, &self.config.build_dir, path.clone())?;
            let fid = self.add_derived_file(files, derived_file.clone());
            self.build_dir_inputs.insert(fid, derived_file);
        }
        Ok(())
    }

    pub fn start(
        &mut self,
        files: &mut graph::GraphFiles,
        bid: BuildId,
        build: &Build,
    ) -> Result<()> {
        let tx = self.tx.clone();

        // A phony rule has no command: record the alias and complete the
        // build immediately, spawning nothing. Ordering falls out of the
        // nix model for free - dependents inherit the phony's inputs as
        // their own derivation inputs via the expansion in new_task.
        if build.cmdline.is_none() {
            let ins: Vec<FileId> = build.ordering_ins().to_vec();
            for fid in build.outs() {
                self.phony_aliases.insert(*fid, ins.clone());
            }
            tx.send(BuildResult {
                bid,
                derived_path: None,
                derived_files: Vec::new(),
                err: None,
            })
            .context("completing phony build")?;
            return Ok(());
        }

        // Record stamp edges BEFORE building the task, so any later
        // consumer's expansion sees them (ninja emits producers before
        // consumers in the traversal order the driver walks).
        if build
            .cmdline
            .as_deref()
            .is_some_and(|c| c.contains("touch_file.py"))
        {
            let ins: Vec<FileId> = build.dirtying_ins().to_vec();
            for fid in build.outs() {
                self.stamp_inputs.insert(*fid, ins.clone());
            }
        }

        let tools = self.tools.clone();
        let task = self.new_task(files, build)?;

        // Acquire before spawning: bounds thread count AND daemon load.
        // Phony builds returned above and never consume a slot. The
        // permit moves into the thread and releases on drop, panic-safe.
        let permit = self.permits.acquire();

        let config = self.config.clone();
        let rpc_client = self.rpc_client.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            let (derived_path, err) =
                match build_task_derivation(tools.clone(), &rpc_client, task.clone()) {
                    Ok(drv) => match handle_derivation_result(
                        tools.clone(),
                        &rpc_client,
                        task.clone(),
                        drv.clone(),
                        &config,
                    ) {
                        Ok(final_derived_path) => (Some(final_derived_path), None),
                        Err(err) => {
                            // Cause FIRST and no multi-megabyte JSON dump:
                            // at Chromium scale the dump swallowed the cause
                            // chain and cost blind debugging rounds.
                            (None, Some(err.context(format!(
                                "Failed to handle derivation result for task (derivation: {}, {} inputs)",
                                drv.name,
                                task.inputs.len()
                            ))))
                        }
                    },
                    Err(err) => (None, Some(err.context("Failed to build task derivation for task".to_string()))),
                };

            // Create DerivedFiles for all outputs if successful
            let derived_files = if let Some(ref final_derived_path) = derived_path {
                let mut drv_outputs: Vec<DerivedFile> = Vec::new();
                for fid in task.outs() {
                    let file = &task.files[fid];
                    // Normalization already happened in new_task for
                    // task.outputs; apply the same here so the recorded
                    // DerivedFile agrees with what the task wrote.
                    match normalize_build_path(&config.build_dir, file.name.clone().into()) {
                        Ok(p) => {
                            drv_outputs.push(new_built_file(final_derived_path.clone(), p))
                        }
                        Err(e) => {
                            eprintln!("Error: {e}");
                        }
                    }
                }
                drv_outputs
            } else {
                Vec::new()
            };

            let result = BuildResult {
                bid,
                derived_path,
                derived_files,
                err,
            };
            let _ = tx.send(result);
        });

        Ok(())
    }

    /// Returns the finished build and whether it SUCCEEDED. A failure is
    /// fully reported here (error chain to stderr); the scheduler decides
    /// whether it is fatal - under keep-going it abandons the failed
    /// subtree and drains the rest, so one round surfaces every
    /// independent failure instead of the first.
    pub fn wait(&mut self, files: &mut graph::GraphFiles) -> Result<(BuildId, bool)> {
        let result = self.rx.recv().unwrap();
        if let Some(err) = result.err {
            eprintln!("Error: {err}");

            eprintln!("Caused by:");
            for cause in err.chain().skip(1) {
                eprintln!("    {cause}");
            }

            let debug_info = if let Some(derived_path) = &result.derived_path {
                format!(
                    "derivation: {}",
                    self.config.store_dir.display(derived_path)
                )
            } else {
                format!("build_id: {:?}", result.bid)
            };
            eprintln!("Failed to build task derivation for {}", debug_info);

            return Ok((result.bid, false));
        }

        for derived_file in result.derived_files {
            self.add_derived_file(files, derived_file.clone());
        }

        Ok((result.bid, true))
    }

    fn add_derived_file(
        &mut self,
        files: &mut graph::GraphFiles,
        derived_file: DerivedFile,
    ) -> FileId {
        let path_str = derived_file.build_path.to_string_lossy().into_owned();
        let fid = match files.lookup(&path_str) {
            Some(fid) => fid,
            None => files.id_from_canonical(path_str),
        };

        self.derived_files.entry(fid).or_insert(derived_file);

        fid
    }

    fn new_task(&mut self, files: &mut graph::GraphFiles, build: &Build) -> Result<Task> {
        let store_dir = self.config.store_dir.to_string();

        // Provide the task access to all the original files for explicit
        // inputs and implicit/explicit outputs.
        let mut build_files: HashMap<FileId, File> = HashMap::new();
        for fid in build.ordering_ins().iter().chain(build.outs()) {
            build_files.insert(*fid, files.by_id[*fid].clone());
        }

        // Iterate over all explict, implicit and order-only dependencies as
        // they must all be linked into the derivation's source directory.
        let mut input_set: HashMap<PathBuf, DerivedFile> = HashMap::new();
        // Expand phony aliases (transitively) into the real inputs behind
        // them. `via_phony` marks fids reached through an expansion: a
        // pure-ordering token that is not a file (CMake emits `phony || .`,
        // the build dir itself) is silently dropped there, whereas a
        // missing DIRECT input stays a loud error as before.
        let is_gcc_task = build.deps.as_deref() == Some("gcc");
        let mut worklist: Vec<(FileId, bool)> =
            build.ordering_ins().iter().map(|f| (*f, false)).collect();
        let mut seen: std::collections::HashSet<FileId> = std::collections::HashSet::new();
        while let Some((fid, via_phony)) = worklist.pop() {
            if !seen.insert(fid) {
                continue;
            }
            if let Some(alias_ins) = self.phony_aliases.get(&fid) {
                worklist.extend(alias_ins.iter().map(|f| (*f, true)));
                continue;
            }
            // A stamp file certifies its edge's inputs EXIST; it carries no
            // content. On a shared filesystem depending on the stamp is
            // enough, but in the task sandbox the certified files must be
            // materialized too: perfetto's trace_processor_table_generator
            // stamps python package files that gen_tp_table_headers.py then
            // imports from the repo root. So a stamp dependency also
            // enqueues the stamp edge's own inputs, marked via_phony so the
            // gcc header filter and the not-a-file drop still apply. The
            // stamp fid itself continues below as a normal task-output dep.
            if let Some(extra) = self.stamp_inputs.get(&fid) {
                worklist.extend(extra.iter().map(|f| (*f, true)));
            }
            // For a compile (deps=gcc), a phony-EXPANDED order-only dep is
            // only a real input if it is header-shaped: expansion of GN's
            // inputdeps phonies otherwise drags generated OBJECTS and
            // SOURCES (perfetto's entire .gen.o world, measured) into one
            // TU's closure. Directly declared order-only deps (a generated
            // buildflags header on the edge itself) are never filtered.
            if via_phony && is_gcc_task {
                let name = &files.by_id[fid].name;
                let header_like = name.ends_with(".h")
                    || name.ends_with(".hpp")
                    || name.ends_with(".hh")
                    || name.ends_with(".inc")
                    || name.ends_with(".ipp");
                if !header_like {
                    continue;
                }
            }
            let input = match self.derived_files.get(&fid) {
                Some(df) => df.to_owned(),
                None => {
                    let file = &files.by_id[fid];
                    if via_phony && !Path::new(&file.name).is_file() {
                        continue;
                    }
                    if file.name.starts_with(&store_dir) {
                        // TODO: Perhaps need to add this as inputSrc? But
                        // will also have to change DerivedFile to have source
                        // Option<PathBuf>, because we don't want it to be
                        // added to $NIX_NINJA_INPUTS.
                        // DerivedFile {
                        //     path: SingleDerivedPath::Opaque(StorePath::new(file.name)),
                        //     source: &file.name,
                        // }
                        continue;
                    }

                    let uploaded = upload_referenced_file(
                        &self.rpc_client,
                        &self.config.build_dir,
                        PathBuf::from(&file.name),
                    )?;
                    for extra in &uploaded[1..] {
                        self.add_derived_file(files, extra.clone());
                        input_set.insert(extra.build_path.clone(), extra.clone());
                    }
                    let input = uploaded.into_iter().next().unwrap();
                    self.add_derived_file(files, input.clone());
                    input
                }
            };
            input_set.insert(input.build_path.clone(), input.clone());
        }

        let mut outputs: Vec<PathBuf> = Vec::new();
        for fid in build.outs() {
            let file = &files.by_id[*fid];
            // See normalize_build_path: an absolute output escapes the
            // task sandbox via Path::join's prefix-discarding semantics.
            outputs.push(normalize_build_path(
                &self.config.build_dir,
                PathBuf::from(&file.name),
            )?);
        }

        // Meson resolves `files(...)` in custom_target commands to absolute
        // paths at configure time. Those paths do not exist inside the task
        // sandbox, where inputs are symlinked at their build-dir-relative
        // locations, so rewrite in-tree absolute paths to relative ones
        // (mirroring `relative_from` in `new_opaque_file`).
        // GN goes further than meson: it bakes absolute paths to ANY
        // ancestor of the build dir (qtwebengine's gen_icui18n_shim passes
        // --headers-root five levels up, and the script computes
        // relpath(root, source_tree) from it, silently writing its outputs
        // outside the declared tree when root is a host-absolute path). So
        // rewrite every ancestor prefix, deepest first - each occurrence of
        // "<ancestor>/" becomes the "../" chain that reaches it from the
        // build dir. Deepest-first matters: a deeper prefix contains every
        // shallower one. Stop above 3 components so "/home/", "/nix/" and
        // "/" are never rewritten.
        let cmdline = build
            .cmdline
            .as_ref()
            .map(|cmdline| rewrite_ancestor_paths(cmdline, &self.config.build_dir));

        // TODO: Can we avoid this? Technically the build rule isn't complete.
        //
        // The command may reference a file pre-generated by the configuration
        // step. We tracked files that existed in the build directory
        // beforehand, so we can see if there's anything that matches and add
        // it as an explicit input.
        if let Some(cmdline) = &cmdline {
            let args = shell_words::split(cmdline)?;
            for arg in args {
                let Some(fid) = files.lookup(&arg) else {
                    // Not a graph node - but GN commands reference source
                    // scripts the graph never declares (gcc_link_wrapper.py
                    // in every host link rule), assuming the runner shares
                    // the filesystem. A relative arg naming a real file is
                    // a task input; upload it. is_file skips -I dirs and
                    // not-yet-existing outputs; absolute and flag-shaped
                    // args never reach the check.
                    if !arg.starts_with('-')
                        && !arg.starts_with('/')
                        && arg.contains('/')
                        && Path::new(&arg).is_file()
                    {
                        for input in upload_referenced_file(
                            &self.rpc_client,
                            &self.config.build_dir,
                            PathBuf::from(&arg),
                        )? {
                            self.add_derived_file(files, input.clone());
                            input_set.insert(input.build_path.clone(), input);
                        }
                    } else if !arg.starts_with('-')
                        && !arg.starts_with('/')
                        && arg.starts_with("../")
                        // A pure ../ chain names a ROOT, not a directory of
                        // interest: GN passes the source root itself as an
                        // argument (round 35's ../../../.. hit the upload
                        // cap, which is the cap doing its job). Require a
                        // named component after the climb.
                        && Path::new(&arg)
                            .components()
                            .any(|c| matches!(c, std::path::Component::Normal(_)))
                        && Path::new(&arg).is_dir()
                    {
                        // A relative arg naming a real SOURCE-TREE directory
                        // is the command asking for that directory's
                        // contents: dawn passes --jinja2-path, a
                        // --template-dir of jinja templates, and
                        // --markupsafe-path, and declares none of their
                        // files (upstream relies on the depfile from a
                        // PREVIOUS run plus a shared filesystem; a first
                        // run in a sandbox has neither). Bounded walk, and
                        // only for dirs that climb out of the build dir -
                        // in-build-dir args (`gen`, output dirs) are the
                        // task's own output space, not inputs.
                        for input in upload_referenced_dir(
                            &self.rpc_client,
                            &self.config.build_dir,
                            Path::new(&arg),
                        )? {
                            self.add_derived_file(files, input.clone());
                            input_set.insert(input.build_path.clone(), input);
                        }
                    }
                    continue;
                };
                let input = match self.derived_files.get(&fid) {
                    Some(derived_file) => derived_file.clone(),
                    None => match self.build_dir_inputs.get(&fid) {
                        Some(derived_file) => derived_file.clone(),
                        None => {
                            // A graph NODE with no derived file yet is the
                            // same case as a non-node: if it names a real
                            // source file, it is a task input (the silent
                            // `continue` here dropped gcc_link_wrapper.py
                            // whenever another rule declared it as a node).
                            if !arg.starts_with('-')
                                && !arg.starts_with('/')
                                && arg.contains('/')
                                && Path::new(&arg).is_file()
                            {
                                for input in upload_referenced_file(
                                    &self.rpc_client,
                                    &self.config.build_dir,
                                    PathBuf::from(&arg),
                                )? {
                                    self.add_derived_file(files, input.clone());
                                    input_set.insert(input.build_path.clone(), input);
                                }
                            }
                            continue;
                        }
                    },
                };
                input_set.insert(input.build_path.clone(), input);
            }
        }

        // TODO: Can we avoid this? Technically the build rule isn't complete.
        //
        // Currently need this because there are rules that depend on
        // configuration phase generated files in Cpp Nix for example
        // `src/libutil/config-util.hh` which has a command like:
        // `-Isrc/libutil -include config-util.hh`.
        //
        // One way is to parse all the includes, then add it to our search
        // path above.
        //
        // BOUNDED, because at Chromium scale this blanket is a memory
        // bomb: injecting every configure-generated file into EVERY task
        // makes each derivation's input map multi-megabyte, and the
        // daemon's per-connection workers hold those derivations - seven
        // workers at ~2 GiB RSS each, measured, squeezing the machine to
        // 2.5 GiB available. Past the threshold the blanket is also
        // redundant: explicit inputs, cmdline references (matched above)
        // and depfile discovery carry the real dependencies. 512 covers
        // the meson projects this hack was written for (dbus: dozens).
        const IMPLICIT_INPUTS_LIMIT: usize = 512;
        if self.build_dir_inputs.len() <= IMPLICIT_INPUTS_LIMIT {
            for input in self.build_dir_inputs.values() {
                input_set.insert(input.build_path.clone(), input.clone());
            }
        }

        // Post-pass, closing the class: a .py input must ALWAYS travel
        // with its same-directory siblings, no matter which of the input
        // paths (ordering-ins, cmdline node, cmdline non-node, or a
        // derived_files HIT from an earlier task's upload) put it in the
        // set. The per-branch versions of this rule missed the HIT case:
        // the second link task found gcc_link_wrapper.py already
        // registered and attached it alone. Uploads are content-cached,
        // so re-encountering a directory is cheap.
        let py_inputs: Vec<PathBuf> = input_set
            .values()
            .filter(|i| i.build_path.extension().is_some_and(|e| e == "py"))
            .map(|i| i.build_path.clone())
            .collect();
        for py in py_inputs {
            if !Path::new(&py).is_file() {
                continue;
            }
            for sib in upload_referenced_file(&self.rpc_client, &self.config.build_dir, py)? {
                self.add_derived_file(files, sib.clone());
                input_set.entry(sib.build_path.clone()).or_insert(sib);
            }
        }

        // grit manifests include partials and translations TEXTUALLY
        // (<part file="x.grdp">, <file path="y.xtb">), resolved relative
        // to the manifest's own directory, and GN declares none of them -
        // round 39 died at FileNotFound: address_input_strings.grdp. Same
        // worklist shape as the python-sibling pass; .grdp partials nest,
        // so found manifests re-enter the list.
        let mut grd_list: Vec<PathBuf> = input_set
            .keys()
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e == "grd" || e == "grdp")
            })
            .cloned()
            .collect();
        while let Some(grd) = grd_list.pop() {
            if !Path::new(&grd).is_file() {
                continue;
            }
            for r in grd_references(&grd)? {
                if input_set.contains_key(&r) {
                    continue;
                }
                let up = new_opaque_file(&self.rpc_client, &self.config.build_dir, r.clone())?;
                self.add_derived_file(files, up.clone());
                input_set.insert(up.build_path.clone(), up);
                if r.extension().is_some_and(|e| e == "grdp") {
                    grd_list.push(r);
                }
            }
        }

        let mut inputs: Vec<DerivedFile> = input_set.into_values().collect();
        inputs.sort();

        // Extract store paths from cmdline and add pre-extracted wrapper store paths
        let mut input_srcs = self.wrapper_store_paths.clone();
        if let Some(cmdline) = &cmdline {
            let found_store_paths =
                extract_store_paths(&self.config.store_dir, &self.store_regex, cmdline)?;
            input_srcs.extend(found_store_paths);
        }

        Ok(Task {
            system: self.config.system.clone(),
            wrapper_vars: self.wrapper_vars.clone(),
            input_srcs,
            build_dir: self.config.build_dir.clone(),
            build_deps: build.dependencies.clone(),
            store_dir: self.config.store_dir.clone(),
            cmdline,
            desc: build.desc.clone(),
            deps: build.deps.clone(),
            rspfile: build
                .rspfile
                .as_ref()
                .map(|r| (r.path.clone(), r.content.clone())),
            files: build_files,
            inputs,
            outputs,
        })
    }
}

fn build_task_derivation(
    tools: Tools,
    rpc_client: &Arc<BuilderRpcClient>,
    task: Task,
) -> Result<Derivation> {
    let cmdline = match &task.cmdline {
        Some(c) => c,
        None => {
            return Err(anyhow!("Phony tasks not yet supported"));
        }
    };

    let mut drv = Derivation::new(
        "ninja-build".parse()?,
        task.system.clone().into_bytes().into(),
        format!(
            "{}/bin/nix-ninja-task",
            task.store_dir.display(&tools.nix_ninja_task)
        )
        .into_bytes()
        .into(),
    );

    drv.args.push(cmdline.to_string().into_bytes().into());

    if let Some(desc) = &task.desc {
        drv.args
            .push(format!("--description={desc}").into_bytes().into());
    }

    // Propagate wrapper environment variables to the task.
    for (key, value) in &task.wrapper_vars {
        let final_value = if key.starts_with("NIX_CFLAGS_COMPILE") {
            // Also add a deterministic random seed based on the task's
            // cmdline for reproducible builds.
            let deterministic_seed = generate_frandom_seed(cmdline);
            format!("{value} -frandom-seed={deterministic_seed}")
        } else {
            value.clone()
        };
        drv.env.insert(
            key.clone().into_bytes().into(),
            final_value.into_bytes().into(),
        );
    }

    // Add pre-extracted store paths from cmdline and wrapper vars
    for store_path in &task.input_srcs {
        drv.inputs
            .insert(SingleDerivedPath::Opaque(store_path.clone()));
    }

    // Needed by all tasks.
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.cc.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.coreutils.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.nix_ninja_task.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.patchelf.clone()));

    // Add all ninja build inputs.
    let mut input_set: HashSet<String> = HashSet::new();
    for input in &task.inputs {
        // Declare input for derivation.
        drv.inputs.insert(input.derived_path.clone());

        // Encode input for nix-ninja-task.
        let encoded = &input.to_encoded(&task.store_dir);
        input_set.insert(encoded.clone());
    }

    // Handle when rule's dep = gcc, which means we need to find all the
    // implicit header dependencies normally handled by gcc's depfiles.
    let mut discovered_inputs: Vec<DerivedFile> = Vec::new();
    if let Some(deps) = &task.deps {
        if deps == "gcc" {
            // Only opaque inputs are processed by gcc
            let files: Vec<PathBuf> = task
                .inputs
                .iter()
                .filter_map(|input| match input.derived_path {
                    SingleDerivedPath::Opaque(_) => Some(input.build_path.clone()),
                    SingleDerivedPath::Built { .. } => None, // Will be filled in by dynamic task derivation
                })
                .collect();

            let (discovered_deps, discovered_store_paths) = discover_c_includes(
                rpc_client,
                &task.store_dir,
                &task.build_dir,
                cmdline,
                files,
                None,
            )?;

            // Add discovered store paths as input sources only
            for store_path in discovered_store_paths {
                drv.inputs
                    .insert(SingleDerivedPath::Opaque(store_path.clone()));
            }

            // Add discovered deps to NIX_NINJA_INPUTS and derivation
            for derived_file in discovered_deps {
                let encoded = derived_file.to_encoded(&task.store_dir);

                // Skip if already in input_set
                if input_set.contains(&encoded) {
                    continue;
                }

                input_set.insert(encoded);
                drv.inputs.insert(derived_file.derived_path.clone());
                discovered_inputs.push(derived_file);
            }
        }
    }

    // The sandbox build dir must be at least as DEEP as the longest
    // `..` climb any input or command token makes: the default
    // /build/source/build is 3 deep, and Chromium's GN graph references
    // sources 5 levels up (../../../../../src/...), which escaped to
    // filesystem root and died at mkdir /src. Mirror the trailing
    // components of the REAL build dir so relative paths resolve to the
    // same names on either side.
    let cmd_climb = cmdline
        .split_whitespace()
        .map(|tok| leading_parent_components(Path::new(tok)))
        .max()
        .unwrap_or(0);
    // Computed AFTER discovery merging: discovered includes (and python
    // siblings) also climb, and a max_up taken before they were added
    // left this task a 3-deep sandbox for 5-up inputs, escaping to /.
    let max_up = task
        .inputs
        .iter()
        .map(|i| leading_parent_components(&i.build_path))
        .chain(
            discovered_inputs
                .iter()
                .map(|i| leading_parent_components(&i.build_path)),
        )
        .max()
        .unwrap_or(0)
        .max(cmd_climb);
    if max_up > 0 {
        let comps: Vec<_> = task.build_dir.components().collect();
        if comps.len() < max_up {
            return Err(anyhow!(
                "inputs climb {} levels above build dir {}, which has only {} components",
                max_up,
                task.build_dir.display(),
                comps.len()
            ));
        }
        let mirrored: PathBuf = comps[comps.len() - max_up..].iter().collect();
        let sandbox_build_dir = Path::new("/build/source").join(mirrored);
        drv.args.push(b"--build-dir"[..].into());
        drv.args.push(
            sandbox_build_dir
                .to_string_lossy()
                .into_owned()
                .into_bytes()
                .into(),
        );
    }

    // Sort NIX_NINJA_INPUTS to ensure determinism.
    let mut inputs: Vec<String> = input_set.into_iter().collect();
    inputs.sort();

    drv.env.insert(
        b"NIX_NINJA_INPUTS"[..].into(),
        inputs.join(" ").into_bytes().into(),
    );

    // Add all ninja build outputs.
    let mut outputs: Vec<String> = Vec::new();
    for output_path in &task.outputs {
        // Declare a content addressed output.
        let normalized_name = normalize_output(&output_path.to_string_lossy());
        drv.outputs.insert(
            normalized_name.parse()?,
            DerivationOutput::CAFloating(ContentAddressMethodAlgorithm::NixArchive(
                harmonia_utils_hash::Algorithm::SHA256,
            )),
        );

        // Create a placeholder and encode output for nix-ninja-task.
        let placeholder = Placeholder::standard_output(
            &OutputName::from_str(&normalized_name).context("While creating placeholder")?,
        );
        let encoded = format!(
            "{}:{}:{}",
            placeholder.render().display(),
            output_path.display(),
            output_path.display(),
        );
        outputs.push(encoded);
    }
    drv.env.insert(
        b"NIX_NINJA_OUTPUTS"[..].into(),
        outputs.join(" ").into_bytes().into(),
    );

    // Ninja rspfile support: the task writes this file (relative to its
    // build dir) before spawning the command. Content rides passAsFile
    // beside the input map - rsp files exist precisely because their
    // content is too large for a command line.
    let mut pass_as_file = String::from("NIX_NINJA_INPUTS NIX_NINJA_OUTPUTS");
    if let Some((rsp_path, rsp_content)) = &task.rspfile {
        drv.env.insert(
            b"NIX_NINJA_RSPFILE_PATH"[..].into(),
            rsp_path.to_string_lossy().into_owned().into_bytes().into(),
        );
        drv.env.insert(
            b"NIX_NINJA_RSPFILE_CONTENT"[..].into(),
            rsp_content.clone().into_bytes().into(),
        );
        pass_as_file.push_str(" NIX_NINJA_RSPFILE_CONTENT");
    }

    // At Chromium scale the encoded input map is megabytes, and an env
    // var over the kernel's per-string exec limit kills the task with
    // "Argument list too long" before it starts. passAsFile is nix's
    // own mechanism for oversized attrs: the daemon writes the value
    // to a file AFTER placeholder substitution (so derived-path
    // placeholders still resolve) and hands the builder
    // NIX_NINJA_INPUTSPath / NIX_NINJA_OUTPUTSPath instead.
    drv.env.insert(
        b"passAsFile"[..].into(),
        pass_as_file.into_bytes().into(),
    );

    {
        // Prepare $PATH to have coreutils.
        let mut path: Vec<String> = vec![
            format!("{}/bin", task.store_dir.display(&tools.cc)),
            format!("{}/bin", task.store_dir.display(&tools.coreutils)),
            format!("{}/bin", task.store_dir.display(&tools.patchelf)),
        ];

        // CMake emits link and custom commands wrapped in shell no-op
        // guards: `: && <real command> && :`. The `:` is a shell builtin,
        // fine at execution time, but it is not a binary to resolve - skip
        // leading no-op tokens to find the real tool.
        let cmdline_binary = cmdline
            .split_whitespace()
            .find(|tok| *tok != ":" && *tok != "&&")
            .ok_or_else(|| anyhow!("No command found in cmdline"))?
            // GN quotes interpreter paths ("…/python3.14"); the shell
            // strips the quotes at exec time, so strip them here too or
            // `which` is asked for a name with literal quote characters.
            .trim_matches(|c| c == '"' || c == '\'');

        // A command resolving outside the store (e.g. a `../gen.sh` script
        // from the source tree) is a task input handled by
        // `new_opaque_file` — it reaches the sandbox as an input symlink,
        // so only store binaries become inputs and PATH entries here.
        if let Some(cmdline_path) = which_store_path_opt(&task.store_dir, cmdline_binary)? {
            drv.inputs
                .insert(SingleDerivedPath::Opaque(cmdline_path.clone()));
            path.push(format!("{}/bin", task.store_dir.display(&cmdline_path)));
        }
        drv.env
            .insert(b"PATH"[..].into(), path.join(":").into_bytes().into());
    }

    // For debugging purposes:
    // let json = serde_json::to_string_pretty(&drv)?;
    // println!("Derivation:\n{json}");

    Ok(drv)
}

// For dynamic tasks, we generate an intermediary derivation that will then
// generate the final derivation with any discovered dependencies from its
// dependencies.
//
// For example, if a task derivation depends on generated.cc, we also want
// to depend on any headers generated.cc includes but we don't know that
// without the derivation that built generated.cc also scanned for includes
// and wrote that to its $deps output.
fn build_dynamic_task_derivation(
    tools: Tools,
    rpc_client: &Arc<BuilderRpcClient>,
    store_dir: &StoreDir,
    input_drv: Derivation,
    built_inputs: Vec<DerivedFile>,
) -> Result<Derivation> {
    let mut drv = Derivation::new(
        format!("{}.drv", input_drv.name).parse()?,
        input_drv.platform.clone(),
        format!("{}/bin/nix-ninja", store_dir.display(&tools.nix_ninja))
            .into_bytes()
            .into(),
    );
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.nix_ninja.clone()));
    drv.inputs
        .insert(SingleDerivedPath::Opaque(tools.nix.clone()));

    // Add built inputs as dependencies so the dynamic task has access to them for scanning
    for built_input in &built_inputs {
        drv.inputs.insert(built_input.derived_path.clone());
    }

    // Encode built inputs for NIX_NINJA_INPUTS so dynamic task can process them
    let mut inputs: Vec<String> = built_inputs
        .iter()
        .map(|input| input.to_encoded(store_dir))
        .collect();
    inputs.sort();
    drv.env.insert(
        b"NIX_NINJA_INPUTS"[..].into(),
        inputs.join(" ").into_bytes().into(),
    );
    // Same E2BIG hazard as the task derivation: see the passAsFile
    // comment in build_task_derivation.
    drv.env.insert(
        b"passAsFile"[..].into(),
        b"NIX_NINJA_INPUTS"[..].into(),
    );

    drv.outputs.insert(
        "out".parse()?,
        DerivationOutput::CAFloating(ContentAddressMethodAlgorithm::Text),
    );
    drv.env.insert(
        b"out"[..].into(),
        Placeholder::standard_output(&OutputName::from_str("out")?)
            .render()
            .into_os_string()
            .into_encoded_bytes()
            .into(),
    );

    // Add the dynamic-task subtool argument
    drv.args.push(b"-t"[..].into());
    drv.args.push(b"dynamic-task"[..].into());

    // builder-rpc-v0 does not auto-export $name, but the outer-output rename needs it.
    let dyn_drv_name = format!("{}.drv", input_drv.name);
    drv.env
        .insert(b"name"[..].into(), dyn_drv_name.into_bytes().into());

    // Propagate sources to dynamic task for it discover inputs.
    let src = env::var("src").map_err(|_| anyhow!("Expected $src to be set"))?;
    drv.env
        .insert(b"src"[..].into(), src.clone().into_bytes().into());
    let src_store_path: StorePath = store_dir.parse(&src)?;
    drv.inputs
        .insert(SingleDerivedPath::Opaque(src_store_path.clone()));

    // Set up PATH to include nix binary
    let path = format!("{}/bin", store_dir.display(&tools.nix));
    drv.env.insert(b"PATH"[..].into(), path.into_bytes().into());

    // Requires extra experimental features to add our derivations.
    drv.env.insert(
        b"NIX_CONFIG"[..].into(),
        b"extra-experimental-features = nix-command ca-derivations dynamic-derivations"[..].into(),
    );

    // Require builder-rpc-v0 so the dynamic-task drv runs in a sandbox
    // where the daemon socket is bind-mounted in restricted mode (see
    // NixOS/nix#15793). nix-ninja's inside-sandbox calls then go through
    // the worker protocol's restricted allowlist instead of recursive-nix.
    drv.env.insert(
        b"requiredSystemFeatures"[..].into(),
        b"builder-rpc-v0"[..].into(),
    );

    // Serialize the derivation and add it to the nix store
    let drv_json = serde_json::to_string(&input_drv)?;
    let drv_json_name = format!("drv-{}.json", input_drv.name);
    let drv_json_path = rpc_client.add_to_store_text(&drv_json_name, drv_json.as_bytes())?;

    // Add derivation.json as input dependency and argument
    drv.inputs
        .insert(SingleDerivedPath::Opaque(drv_json_path.clone()));
    drv.args.push(
        store_dir
            .display(&drv_json_path)
            .to_string()
            .into_bytes()
            .into(),
    );

    Ok(drv)
}

/// Handles the result of build_task_derivation, deciding whether to wrap with
/// a dynamic task derivation or use the derivation directly.
fn handle_derivation_result(
    tools: Tools,
    rpc_client: &Arc<BuilderRpcClient>,
    task: Task,
    mut drv: Derivation,
    config: &RunnerConfig,
) -> Result<SingleDerivedPath> {
    // Collect built inputs when deps == "gcc" for dynamic dependency discovery
    let built_inputs: Vec<DerivedFile> = if task.deps.as_ref() == Some(&"gcc".to_string()) {
        task.inputs
            .iter()
            .filter(|input| matches!(input.derived_path, SingleDerivedPath::Built { .. }))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    if !built_inputs.is_empty() {
        // If we're in Nix sandbox, create a dynamic derivation to handle
        // dynamic dependencies.
        if config.is_output_derivation {
            let dynamic_drv = build_dynamic_task_derivation(
                tools.clone(),
                rpc_client,
                &config.store_dir,
                drv,
                built_inputs,
            )?;
            let dynamic_drv_path = rpc_client.add_drv_to_store(&config.store_dir, &dynamic_drv)?;
            Ok(SingleDerivedPath::Built {
                drv_path: Arc::new(SingleDerivedPath::Opaque(dynamic_drv_path)),
                output: "out".parse().unwrap(),
            })
        } else {
            // Otherwise, symlink these built_inputs into build_dir and do
            // dependency discovery locally.

            let built_paths =
                local::build_derived_files(rpc_client, &config.store_dir, &built_inputs)?;

            let (discovered_deps, discovered_store_paths) =
                dynamic_task::discover_dynamic_dependencies(
                    rpc_client,
                    &config.store_dir,
                    &config.build_dir,
                    &drv,
                    built_paths,
                )?;

            dynamic_task::update_derivation_with_discoveries(
                &mut drv,
                discovered_deps,
                discovered_store_paths,
                &config.store_dir,
            )?;

            let drv_path = rpc_client.add_drv_to_store(&config.store_dir, &drv)?;
            Ok(SingleDerivedPath::Opaque(drv_path))
        }
    } else {
        let drv_path = rpc_client.add_drv_to_store(&config.store_dir, &drv)?;
        Ok(SingleDerivedPath::Opaque(drv_path))
    }
}

pub fn which_store_path(store_dir: &StoreDir, binary_name: &str) -> Result<StorePath> {
    which_store_path_opt(store_dir, binary_name)?.ok_or_else(|| {
        anyhow!(
            "{} resolved to a path outside the Nix store ({})",
            binary_name,
            store_dir
        )
    })
}

/// Like [`which_store_path`], but returns `None` for a binary that resolves
/// outside the Nix store (e.g. a script in the source tree).
fn which_store_path_opt(store_dir: &StoreDir, binary_name: &str) -> Result<Option<StorePath>> {
    let binary_path =
        which(binary_name).map_err(|err| anyhow!("Failed to find {}: {}", binary_name, err))?;

    // Canonicalize will resolve all symlinks and return an absolute path
    let canonical_path = std::fs::canonicalize(&binary_path).with_context(|| {
        format!(
            "canonicalize {} (from PATH lookup of {})",
            binary_path.display(),
            binary_name
        )
    })?;

    if !canonical_path.starts_with(store_dir) {
        return Ok(None);
    }

    let store_path_dir = canonical_path
        .parent() // Get bin/ directory
        .and_then(|p| p.parent()) // Get the store path ($out)
        .ok_or_else(|| anyhow!("Cannot determine store path from binary: {}", binary_name))?
        .to_str()
        .context("Path was not valid UTF-8")?;

    store_dir
        .parse(store_path_dir)
        .context("Failed to parse store path")
        .map(Some)
}

fn extract_store_paths(
    store_dir: &StoreDir,
    store_regex: &Regex,
    s: &str,
) -> Result<Vec<StorePath>> {
    let mut store_paths = Vec::new();
    for cap in store_regex.find_iter(s) {
        let store_path: StorePath = store_dir.parse(cap.as_str())?;
        if store_path.is_derivation() {
            continue;
        }
        if !store_path.to_absolute_path(store_dir).exists() {
            continue;
        }
        store_paths.push(store_path);
    }
    Ok(store_paths)
}

fn new_opaque_file(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &std::path::Path,
    path: PathBuf,
) -> Result<DerivedFile> {
    let relative_path = relative_from(&path, build_dir).unwrap_or(path);
    let mut path = relative_path
        .to_str()
        .context("Path was not valid UTF-8")?
        .to_owned();
    canon::canonicalize_path(&mut path);

    let canonical_path = fs::canonicalize(&path).with_context(|| format!("canonicalize {path}"))?;

    // builder-rpc-v0 restricts the allowlist; `nix store add` issues
    // AddTempRoot which the daemon refuses. Upload as a NAR-hashed CA
    // object instead, which (unlike a text-CA object) preserves the file
    // mode, notably the executable bit.
    // Route through normalize_output: a raw file_name() containing a
    // character outside the store-name grammar (e.g. `@`) is refused by
    // AddToStore. See normalize_output for the grammar and why the lossy
    // map is sound.
    let name = canonical_path
        .file_name()
        .map(|n| normalize_output(&n.to_string_lossy()))
        .unwrap_or_else(|| "source".to_string());
    let store_path = rpc_client.add_to_store_nar(&name, &canonical_path)?;

    Ok(DerivedFile {
        derived_path: SingleDerivedPath::Opaque(store_path),
        build_path: relative_path,
        rel_path: None, // None for opaque files - store path points directly to file
    })
}

/// An ABSOLUTE build_path is a sandbox escape: nix-ninja-task joins it
/// onto its build dir with Path::join, whose semantics DISCARD the
/// prefix for absolute paths, so the task then mkdirs the host path
/// inside the sandbox and dies EACCES (CMake's ${cmake_ninja_workdir}
/// and GN actions both emit absolute workdir paths; meson never does,
/// which is why five packages built before Chromium's graph hit this).
/// Relativize against the build dir; refuse LOUDLY anything that
/// escapes it rather than silently relocating an unknown path.
/// Number of leading `..` components of a relative path: how many
/// levels above its base directory it reaches before descending.
/// Rewrite absolute paths under any ancestor of the build dir into
/// build-dir-relative ones. Meson bakes `files(...)` results and GN bakes
/// arguments like gen_icui18n_shim's `--headers-root` as absolute host
/// paths; neither exists inside the task sandbox, where the source tree is
/// materialized at build-dir-relative locations. Each occurrence of
/// "<ancestor>/" becomes the "../" chain reaching it from the build dir.
/// Deepest first, because a deeper prefix contains every shallower one;
/// stops above 3 path components so "/home/", "/nix/" and "/" are never
/// rewritten. The cmd_climb scan below the discovery loop keeps the sandbox
/// deep enough for whatever "../" chains this emits.
fn rewrite_ancestor_paths(cmdline: &str, build_dir: &Path) -> String {
    let mut cmdline = cmdline.to_string();
    let mut ups = 0usize;
    let mut ancestor = Some(build_dir);
    while let Some(dir) = ancestor {
        if dir.components().count() < 3 {
            break;
        }
        if let Some(dir) = dir.to_str() {
            cmdline = cmdline.replace(&format!("{dir}/"), &"../".repeat(ups));
        }
        ups += 1;
        ancestor = dir.parent();
    }
    cmdline
}

fn leading_parent_components(p: &Path) -> usize {
    p.components()
        .take_while(|c| matches!(c, std::path::Component::ParentDir))
        .count()
}

/// Upload one referenced source file, plus - for a python script - its
/// same-directory .py siblings (gcc_link_wrapper.py imports
/// wrapper_utils.py; python resolves sibling imports from the script's
/// own directory). Returns every DerivedFile created, main file first.
/// This is THE upload path for referenced files: the ordering-ins loop,
/// the cmdline scan's node branch and its non-node branch all route
/// here, because the sibling rule was first added to only two of the
/// three and the third is where GN's declared `| script` inputs go.
fn upload_referenced_file(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    path: PathBuf,
) -> Result<Vec<DerivedFile>> {
    let is_py = path.extension().is_some_and(|e| e == "py");
    let main = new_opaque_file(rpc_client, build_dir, path.clone())?;
    let mut out = vec![main];
    if is_py {
        if let Some(dir) = path.parent() {
            let entries = fs::read_dir(dir).map_err(|e| {
                anyhow!("read_dir({}) for python siblings: {e}", dir.display())
            })?;
            for entry in entries.flatten() {
                let p = entry.path();
                if p == path {
                    continue;
                }
                if p.extension().is_some_and(|e| e == "py") && p.is_file() {
                    out.push(new_opaque_file(rpc_client, build_dir, p)?);
                } else if p.is_dir() && p.join("__init__.py").is_file() {
                    // A sibling package DIRECTORY: grit.py is a launcher
                    // whose first act is `import grit.grit_runner`,
                    // expecting tools/grit/grit/ beside it (126 files,
                    // measured). Over-cap siblings skip with a note
                    // rather than failing - an unrelated giant package
                    // beside a script must not kill the task; a needed
                    // one names itself at import time.
                    match walk_dir_capped(rpc_client, build_dir, &p, 512)? {
                        Some(files) => out.extend(files),
                        None => println!(
                            "nix-ninja: sibling package {} exceeds 512 files; skipped",
                            p.display()
                        ),
                    }
                }
            }
            // Imports the SIBLINGS cannot satisfy resolve one level up:
            // chromium keeps single-module tools in their own directory
            // (tools/json_comment_eater/json_comment_eater.py) and the
            // importer inserts `../<name>` into sys.path itself
            // (json_schema_compiler/json_parse.py does), so presence of
            // the files is the whole requirement - no PYTHONPATH change.
            // Names satisfied in-directory (a sibling module or package,
            // stdlib resolving to neither) are skipped; a dot in the
            // first segment cannot occur, import grammar forbids it.
            if let Some(parent) = dir.parent() {
                for name in python_import_names(dir)? {
                    if dir.join(format!("{name}.py")).is_file()
                        || dir.join(&name).is_dir()
                    {
                        continue;
                    }
                    let uncle = parent.join(&name);
                    if uncle.is_dir() {
                        match walk_dir_capped(rpc_client, build_dir, &uncle, 512)? {
                            Some(files) => out.extend(files),
                            None => println!(
                                "nix-ninja: uncle module dir {} exceeds 512 files; skipped",
                                uncle.display()
                            ),
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Upload every regular file under a directory the command names as an
/// argument (recursive), capped so a mistaken match cannot ingest a
/// source tree: past the cap the walk STOPS AND FAILS rather than
/// silently truncating - a partial package import fails stranger than a
/// named refusal. 512 covers dawn's jinja2 (~60 files incl. templates);
/// raise it deliberately if a real consumer needs more.
fn upload_referenced_dir(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
) -> Result<Vec<DerivedFile>> {
    const DIR_UPLOAD_CAP: usize = 512;
    match walk_dir_capped(rpc_client, build_dir, dir, DIR_UPLOAD_CAP)? {
        Some(mut files) => {
            // A package dir uploaded wholesale may import SIBLING
            // packages the command never names: jinja2 does
            // `from markupsafe import Markup`, dawn passes only
            // --jinja2-path, and upstream finds markupsafe beside it on
            // the shared filesystem. Scan the uploaded package's .py
            // files for top-level import names and upload each sibling
            // package they resolve to, one level (markupsafe imports
            // nothing further; a deeper chain will name itself when it
            // exists).
            if dir.join("__init__.py").is_file() {
                if let Some(parent) = dir.parent() {
                    for name in python_import_names(dir)? {
                        let sib = parent.join(&name);
                        if sib != dir && sib.join("__init__.py").is_file() {
                            if let Some(more) =
                                walk_dir_capped(rpc_client, build_dir, &sib, DIR_UPLOAD_CAP)?
                            {
                                files.extend(more);
                            }
                        }
                    }
                }
            }
            Ok(files)
        }
        None => {
            // Over the cap. Two conventions share this shape: dawn passes
            // the package dir itself (small, handled above), and
            // inspector_protocol passes the package's PARENT - all of
            // chromium's third_party, 310 entries - to sys.path.insert it
            // and import jinja2. Measured: only 6 of those 310 are python
            // packages, ~50 files. So the over-cap fallback uploads just
            // the immediate subdirectories that are importable packages
            // (an __init__.py at their root), each itself capped; a
            // package that ALSO busts the cap is a hard error, because a
            // partial package import fails stranger than a named refusal.
            let mut out = Vec::new();
            let entries = fs::read_dir(dir)
                .map_err(|e| anyhow!("read_dir({}) for dir arg: {e}", dir.display()))?;
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && p.join("__init__.py").is_file() {
                    match walk_dir_capped(rpc_client, build_dir, &p, DIR_UPLOAD_CAP)? {
                        Some(files) => out.extend(files),
                        None => {
                            return Err(anyhow!(
                                "python package {} holds more than {} files; \
                                 declare its files as inputs or raise \
                                 DIR_UPLOAD_CAP deliberately",
                                p.display(),
                                DIR_UPLOAD_CAP
                            ))
                        }
                    }
                }
            }
            println!(
                "nix-ninja: dir arg {} exceeds {} files; uploaded only its \
                 python packages ({} files)",
                dir.display(),
                DIR_UPLOAD_CAP,
                out.len()
            );
            Ok(out)
        }
    }
}

/// Files a grit manifest references textually: every file="..." and
/// path="..." attribute value, resolved against the manifest's own
/// directory, kept only where the file exists on disk (grd files also
/// carry output filenames in the same attributes; existence is the
/// discriminator, and a generated input would be a graph node already).
///
/// chrome_scaled_image structures resolve their file value through a
/// per-scale context directory INSERTED between the manifest dir and the
/// value (grit's ChromeScaledImage._FindInputFile), so a bare join misses
/// every image asset. The context names come from the top-level grd's
/// <output context="..."> nodes, which this per-file scan cannot see from
/// a .grdp, so the conventional chromium scale dirs are tried as well;
/// existence keeps the false candidates out, same as the base case.
fn grd_references(grd: &Path) -> Result<Vec<PathBuf>> {
    let body = fs::read_to_string(grd)
        .map_err(|e| anyhow!("read({}) for grd scan: {e}", grd.display()))?;
    let dir = grd.parent().unwrap_or(Path::new(""));
    let mut contexts: Vec<String> = ["default_100_percent", "default_200_percent", "default_300_percent"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for chunk in body.split("context=\"").skip(1) {
        if let Some(val) = chunk.split('"').next() {
            if !val.is_empty() && !contexts.iter().any(|c| c == val) {
                contexts.push(val.to_string());
            }
        }
    }
    let mut out = Vec::new();
    for attr in ["file=\"", "path=\""] {
        for chunk in body.split(attr).skip(1) {
            let Some(val) = chunk.split('"').next() else {
                continue;
            };
            if val.is_empty() || val.contains("://") {
                continue;
            }
            let p = dir.join(val);
            if p.is_file() {
                out.push(p);
                continue;
            }
            for ctx in &contexts {
                let p = dir.join(ctx).join(val);
                if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    Ok(out)
}

/// Top-level module names imported by a python package's own files:
/// `import X` and `from X import ...`, first path segment only. Line
/// matching by prefix, which cannot see an import inside a try block's
/// indentation - fine here, because an OPTIONAL import that fails is the
/// pattern try blocks exist for.
fn python_import_names(pkg: &Path) -> Result<Vec<String>> {
    let mut names = std::collections::BTreeSet::new();
    let entries = fs::read_dir(pkg)
        .map_err(|e| anyhow!("read_dir({}) for import scan: {e}", pkg.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.extension().is_some_and(|e| e == "py") {
            continue;
        }
        let body = fs::read_to_string(&p)
            .map_err(|e| anyhow!("read({}) for import scan: {e}", p.display()))?;
        for line in body.lines() {
            let rest = if let Some(r) = line.strip_prefix("import ") {
                r
            } else if let Some(r) = line.strip_prefix("from ") {
                r
            } else {
                continue;
            };
            let first = rest
                .split(|c: char| c == ' ' || c == '.' || c == ',')
                .next()
                .unwrap_or("");
            if !first.is_empty()
                && first
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                names.insert(first.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// Recursive regular-file upload of one directory, or None past the cap.
fn walk_dir_capped(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
    cap: usize,
) -> Result<Option<Vec<DerivedFile>>> {
    let mut paths = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = fs::read_dir(&d)
            .map_err(|e| anyhow!("read_dir({}) for dir arg: {e}", d.display()))?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                if paths.len() >= cap {
                    return Ok(None);
                }
                paths.push(p);
            }
        }
    }
    // Upload only after the whole walk fits the cap, so an over-cap dir
    // costs a directory scan and zero store writes.
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        out.push(new_opaque_file(rpc_client, build_dir, p)?);
    }
    Ok(Some(out))
}

fn normalize_build_path(build_dir: &Path, p: PathBuf) -> Result<PathBuf> {
    if p.is_relative() {
        return Ok(p);
    }
    match relative_from(&p, build_dir) {
        Some(rel) if rel.is_relative() => Ok(rel),
        _ => Err(anyhow!(
            "absolute path {} does not resolve under build dir {}; refusing to relocate it silently",
            p.display(),
            build_dir.display()
        )),
    }
}

fn new_built_file(derived_path: SingleDerivedPath, build_path: PathBuf) -> DerivedFile {
    let output_name = normalize_output(&build_path.to_string_lossy());
    DerivedFile {
        derived_path: SingleDerivedPath::Built {
            drv_path: Arc::new(derived_path),
            output: output_name.parse().expect("invalid output name"),
        },
        build_path: build_path.clone(),
        rel_path: Some(build_path),
    }
}

// Derivation outputs cannot have `/` in them as its suffixed to the derivation
// store path. More generally, nix store-path names admit only
// [A-Za-z0-9+._?=-] and must not start with a period: anything else
// (npm scope dirs like node_modules/@babel, mac assets like icon@2x.png,
// gettext's sr@latin.po) makes the daemon's AddToStore refuse the path,
// which poisons every target that transitively snapshots the file. The
// name is only a label - identity is the content hash - so a lossy map
// is sound; two names colliding after the map still yield distinct
// store paths unless their content is also identical.
fn normalize_output(output: &str) -> String {
    let mapped: String = output
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '.' | '_' | '?' | '=' => c,
            _ => '-',
        })
        .collect();
    if mapped.is_empty() {
        "source".to_string()
    } else if mapped.starts_with('.') {
        format!("-{mapped}")
    } else {
        mapped
    }
}

#[cfg(test)]
mod job_permits_tests {
    use super::JobPermits;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn concurrency_never_exceeds_cap() {
        let permits = Arc::new(JobPermits::new(3));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..20 {
            let permits = permits.clone();
            let running = running.clone();
            let peak = peak.clone();
            handles.push(std::thread::spawn(move || {
                let _permit = permits.acquire();
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(running.load(Ordering::SeqCst), 0, "all tasks completed");
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "peak concurrency {} exceeded cap 3",
            peak.load(Ordering::SeqCst)
        );
        assert!(
            peak.load(Ordering::SeqCst) >= 2,
            "peak {} suspiciously low - the test exercised no parallelism",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn permit_released_on_panic() {
        let permits = Arc::new(JobPermits::new(1));
        let p2 = permits.clone();
        let _ = std::thread::spawn(move || {
            let _permit = p2.acquire();
            panic!("task died");
        })
        .join();
        // If the panicking thread leaked its permit, this blocks forever
        // and the test times out; acquiring proves the Drop ran.
        let _permit = permits.acquire();
    }
}

#[cfg(test)]
mod python_import_names_tests {
    use super::python_import_names;

    #[test]
    fn finds_import_and_from_first_segment() {
        let d = std::env::temp_dir().join(format!("nn-imports-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("__init__.py"),
            "import os\nfrom markupsafe import Markup\nfrom .environment import E\nimport sys, re\n",
        )
        .unwrap();
        let names = python_import_names(&d).unwrap();
        assert!(names.contains(&"markupsafe".to_string()));
        assert!(names.contains(&"os".to_string()));
        // relative import's first segment is empty and must not appear
        assert!(!names.contains(&"".to_string()));
        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[cfg(test)]
mod rewrite_ancestor_paths_tests {
    use super::rewrite_ancestor_paths;
    use std::path::Path;

    // The gen_icui18n_shim shape: build dir 5 deep under the work root,
    // an argument pointing at the work root's src tree.
    #[test]
    fn deep_ancestor_becomes_up_chain() {
        let bd = Path::new("/work/qtwe/build/src/core/Release/x86_64");
        let cmd = "python3 gen.py --headers-root /work/qtwe/src/3p/icu/unicode --out gen";
        assert_eq!(
            rewrite_ancestor_paths(cmd, bd),
            "python3 gen.py --headers-root ../../../../../src/3p/icu/unicode --out gen"
        );
    }

    #[test]
    fn build_dir_itself_strips_to_relative() {
        let bd = Path::new("/work/qtwe/build/out");
        assert_eq!(
            rewrite_ancestor_paths("cp /work/qtwe/build/out/a.h b.h", bd),
            "cp a.h b.h"
        );
    }

    #[test]
    fn store_and_system_paths_untouched() {
        let bd = Path::new("/work/qtwe/build/out");
        let cmd = "/nix/store/abc-python/bin/python3 /bin/sh /home/x";
        assert_eq!(rewrite_ancestor_paths(cmd, bd), cmd);
    }
}

#[cfg(test)]
mod normalize_output_tests {
    use super::normalize_output;

    #[test]
    fn slash_still_maps_to_dash() {
        assert_eq!(normalize_output("src/main.o"), "src-main.o");
    }

    #[test]
    fn at_sign_is_sanitized() {
        // The three shapes measured in the wild: mac asset suffixes,
        // npm scope directories, gettext locale variants.
        assert_eq!(normalize_output("icon@2x.png"), "icon-2x.png");
        assert_eq!(
            normalize_output("node_modules/@babel/core/lib/index.js"),
            "node_modules--babel-core-lib-index.js"
        );
        assert_eq!(normalize_output("sr@latin.po"), "sr-latin.po");
    }

    #[test]
    fn allowed_charset_passes_through() {
        assert_eq!(
            normalize_output("libfoo+bar_1.2-r3?x=y.so"),
            "libfoo+bar_1.2-r3?x=y.so"
        );
    }

    #[test]
    fn leading_period_is_prefixed() {
        assert_eq!(normalize_output(".hidden.c"), "-.hidden.c");
    }

    #[test]
    fn empty_falls_back() {
        assert_eq!(normalize_output(""), "source");
    }

    #[test]
    fn spaces_and_unicode_are_sanitized() {
        assert_eq!(normalize_output("a b\u{e9}.c"), "a-b-.c");
    }

    #[test]
    fn grd_scaled_image_resolves_through_context_dir() {
        use super::grd_references;
        use std::fs;
        let dir = std::env::temp_dir().join(format!("grd-test-{}", std::process::id()));
        let scaled = dir.join("default_100_percent/flags_ui");
        fs::create_dir_all(&scaled).unwrap();
        fs::write(scaled.join("favicon.png"), b"png").unwrap();
        fs::write(dir.join("direct.xtb"), b"xtb").unwrap();
        let grd = dir.join("res.grd");
        fs::write(
            &grd,
            r#"<grit><outputs><output context="default_100_percent"/></outputs>
               <file path="direct.xtb"/>
               <structure type="chrome_scaled_image" file="flags_ui/favicon.png"/>
               <structure type="chrome_scaled_image" file="flags_ui/absent.png"/></grit>"#,
        )
        .unwrap();
        let refs = grd_references(&grd).unwrap();
        assert!(refs.contains(&dir.join("direct.xtb")));
        assert!(refs.contains(&scaled.join("favicon.png")));
        assert_eq!(refs.len(), 2, "absent file must not be invented: {refs:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}

/// Discovers C include dependencies from a command line and input files.
/// Returns (discovered_deps, discovered_store_paths) where:
/// - discovered_deps: DerivedFiles that need to be encoded and added to NIX_NINJA_INPUTS
/// - discovered_store_paths: Store paths that only need to be added as input sources
pub fn discover_c_includes(
    rpc_client: &Arc<BuilderRpcClient>,
    store_dir: &StoreDir,
    build_dir: &Path,
    cmdline: &str,
    files: Vec<PathBuf>,
    virtual_paths: Option<HashMap<PathBuf, PathBuf>>,
) -> Result<(Vec<DerivedFile>, Vec<StorePath>)> {
    let c_includes = c_include_parser::retrieve_c_includes(cmdline, files.clone(), virtual_paths)?;
    let mut discovered_deps = Vec::new();
    let mut discovered_store_paths = Vec::new();

    // Convert input files to a set for filtering
    let input_files: HashSet<PathBuf> = files.into_iter().collect();

    for include in c_includes {
        // Skip input files - we only want to discover new dependencies
        if input_files.contains(&include) {
            continue;
        }

        // Check if include is from Nix store or a regular file
        if let Ok(relative) = include.strip_prefix(AsRef::<Path>::as_ref(store_dir)) {
            if let Some(hash_path) = relative.components().next().map(|c| c.as_os_str()) {
                let full_path = AsRef::<Path>::as_ref(store_dir).join(hash_path);
                let store_path: StorePath =
                    store_dir.parse(full_path.to_str().context("Path was not valid UTF-8")?)?;
                discovered_store_paths.push(store_path);
                continue;
            }
        }

        // Regular file, add to nix store and treat as derived dependency
        let derived_file = new_opaque_file(rpc_client, build_dir, include)?;
        discovered_deps.push(derived_file);
    }

    Ok((discovered_deps, discovered_store_paths))
}

/// Removes -frandom-seed flag from a string of CFLAGS.
fn remove_frandom_seed(flags: &str) -> String {
    flags
        .split_whitespace()
        .filter(|flag| !flag.starts_with("-frandom-seed="))
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Generates -frandom-seed based on the task's cmdline.
fn generate_frandom_seed(cmdline: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cmdline.as_bytes());
    let result = hasher.finalize();
    format!("{result:x}")[..16].to_string()
}
