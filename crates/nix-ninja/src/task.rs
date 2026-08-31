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
    sync::{mpsc, Arc},
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
}

/// Runner is an async runtime that spawns threads for each task.
pub struct Runner {
    pub derived_files: HashMap<FileId, DerivedFile>,
    build_dir_inputs: HashMap<FileId, DerivedFile>,

    tx: mpsc::Sender<BuildResult>,
    rx: mpsc::Receiver<BuildResult>,
    tools: Tools,
    rpc_client: Arc<BuilderRpcClient>,
    config: RunnerConfig,
    wrapper_vars: HashMap<String, String>,
    wrapper_store_paths: Vec<StorePath>,
    store_regex: Regex,
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
            // SAME CLASS, OTHER VARIABLE. The outer derivation's
            // cc-wrapper puts `-rpath $out/lib` into NIX_LDFLAGS, and
            // $out is the OUTER output path: it moves with every change
            // to the outer derivation, a source edit included, so every
            // task re-keyed on it and a one-line edit rebuilt all 105 of
            // qtsvg's translation units (measured 2026-08-23). Nothing
            // inside a task needs it: a compile never links, and the
            // install step rewrites RPATH off the build tree anyway.
            if key.starts_with("NIX_LDFLAGS") {
                *value = remove_outer_rpath(value);
            }
        }

        // Outer output paths -> placeholders in the forwarded env too.
        let rewrite = outer_rewrite_map();
        for value in wrapper_vars.values_mut() {
            *value = rewrite_str(value, &rewrite);
        }

        // Extract store paths from wrapper variables once
        let mut wrapper_store_paths = Vec::new();
        for value in wrapper_vars.values() {
            let found_store_paths = extract_store_paths(&config.store_dir, &store_regex, value)?;
            wrapper_store_paths.extend(found_store_paths);
        }

        let (tx, rx) = mpsc::channel();
        Ok(Runner {
            derived_files: HashMap::new(),
            build_dir_inputs: HashMap::new(),
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

        let tools = self.tools.clone();
        let task = self.new_task(files, build)?;

        let config = self.config.clone();
        let rpc_client = self.rpc_client.clone();
        std::thread::spawn(move || {
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
                        Err(err) => (None, Some(err.context(format!("Failed to handle derivation result for task (derivation: {})\nDerivation JSON:\n{}", drv.name, serde_json::to_string_pretty(&drv).unwrap_or_else(|_| "Failed to serialize derivation".to_string()))))),
                    },
                    Err(err) => (None, Some(err.context("Failed to build task derivation for task".to_string()))),
                };

            // Create DerivedFiles for all outputs if successful
            let derived_files = if let Some(ref final_derived_path) = derived_path {
                let mut drv_outputs: Vec<DerivedFile> = Vec::new();
                for fid in task.outs() {
                    let file = &task.files[fid];
                    let built_file =
                        new_built_file(final_derived_path.clone(), file.name.clone().into());
                    drv_outputs.push(built_file);
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

    pub fn wait(&mut self, files: &mut graph::GraphFiles) -> Result<BuildId> {
        let result = self.rx.recv().unwrap();
        if let Some(err) = result.err {
            eprintln!("Error: {err}");

            eprintln!("Caused by:");
            for cause in err.chain().skip(1) {
                eprintln!("    {cause}");
            }

            eprintln!("Backtrace: {}", err.backtrace());

            let debug_info = if let Some(derived_path) = &result.derived_path {
                format!(
                    "derivation: {}",
                    self.config.store_dir.display(derived_path)
                )
            } else {
                format!("build_id: {:?}", result.bid)
            };

            return Err(anyhow!(
                "Failed to build task derivation for {}: {}",
                debug_info,
                err
            ));
        }

        for derived_file in result.derived_files {
            self.add_derived_file(files, derived_file.clone());
        }

        Ok(result.bid)
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
        for fid in build.ordering_ins() {
            // TODO: what about phony inputs?
            let input = match self.derived_files.get(fid) {
                Some(df) => df.to_owned(),
                None => {
                    let file = &files.by_id[*fid];
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

                    let input = new_opaque_file(
                        &self.rpc_client,
                        &self.config.build_dir,
                        file.name.clone().into(),
                    )?;
                    self.add_derived_file(files, input.clone().to_owned());
                    input.to_owned()
                }
            };
            input_set.insert(input.build_path.clone(), input.clone());
        }

        let mut outputs: Vec<PathBuf> = Vec::new();
        for fid in build.outs() {
            let file = &files.by_id[*fid];
            outputs.push(PathBuf::from(&file.name));
        }

        // Meson resolves `files(...)` in custom_target commands to absolute
        // paths at configure time. Those paths do not exist inside the task
        // sandbox, where inputs are symlinked at their build-dir-relative
        // locations, so rewrite in-tree absolute paths to relative ones
        // (mirroring `relative_from` in `new_opaque_file`).
        let cmdline = build.cmdline.as_ref().map(|cmdline| {
            let mut cmdline = cmdline.clone();
            let build_dir = &self.config.build_dir;
            if let Some(dir) = build_dir.to_str() {
                cmdline = cmdline.replace(&format!("{dir}/"), "");
            }
            if let Some(dir) = build_dir.parent().and_then(|p| p.to_str()) {
                cmdline = cmdline.replace(&format!("{dir}/"), "../");
            }
            cmdline
        });

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
                    continue;
                };
                let input = match self.derived_files.get(&fid) {
                    Some(derived_file) => derived_file,
                    None => match self.build_dir_inputs.get(&fid) {
                        Some(derived_file) => derived_file,
                        None => {
                            continue;
                        }
                    },
                };
                input_set.insert(input.build_path.clone(), input.clone());
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
        for input in self.build_dir_inputs.values() {
            input_set.insert(input.build_path.clone(), input.clone());
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
    // Outer output paths -> placeholders (see outer_rewrite_map), EXCEPT
    // for an LTO compile, which keeps the real paths everywhere: see
    // task_is_lto for the measured reason.
    let lto_raw = task.deps.as_deref() == Some("gcc") && task_is_lto(cmdline, &task.wrapper_vars);
    let cmdline = &if lto_raw {
        cmdline.to_string()
    } else {
        rewrite_str(cmdline, &outer_rewrite_map())
    };
    drv.args.push(cmdline.to_string().into_bytes().into());

    if let Some(desc) = &task.desc {
        drv.args
            .push(format!("--description={desc}").into_bytes().into());
    }

    // Propagate wrapper environment variables to the task. They were
    // placeholdered at task creation; an LTO task gets them restored,
    // the exact inverse map, because its output cannot be restored.
    let env_restore = if lto_raw {
        outer_restore_map()
    } else {
        Vec::new()
    };
    for (key, value) in &task.wrapper_vars {
        let value = &if lto_raw {
            rewrite_str(value, &env_restore)
        } else {
            value.clone()
        };
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

    // The input half of keeping an LTO task on real outer paths: any
    // input this task reads that was uploaded with the placeholder
    // substituted is re-uploaded raw and swapped in.
    if lto_raw {
        let rewritten = rewritten_uploads().lock().unwrap().clone();
        if !rewritten.is_empty() {
            let mut swapped = 0usize;
            for i in task.inputs.iter().chain(discovered_inputs.iter()) {
                if !matches!(i.derived_path, SingleDerivedPath::Opaque(_)) {
                    continue;
                }
                let abs = task.build_dir.join(&i.build_path);
                let Ok(canonical) = fs::canonicalize(&abs) else {
                    continue;
                };
                if !rewritten.contains(&canonical) {
                    continue;
                }
                let fresh = new_opaque_file_raw(rpc_client, &task.build_dir, abs)?;
                input_set.remove(&i.to_encoded(&task.store_dir));
                drv.inputs.remove(&i.derived_path);
                input_set.insert(fresh.to_encoded(&task.store_dir));
                drv.inputs.insert(fresh.derived_path.clone());
                swapped += 1;
            }
            if swapped > 0 {
                eprintln!(
                    "nix-ninja: LTO task keeps real outer paths in {swapped} input(s) (resumable only while $out is unchanged)"
                );
            }
        }
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

    {
        // Prepare $PATH to have coreutils.
        let mut path: Vec<String> = vec![
            format!("{}/bin", task.store_dir.display(&tools.cc)),
            format!("{}/bin", task.store_dir.display(&tools.coreutils)),
            format!("{}/bin", task.store_dir.display(&tools.patchelf)),
        ];

        let cmdline_binary = cmdline
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow!("No command found in cmdline"))?;

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
    let name = canonical_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".to_string());
    // A file that names an outer output path is uploaded with the path
    // rewritten to its placeholder (a config.h carrying $out/share/alsa),
    // so the upload's hash, and every task reading it, is stable across
    // changes to the outer derivation. Regular files only; a directory is
    // uploaded as it is.
    let upload_src = if canonical_path.is_file() {
        let map = outer_rewrite_map();
        let data = fs::read(&canonical_path)?;
        match rewrite_bytes(&data, &map) {
            Some(rewritten) => {
                // UNIQUE PER CALL, not per pid: batched adds run this
                // concurrently, and two uploads of one file racing a
                // pid-keyed tmp name hand the daemon a truncated stream.
                let tmp = std::env::temp_dir().join(format!(
                    "nn-outer-{}-{}",
                    std::process::id(),
                    NEXT_REWRITE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                ));
                fs::write(&tmp, rewritten)?;
                if let Ok(md) = fs::metadata(&canonical_path) {
                    let _ = fs::set_permissions(&tmp, md.permissions());
                }
                rewritten_uploads()
                    .lock()
                    .unwrap()
                    .insert(canonical_path.clone());
                Some(tmp)
            }
            None => None,
        }
    } else {
        None
    };

    let store_path =
        rpc_client.add_to_store_nar(&name, upload_src.as_deref().unwrap_or(&canonical_path))?;
    if let Some(tmp) = &upload_src {
        let _ = fs::remove_file(tmp);
    }

    Ok(DerivedFile {
        derived_path: SingleDerivedPath::Opaque(store_path),
        build_path: relative_path,
        rel_path: None, // None for opaque files - store path points directly to file
    })
}

static NEXT_REWRITE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
// store path.
fn normalize_output(output: &str) -> String {
    output.replace('/', "-")
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

/// THE OUTER OUTPUT PATHS ARE REWRITTEN TO SAME-LENGTH PLACEHOLDERS IN
/// EVERYTHING A TASK SEES, AND BACK IN EVERYTHING IT PRODUCES.
///
/// `$out` of the outer derivation moves with every change to that
/// derivation, a one-line source edit included, and configure writes it
/// into generated headers (`ALSA_CONFIG_DIR` in alsa-lib's config.h, which
/// every TU includes) and into command lines. Measured 2026-08-23: one
/// edited .c file rebuilt 110 of 116 translation units. This is the same
/// problem Nix solves for content-addressed outputs the same way: a
/// placeholder of identical length stands in during the build, and the
/// real path is substituted into the result afterwards. Same length
/// because the substitution has to be valid inside an object file.
///
/// The task derivation and its output carry ONLY the placeholder, which is
/// what makes the derivation stable and its cached output reusable under
/// a different `$out`; the driver alone holds the mapping and applies it
/// when it materializes an output into the build tree (`local.rs`).
pub fn outer_rewrite_map() -> Vec<(String, String)> {
    let store_dir = std::env::var("NIX_STORE").unwrap_or_else(|_| "/nix/store".into());
    std::env::var("outputs")
        .unwrap_or_else(|_| "out".into())
        .split_whitespace()
        .filter_map(|name| std::env::var(name).ok().map(|p| (name.to_string(), p)))
        .filter_map(|(name, real)| {
            // /nix/store/<32 base32 chars>-<rest>
            let rel = real.strip_prefix(&format!("{store_dir}/"))?;
            let (hash, rest) = rel.split_once('-')?;
            if hash.len() != 32 {
                return None;
            }
            let digest = Sha256::digest(format!("nix-ninja-outer-output:{name}").as_bytes());
            let alphabet = b"0123456789abcdfghijklmnpqrsvwxyz";
            let fake: String = digest
                .iter()
                .take(32)
                .map(|b| alphabet[(*b as usize) % 32] as char)
                .collect();
            Some((real.clone(), format!("{store_dir}/{fake}-{rest}")))
        })
        .collect()
}

/// Apply `map` (from -> to) to `s`; a no-op where nothing matches.
pub fn rewrite_str(s: &str, map: &[(String, String)]) -> String {
    let mut out = s.to_string();
    for (from, to) in map {
        if out.contains(from.as_str()) {
            out = out.replace(from.as_str(), to);
        }
    }
    out
}

/// Byte-wise form for file contents, which may be binary.
pub fn rewrite_bytes(data: &[u8], map: &[(String, String)]) -> Option<Vec<u8>> {
    let mut out = data.to_vec();
    let mut changed = false;
    for (from, to) in map {
        let (f, t) = (from.as_bytes(), to.as_bytes());
        debug_assert_eq!(f.len(), t.len());
        let mut i = 0;
        while i + f.len() <= out.len() {
            if &out[i..i + f.len()] == f {
                out[i..i + f.len()].copy_from_slice(t);
                changed = true;
                i += f.len();
            } else {
                i += 1;
            }
        }
    }
    changed.then_some(out)
}

/// The reverse map: placeholder -> real.
pub fn outer_restore_map() -> Vec<(String, String)> {
    outer_rewrite_map()
        .into_iter()
        .map(|(r, p)| (p, r))
        .collect()
}

/// The outer derivation's output paths, from the `outputs` env var the
/// builder is given (`$out`, `$dev`, ...). Empty outside a build.
fn outer_output_paths() -> Vec<String> {
    std::env::var("outputs")
        .unwrap_or_else(|_| "out".into())
        .split_whitespace()
        .filter_map(|o| std::env::var(o).ok())
        .collect()
}

/// Removes -frandom-seed flag from a string of CFLAGS.
/// Drop every `-rpath <path>` pair whose path names one of the OUTER
/// derivation's outputs ($out, $dev, ... as the `outputs` env lists them).
/// Other rpath entries (store libraries) are inputs and stay.
fn remove_outer_rpath(value: &str) -> String {
    let outer = outer_output_paths();
    let toks: Vec<&str> = value.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        if toks[i] == "-rpath"
            && i + 1 < toks.len()
            && outer.iter().any(|o| toks[i + 1].starts_with(o.as_str()))
        {
            i += 2;
            continue;
        }
        out.push(toks[i]);
        i += 1;
    }
    out.join(" ")
}

/// Last-wins scan of `-flto*` / `-fno-lto` over one flag string, from a
/// starting state.
fn scan_lto_flags(flags: &str, start: bool) -> bool {
    let mut lto = start;
    for tok in flags.split_whitespace() {
        if tok == "-fno-lto" {
            lto = false;
        } else if tok == "-flto" || tok.starts_with("-flto=") {
            lto = true;
        }
    }
    lto
}

/// The compiler's view, not the command line's. A stdenv can inject
/// -flto from INSIDE its cc wrapper (nixpkgs `cc-cflags-before`), where
/// neither the ninja edge nor the task env shows it, so a task whose
/// command reads `gcc -O2 -c` is LTO all the same. The wrapper's order
/// decides precedence and is reproduced here: the wrapper's own
/// baseline first (NIX_NINJA_ASSUME_LTO=1, set by a stdenv that injects
/// -flto globally), then the command line, then NIX_CFLAGS_COMPILE,
/// which the wrapper appends AFTER the command line - so a package's
/// -fno-lto opt-out delivered that way wins, exactly as it does for gcc.
fn task_is_lto(cmdline: &str, wrapper_vars: &HashMap<String, String>) -> bool {
    let assume = std::env::var("NIX_NINJA_ASSUME_LTO")
        .map(|v| v == "1")
        .unwrap_or(false);
    let after_cmdline = scan_lto_flags(cmdline, assume);
    match wrapper_vars.get("NIX_CFLAGS_COMPILE") {
        Some(extra) => scan_lto_flags(extra, after_cmdline),
        None => after_cmdline,
    }
}

/// Does this compile emit LTO bytecode? The last of `-flto*` / `-fno-lto`
/// on the line wins, as it does for gcc. `-ffat-lto-objects` changes
/// nothing here: the linker still consumes the IR half.
///
/// Why it matters. The outer-output
/// placeholder is restored in task OUTPUTS by an equal-length byte
/// rewrite (rewrite_bytes, local.rs). That is sound for a plain object,
/// whose literals sit verbatim in .rodata, and UNSOUND for LTO
/// bytecode: gcc streams the IR through a compressed, checksummed
/// section, so the placeholder is invisible to the byte search at the
/// default level and the object is corrupted by the rewrite at
/// -flto-compression-level=0 ("compressed stream: data error" from
/// lto1, measured 2026-08-24). Nothing downstream can repair it either:
/// the link that consumes the IR embeds the placeholder into the final
/// binary, where it names a path that does not exist. bison under the
/// make drop-in shipped PKGDATADIR as the placeholder and failed 698 of
/// 744 tests, twice, the first time misattributed to a materialization
/// window (the local.rs comment); the window was real and was not the
/// cause. So an LTO compile task never sees a placeholder at all: real
/// paths on its command line and in its environment, and a raw upload
/// of every input the global rewrite touched. Such a task re-keys when
/// the outer output path moves, which is the honest cost - only tasks
/// that actually read an outer path pay it, and a wrong artifact is not
/// a cache hit.
/// Canonical paths whose upload carried an outer output path and was
/// therefore uploaded with the placeholder substituted. An LTO task must
/// not read those: its inputs must carry the real outer paths, because the
/// placeholder would be baked into checksummed LTO bytecode.
fn rewritten_uploads() -> &'static std::sync::Mutex<HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// The RAW upload of a build-dir file: no outer-output placeholder, and
/// past the NAR stamp cache, which is keyed by canonical path and would
/// otherwise hand back the placeholder-carrying object for the same
/// path. Content-addressed on the daemon side, so a repeat costs one
/// dedup round trip.
fn new_opaque_file_raw(
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
    let name = canonical_path
        .file_name()
        .map(|n| normalize_output(&n.to_string_lossy()))
        .unwrap_or_else(|| "source".to_string());
    let store_path = rpc_client.add_to_store_nar(&name, &canonical_path)?;
    Ok(DerivedFile {
        derived_path: SingleDerivedPath::Opaque(store_path),
        build_path: relative_path,
        rel_path: None,
    })
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

#[cfg(test)]
mod outer_placeholder_tests {
    use std::collections::HashMap;

    /// Both tests below set `outputs`/`out`/`dev`, which are PROCESS-wide:
    /// cargo runs tests in threads, so without this lock one test reads the
    /// other's assertions. Poisoning is survivable: a panicking holder must
    /// not fail the other test twice.
    static OUT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `scan_lto_flags` with no wrapper baseline, which is the state
    /// `task_is_lto` starts from when the environment sets none. Written as
    /// a helper so these read as the command-line question they ask.
    fn scan_lto_flags_no_baseline(cmdline: &str) -> bool {
        super::scan_lto_flags(cmdline, false)
    }

    #[test]
    fn last_lto_flag_wins_and_fat_changes_nothing() {
        assert!(scan_lto_flags_no_baseline("gcc -O3 -flto=8 -c a.c -o a.o"));
        assert!(scan_lto_flags_no_baseline("gcc -flto -c a.c"));
        assert!(scan_lto_flags_no_baseline(
            "gcc -flto=auto -ffat-lto-objects -c a.c"
        ));
        assert!(
            !scan_lto_flags_no_baseline("gcc -flto=8 -fno-lto -c a.c"),
            "later -fno-lto wins"
        );
        assert!(
            scan_lto_flags_no_baseline("gcc -fno-lto -flto -c a.c"),
            "later -flto wins"
        );
        assert!(!scan_lto_flags_no_baseline("gcc -O3 -c a.c -o a.o"));
        // the placeholder-restore hazard is about IR, not the word: a
        // path containing 'flto' is not a flag
        assert!(!scan_lto_flags_no_baseline("gcc -c /src/flto/a.c"));
    }

    #[test]
    fn task_is_lto_sees_the_wrapper_baseline_and_the_opt_out_after_it() {
        // Never read the real environment here; the baseline is
        // exercised through scan_lto_flags's start state, which is what
        // task_is_lto folds NIX_NINJA_ASSUME_LTO into.
        let mut vars: std::collections::HashMap<String, String> = Default::default();
        // no baseline, nothing on the line: not LTO
        assert!(!super::scan_lto_flags("gcc -O2 -c a.c", false));
        // wrapper baseline with a bare command line (the bison shape): LTO
        assert!(super::scan_lto_flags("gcc -O2 -c a.c", true));
        // a package's -fno-lto opt-out arrives via NIX_CFLAGS_COMPILE and
        // wins over the baseline, as the wrapper appends it last
        vars.insert("NIX_CFLAGS_COMPILE".into(), "-O3 -fno-lto".into());
        assert!(!super::scan_lto_flags(
            vars["NIX_CFLAGS_COMPILE"].as_str(),
            super::scan_lto_flags("gcc -O2 -c a.c", true)
        ));
        // and the env-driven path with no baseline set in this process
        std::env::remove_var("NIX_NINJA_ASSUME_LTO");
        assert!(!super::task_is_lto("gcc -O2 -c a.c", &vars));
        vars.insert("NIX_CFLAGS_COMPILE".into(), "-O3 -flto=8".into());
        assert!(super::task_is_lto("gcc -O2 -c a.c", &vars));
        // THE WRAPPER BASELINE ITSELF. Only "1" turns it on: a stdenv
        // exporting NIX_NINJA_ASSUME_LTO=0 must not be read as yes, and
        // nothing else covered the comparison.
        let empty: HashMap<String, String> = HashMap::new();
        std::env::set_var("NIX_NINJA_ASSUME_LTO", "1");
        assert!(super::task_is_lto("gcc -O2 -c a.c", &empty));
        std::env::set_var("NIX_NINJA_ASSUME_LTO", "0");
        assert!(!super::task_is_lto("gcc -O2 -c a.c", &empty));
        std::env::remove_var("NIX_NINJA_ASSUME_LTO");
    }

    #[test]
    fn outer_rewrite_map_is_same_length_stable_and_reversible() {
        let _env = OUT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("outputs", "out dev");
        std::env::set_var(
            "out",
            "/nix/store/29byqlv4flilwli8hc23rm9v1cpn32pl-alsa-lib-1.2.16.1",
        );
        std::env::set_var(
            "dev",
            "/nix/store/npbwm562dyvwjim6j7qa1cish2vxlqr0-alsa-lib-1.2.16.1-dev",
        );
        let m = super::outer_rewrite_map();
        assert_eq!(m.len(), 2);
        for (real, fake) in &m {
            assert_eq!(real.len(), fake.len());
            assert_ne!(real, fake);
            assert!(fake.starts_with("/nix/store/"));
        }
        // Round trip FIRST, while $out still holds the path `m` was built
        // from - the stability check below moves it, and the restore map
        // reads the environment fresh.
        let data = format!("#define DIR \"{}/share\"\0bin", m[0].0);
        let fwd = super::rewrite_bytes(data.as_bytes(), &m).expect("rewritten");
        assert!(!fwd.windows(m[0].0.len()).any(|w| w == m[0].0.as_bytes()));
        // Through outer_restore_map, not a hand-built inverse: the
        // hand-built one passed with outer_restore_map returning an empty
        // vec. Mutation, 2026-08-30.
        let back = super::outer_restore_map();
        assert_eq!(back.len(), m.len());
        assert!(back
            .iter()
            .all(|(p, r)| m.contains(&(r.clone(), p.clone()))));
        assert_eq!(super::rewrite_bytes(&fwd, &back).unwrap(), data.as_bytes());
        // A LEADING MATCH, so the scan's advance is exercised from offset 0
        // as well as from the middle.
        let lead = format!("{}/lib", m[0].0);
        assert!(super::rewrite_bytes(lead.as_bytes(), &m).is_some());
        assert!(super::rewrite_bytes(b"nothing here", &m).is_none());
        // Stable under a different $out hash: the placeholder depends on the
        // output NAME only.
        std::env::set_var(
            "out",
            "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-alsa-lib-1.2.16.1",
        );
        assert_eq!(super::outer_rewrite_map()[0].1, m[0].1);
    }

    #[test]
    fn remove_outer_rpath_strips_the_out_lib_pair() {
        let _env = OUT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("outputs", "out dev");
        std::env::set_var(
            "out",
            "/nix/store/29byqlv4flilwli8hc23rm9v1cpn32pl-alsa-lib-cc-dropin-1.2.16.1",
        );
        std::env::set_var(
            "dev",
            "/nix/store/npbwm562dyvwjim6j7qa1cish2vxlqr0-alsa-lib-cc-dropin-1.2.16.1-dev",
        );
        let v = "-rpath /nix/store/29byqlv4flilwli8hc23rm9v1cpn32pl-alsa-lib-cc-dropin-1.2.16.1/lib  -L/nix/store/klkb81wkzlz3bpfv6brnh3gwcapy5b4w-boost-1.89.0/lib";
        assert_eq!(
            super::remove_outer_rpath(v),
            "-L/nix/store/klkb81wkzlz3bpfv6brnh3gwcapy5b4w-boost-1.89.0/lib"
        );
        // AN RPATH THAT IS NOT AN OUTER OUTPUT MUST SURVIVE. Without this
        // the whole test passes when outer_output_paths returns [""], on
        // which starts_with is true for every path and every rpath is
        // stripped - a task then links against nothing. Found by mutation
        // 2026-08-30.
        let keep = "-rpath /nix/store/klkb81wkzlz3bpfv6brnh3gwcapy5b4w-boost-1.89.0/lib -lfoo";
        assert_eq!(super::remove_outer_rpath(keep), keep);
        // A TRAILING BARE `-rpath` HAS NO PAIR. The bound is what stops
        // the peek running off the end; drop it and the flag is consumed
        // as if it had a value.
        assert_eq!(super::remove_outer_rpath("-lfoo -rpath"), "-lfoo -rpath");
    }

    /// `rewrite_str` had no direct test: replacing its whole body with
    /// `String::new()` survived the suite, because every caller of it in
    /// the tests went through `rewrite_bytes` instead. Mutation, 2026-08-30.
    #[test]
    fn rewrite_str_substitutes_and_leaves_non_matches_alone() {
        let map = vec![(
            "/nix/store/aaa-x".to_string(),
            "/nix/store/bbb-x".to_string(),
        )];
        assert_eq!(
            super::rewrite_str("-L/nix/store/aaa-x/lib -lfoo", &map),
            "-L/nix/store/bbb-x/lib -lfoo"
        );
        assert_eq!(super::rewrite_str("nothing here", &map), "nothing here");
        assert_eq!(super::rewrite_str("anything", &[]), "anything");
    }
}
