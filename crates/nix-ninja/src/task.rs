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
use std::sync::{LazyLock, Mutex};
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
    /// The edge's `depfile`, carried so the task can declare it as an output
    /// and the run can collect it. Upstream #17.
    depfile: Option<String>,

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
                        Ok(final_derived_path) => {
                            // UPSTREAM #17, the collection half. Only in
                            // local mode: inside a derivation nothing drains
                            // this, so every push would cost a name
                            // normalisation per task for a value no one
                            // reads.
                            if let Some(dpath) = accepted_depfile_output(&task)
                                .filter(|_| !config.is_output_derivation)
                            {
                                COLLECTED_DEPFILES.lock().unwrap().push(new_built_file(
                                    final_derived_path.clone(),
                                    dpath,
                                ));
                            }
                            (Some(final_derived_path), None)
                        }
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
            depfile: build.depfile.clone(),
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

    // Sort NIX_NINJA_INPUTS to ensure determinism.
    let mut inputs: Vec<String> = input_set.into_iter().collect();
    inputs.sort();

    drv.env.insert(
        b"NIX_NINJA_INPUTS"[..].into(),
        inputs.join(" ").into_bytes().into(),
    );

    // UPSTREAM #17: declare the edge's depfile as an output too, so the
    // build writes it somewhere the run can collect it from. Only when the
    // command actually generates one - see `accepted_depfile_output`.
    let mut task_outputs = task.outputs.clone();
    if let Some(dpath) = accepted_depfile_output(&task) {
        task_outputs.push(dpath);
    }

    // Add all ninja build outputs.
    let mut outputs: Vec<String> = Vec::new();
    for output_path in &task_outputs {
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
    let store_path = rpc_client.add_to_store_nar(&name, &canonical_path)?;

    Ok(DerivedFile {
        derived_path: SingleDerivedPath::Opaque(store_path),
        build_path: relative_path,
        rel_path: None, // None for opaque files - store path points directly to file
    })
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

///
/// Takes the four fields rather than a `&Task`, for the reason
/// `is_compile_edge` does: a Task needs a whole ninja graph to construct,
/// and a gate nobody can unit-test is a gate that drifts.
fn accepted_depfile_output(task: &Task) -> Option<PathBuf> {
    accepted_depfile_output_of(
        task.depfile.as_deref(),
        task.deps.as_deref(),
        task.cmdline.as_deref(),
        // The rspfile spelling is deliberately NOT read here. An edge that
        // hides its `-MD` inside an rspfile is refused, which costs a scan
        // rather than producing a wrong answer, and reading it would pull in
        // plumbing this change does not otherwise need.
        None,
    )
}

fn accepted_depfile_output_of(
    depfile: Option<&str>,
    deps: Option<&str>,
    cmdline: Option<&str>,
    rspfile_content: Option<&str>,
) -> Option<PathBuf> {
    let (Some(d), Some("gcc")) = (depfile, deps) else {
        return None;
    };
    if d.is_empty() || !command_writes_depfile(cmdline, rspfile_content) {
        return None;
    }
    let p = PathBuf::from(d);
    // An absolute or escaping depfile path is not ours to copy: the task's
    // outputs are build-dir-relative by construction, and a path outside
    // that tree would be silently rebased. Skip it and leave the task
    // exactly as it was.
    if p.is_absolute() || p.starts_with("..") {
        None
    } else {
        Some(p)
    }
}

/// UPSTREAM #17, THE COLLECTION HALF. Every depfile a task emitted, as a
/// derived path that can be realized after the run.
///
/// The read-back half already existed and could never fire on a fresh
/// build directory, which is why this looked finished and was not: the
/// depfile is a content-addressed OUTPUT of the task derivation, and local
/// mode materializes only the requested TARGETS (`cli.rs`), so no
/// per-object depfile ever reached the build directory for a later run to
/// read. Collect them here, materialize them in local mode, and
/// `depfile_read_back` does the skipping it was always written to do.
static COLLECTED_DEPFILES: LazyLock<Mutex<Vec<DerivedFile>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Drains the collected depfile outputs. Drained rather than cloned: the
/// caller materializes them once at the end of a run, and leaving them
/// behind would make a second call in the same process re-realize paths it
/// already wrote.
pub fn take_collected_depfiles() -> Vec<DerivedFile> {
    std::mem::take(&mut *COLLECTED_DEPFILES.lock().unwrap())
}

/// Does this command actually WRITE a dependency file?
///
/// CMake's LTO capability probe generates an edge declaring `deps = gcc` and
/// a depfile while its command carries no generation flag, so gcc never
/// writes the file and a task that declared it as an output dies collecting
/// it. The edge declaration is a promise about the RULE; the command is the
/// truth, and the rspfile is part of that command line.
///
/// `-MF` DOES NOT ANSWER THAT, and treating it as though it did cost a real
/// package. `-MF` names where a depfile would go; `-MQ` and `-MT` set the
/// target inside it. Only a dependency-GENERATION flag makes one appear.
///
/// gcc does not merely skip the depfile in that case, it REFUSES the compile:
/// `cc1: error: to generate dependencies you must specify either '-M' or
/// '-MM'`. So no gcc command line reaching this function can have a bare
/// `-MF` and still be a command that runs.
///
/// nasm is the case that reached a build. meson's nasm rule is `deps = gcc`
/// with `-MQ ... -MF ...` and no generation flag, and nasm accepts that,
/// exits 0, and writes nothing. #17 then declared the depfile a
/// content-addressed output and the task failed collecting an output the
/// command never produces: `canonicalize(...cpuid.obj.ndep): No such file or
/// directory`. Measured 2026-08-30 driving libvmaf, the package that named
/// failure class 3. This is the exact hazard the gate exists to prevent, and
/// it was in the gate.
fn command_writes_depfile(cmdline: Option<&str>, rsp: Option<&str>) -> bool {
    let writes = |s: &str| {
        s.split_whitespace().any(|t| {
            // Generation flags. `-M`/`-MM` write to stdout unless redirected
            // by `-MF`, and both are dependency modes, so both count.
            t == "-M"
                || t == "-MM"
                || t == "-MD"
                || t == "-MMD"
                || t == "-MG"
                // The LINKER spelling: CMake 3.27+ link edges write their
                // depfile via `-Wl,--dependency-file=...` (capstone's LTO
                // link died writing link.d into a directory only the
                // declared-output path used to create).
                || t.contains("--dependency-file")
        })
    };
    cmdline.is_some_and(writes) || rsp.is_some_and(writes)
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
mod accepted_depfile_output_tests {
    use super::accepted_depfile_output_of;
    use std::path::PathBuf;

    const CC: &str = "cc -MD -MF foo.o.d -c foo.c -o foo.o";

    #[test]
    fn a_gcc_deps_edge_with_a_relative_depfile_is_accepted() {
        assert_eq!(
            accepted_depfile_output_of(Some("foo.o.d"), Some("gcc"), Some(CC), None),
            Some(PathBuf::from("foo.o.d"))
        );
    }

    /// THE GATE THAT COSTS A BUILD IF IT WIDENS. A declared output the
    /// command does not produce fails the task, and `depfile` without
    /// `deps = gcc` is a path ninja would read IF one appeared - not a
    /// promise that anything writes it. meson's nasm rule is the real
    /// case: `depfile =` and no `deps`.
    #[test]
    fn a_depfile_without_deps_gcc_is_refused() {
        assert_eq!(
            accepted_depfile_output_of(Some("foo.o.d"), None, Some(CC), None),
            None
        );
    }

    /// And a command that declares no depfile flag writes nothing, however
    /// the rule is spelled.
    #[test]
    fn a_command_that_writes_no_depfile_is_refused() {
        assert_eq!(
            accepted_depfile_output_of(Some("foo.o.d"), Some("gcc"), Some("cc -c foo.c"), None),
            None
        );
    }

    /// An absolute or escaping path would be silently rebased into the
    /// build dir, so it is left alone entirely.
    #[test]
    fn a_depfile_outside_the_build_dir_is_refused() {
        for d in ["/tmp/foo.o.d", "../foo.o.d"] {
            assert_eq!(
                accepted_depfile_output_of(Some(d), Some("gcc"), Some(CC), None),
                None,
                "{d} must not be adopted as an output"
            );
        }
    }

    #[test]
    fn an_empty_depfile_path_is_refused() {
        assert_eq!(
            accepted_depfile_output_of(Some(""), Some("gcc"), Some(CC), None),
            None
        );
    }

    /// THE libvmaf REGRESSION. meson's nasm rule is `deps = gcc` with
    /// `-MQ <target> -MF <path>` and NO generation flag. nasm writes nothing,
    /// exits 0, and declaring the depfile an output fails the task on an
    /// output the command never produces. `-MF` names a destination; it does
    /// not ask for a depfile.
    #[test]
    fn nasm_naming_a_depfile_without_asking_for_one_is_refused() {
        let nasm = "nasm -f elf64 -I ../src/ -MQ src/cpuid.obj \
                    -MF src/cpuid.obj.ndep ../src/x86/cpuid.asm -o src/cpuid.obj";
        assert_eq!(
            accepted_depfile_output_of(Some("src/cpuid.obj.ndep"), Some("gcc"), Some(nasm), None),
            None,
            "-MF alone must not be read as a promise that anything writes it"
        );
    }

    /// And the same command WITH a generation flag is accepted, so the fix
    /// narrows the gate rather than closing it.
    #[test]
    fn the_same_command_with_a_generation_flag_is_accepted() {
        let nasm =
            "nasm -f elf64 -MD -MQ src/cpuid.obj -MF src/cpuid.obj.ndep ../src/x86/cpuid.asm";
        assert_eq!(
            accepted_depfile_output_of(Some("src/cpuid.obj.ndep"), Some("gcc"), Some(nasm), None),
            Some(PathBuf::from("src/cpuid.obj.ndep"))
        );
    }

    /// The flag can live in the rspfile rather than the command line.
    #[test]
    fn the_depfile_flag_is_honoured_from_the_rspfile() {
        assert_eq!(
            accepted_depfile_output_of(
                Some("foo.o.d"),
                Some("gcc"),
                Some("cc @foo.rsp"),
                Some("-MD -MF foo.o.d -c foo.c")
            ),
            Some(PathBuf::from("foo.o.d"))
        );
    }
}
