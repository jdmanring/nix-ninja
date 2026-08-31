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

                    // A DECLARED INPUT CAN BRING MORE THAN ITSELF. A
                    // python script is the case that forced this: the
                    // edge declares tool.py and nothing it imports, so
                    // the script starts inside the sandbox and dies on
                    // its first `import`. upload_referenced_file uploads
                    // the file AND, for a .py, walks its import closure.
                    // Everything else costs one extension check.
                    let uploaded = upload_referenced_file(
                        &self.rpc_client,
                        &self.config.build_dir,
                        file.name.clone().into(),
                    )?;
                    let mut declared = None;
                    for input in uploaded {
                        self.add_derived_file(files, input.clone());
                        if declared.is_none() {
                            declared = Some(input.clone());
                        } else {
                            input_set.insert(input.build_path.clone(), input);
                        }
                    }
                    // The first element is the declared file itself; the
                    // rest are its closure and are inserted above.
                    match declared {
                        Some(d) => d,
                        None => continue,
                    }
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

/// Python module names shipped with the interpreter: an import of one
/// is satisfied by the runtime, so it must never trigger the ancestor
/// and vendored-tree probes below. Incomplete by design - a missed name
/// costs a few stats and a failed directory probe, never a wrong file.
const PY_STDLIB: &[&str] = &[
    "abc",
    "argparse",
    "ast",
    "base64",
    "binascii",
    "bisect",
    "codecs",
    "collections",
    "contextlib",
    "copy",
    "csv",
    "ctypes",
    "dataclasses",
    "datetime",
    "difflib",
    "enum",
    "errno",
    "fnmatch",
    "functools",
    "getopt",
    "glob",
    "gzip",
    "hashlib",
    "heapq",
    "html",
    "http",
    "io",
    "importlib",
    "inspect",
    "itertools",
    "json",
    "keyword",
    "locale",
    "logging",
    "math",
    "multiprocessing",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pickle",
    "platform",
    "posixpath",
    "pprint",
    "queue",
    "random",
    "re",
    "shlex",
    "shutil",
    "signal",
    "site",
    "socket",
    "stat",
    "string",
    "struct",
    "subprocess",
    "sys",
    "tempfile",
    "textwrap",
    "threading",
    "time",
    "traceback",
    "types",
    "typing",
    "unittest",
    "urllib",
    "uuid",
    "warnings",
    "xml",
    "zipfile",
    "zlib",
];

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
            upload_python_closure(rpc_client, build_dir, dir, &mut out)?;
        }
    }
    Ok(out)
}

/// Upload the TRANSITIVE python dependency closure rooted at a script's
/// directory. Chromium's build scripts resolve imports through every
/// mechanism python has - sibling modules, sibling packages, uncle
/// directories whose names need not match the module, sys.path splices
/// assembled across statements, and vendored version-suffixed trees
/// (beautifulsoup4-4.9.3/py3k/bs4) - and they do it TRANSITIVELY:
/// parse_html_deps.py, itself two packages deep, imports bs4. So every
/// uploaded package directory gets the same scan its referencing script
/// got, until the closure is dry. Bounded by a directory-count cap and
/// a visited set; existence discriminates every candidate.
fn upload_python_closure(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    start_dir: &Path,
    out: &mut Vec<DerivedFile>,
) -> Result<()> {
    // Memoized on the script DIRECTORY alone. The key was
    // (script dir, script) and that coarseness was the whole serial
    // bottleneck at Chromium scale: the walk depends on the script only
    // through a skip of the script's own file, so every script in a
    // shared directory (mojom, grit: hundreds per dir) re-paid the full
    // tree walk under a fresh key. perf on round 60: Path::join plus
    // malloc churn under upload_python_closure_uncached at ~50% of all
    // driver cycles. The skip is gone (the script re-uploads as one
    // more content-cached file and the consumer's input_set dedupes by
    // path), so the result is script-independent and one walk per
    // directory serves every script in it.
    out.extend(
        python_closure_cached(rpc_client, build_dir, start_dir)?
            .iter()
            .cloned(),
    );
    Ok(())
}

/// Recursive regular-file upload of one directory, or None past the cap.
fn walk_dir_capped(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
    cap: usize,
) -> Result<Option<Vec<DerivedFile>>> {
    // Memoized process-wide: the python closure runs per TASK, and at
    // Chromium scale the same directories recur thousands of times -
    // including over-cap refusals, which cost a full capped walk each
    // (the chromium root was being re-walked to 8192 entries per task,
    // measured as the driver pinning a core while the build sat idle).
    // Store dedup makes repeat uploads cheap; this makes them free, and
    // makes the NEGATIVE result free too, which is the half that
    // mattered.
    // Keyed by (directory, cap) because the cap changes which entries a walk
    // is allowed to return, so a hit under one cap is not a hit under another.
    // None is a REMEMBERED NEGATIVE - the half that mattered, per the comment
    // above - not an absent entry.
    type WalkMemo = std::sync::Mutex<HashMap<(PathBuf, usize), Option<Vec<DerivedFile>>>>;
    static WALK_MEMO: std::sync::OnceLock<WalkMemo> = std::sync::OnceLock::new();
    let memo = WALK_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let key = (dir.to_path_buf(), cap);
    if let Some(hit) = memo.lock().unwrap().get(&key) {
        return Ok(hit.clone());
    }
    let result = walk_dir_capped_uncached(rpc_client, build_dir, dir, cap)?;
    memo.lock().unwrap().insert(key, result.clone());
    Ok(result)
}

fn walk_dir_capped_uncached(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    dir: &Path,
    cap: usize,
) -> Result<Option<Vec<DerivedFile>>> {
    let mut paths = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries =
            fs::read_dir(&d).map_err(|e| anyhow!("read_dir({}) for dir arg: {e}", d.display()))?;
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
    // costs a directory scan and zero store writes. Batched adds: a
    // node_modules tree is thousands of files at one round trip each.
    let out = new_opaque_files(rpc_client, build_dir, paths)?;
    Ok(Some(out))
}

fn upload_python_closure_uncached(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    start_dir: &Path,
    out: &mut Vec<DerivedFile>,
) -> Result<()> {
    const CLOSURE_DIR_CAP: usize = 64;
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut queue: Vec<PathBuf> = vec![start_dir.to_path_buf()];
    let upload_dir = |dir: &Path, cap: usize, out: &mut Vec<DerivedFile>| -> Result<bool> {
        match walk_dir_capped(rpc_client, build_dir, dir, cap)? {
            Some(files) => {
                out.extend(files);
                Ok(true)
            }
            None => {
                eprintln!(
                    "nix-ninja: python dep dir {} exceeds {} files; skipped",
                    dir.display(),
                    cap
                );
                Ok(false)
            }
        }
    };
    while let Some(dir) = queue.pop() {
        // Dedup and cap are separate questions, and running them as one
        // expression made the cap self-feeding: every directory dropped for
        // being over the cap was first INSERTED, so `visited` kept growing
        // while nothing more was scanned.
        let step = closure_step(visited.insert(dir.clone()), visited.len(), CLOSURE_DIR_CAP);
        if step == ClosureStep::Seen {
            continue;
        }
        // OVER THE CAP IS A REFUSAL, NOT A TRUNCATION. Dropping the rest
        // yields a closure missing modules the script imports, and this
        // crate's whole polarity is that under-declaring ships a wrong
        // artifact quietly while over-declaring costs an upload. It also
        // used to depend on readdir order, so which directories survived
        // differed between machines and the input set was not a function of
        // the source tree - the one thing a content-addressed per-unit build
        // cannot tolerate.
        if step == ClosureStep::Refuse {
            return Err(anyhow!(
                "python import closure below {} exceeds {} directories; \
                 refusing to declare a partial closure",
                start_dir.display(),
                CLOSURE_DIR_CAP
            ));
        }
        // Siblings: every .py module, node_modules for node.py, and
        // package directories - which recurse, because their own files
        // import too.
        //
        // SORTED, so the traversal is a function of the tree. readdir order
        // is filesystem order, and with a LIFO queue that decided which
        // directories were reached first.
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| anyhow!("read_dir({}) for python closure: {e}", dir.display()))?
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "py") && p.is_file() {
                out.push(new_opaque_file(rpc_client, build_dir, p)?);
            } else if p.is_dir() && p.file_name().is_some_and(|n| n == "node_modules") {
                upload_dir(&p, 8192, out)?;
            } else if p.is_dir() && p.join("__init__.py").is_file() && upload_dir(&p, 512, out)? {
                queue.push(p);
            }
        }
        // Imports this directory's files make that nothing here satisfies.
        let unsatisfied: Vec<String> = python_import_names(&dir)?
            .into_iter()
            .filter(|name| {
                !PY_STDLIB.contains(&name.as_str())
                    && !dir.join(format!("{name}.py")).is_file()
                    && !dir.join(name).is_dir()
            })
            .collect();
        if let Some(parent) = dir.parent() {
            if !unsatisfied.is_empty() {
                // Uncles: a directory beside this one holding <name>.py
                // (common/ holds models.py) or BEING the named package
                // (tracing/tracing_build/ answers import tracing_build).
                // SORTED, like every other walk here: these are pushed onto
                // the queue, so readdir order decided which uncle was
                // reached first and therefore what the closure contained.
                let mut uncles: Vec<PathBuf> = fs::read_dir(parent)
                    .map_err(|e| anyhow!("read_dir({}) for uncle modules: {e}", parent.display()))?
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && *p != dir)
                    .collect();
                uncles.sort();
                for uncle in uncles {
                    let hit = unsatisfied.iter().any(|n| {
                        uncle.join(format!("{n}.py")).is_file()
                            || (uncle.file_name().is_some_and(|f| f == n.as_str())
                                && uncle.join("__init__.py").is_file())
                    });
                    if hit && upload_dir(&uncle, 512, out)? {
                        queue.push(uncle);
                    }
                }
                // Ancestors, by chromium's layout: <root>/<name>/,
                // <root>/third_party/<name>/, and the vendored deep
                // shape <root>/third_party/<pkg-x.y.z>[/<subdir>]/<name>/
                // (beautifulsoup4-4.9.3/py3k/bs4). Four levels up, first
                // hit per name; the deep scan runs only when the cheap
                // probes miss, and only over third_party.
                for name in &unsatisfied {
                    let mut anc = parent.to_path_buf();
                    'levels: for _ in 0..4 {
                        let Some(up) = anc.parent() else { break };
                        anc = up.to_path_buf();
                        for cand in [anc.join(name), anc.join("third_party").join(name)] {
                            let module = cand.join(format!("{name}.py")).is_file();
                            let package = cand.join("__init__.py").is_file();
                            if module || package {
                                if upload_dir(&cand, 8192, out)? && package {
                                    queue.push(cand);
                                }
                                break 'levels;
                            }
                        }
                        // A vendored tree can hold SEVERAL copies of one
                        // package - catapult carries a legacy
                        // beautifulsoup4/ AND beautifulsoup4-4.9.3/py3k/,
                        // and its own sys.path assembly picks the py3 one
                        // across three statements no scan can chase. So
                        // collect every candidate and prefer the
                        // python-3, version-suffixed copy: the legacy one
                        // imports interfaces that no longer exist
                        // (html5lib.treebuilders._base, measured).
                        let tp = anc.join("third_party");
                        let mut cands: Vec<PathBuf> = Vec::new();
                        if let Ok(subs) = fs::read_dir(&tp) {
                            for sub in subs.flatten().map(|e| e.path()) {
                                if !sub.is_dir() {
                                    continue;
                                }
                                let direct = sub.join(name);
                                if direct.join("__init__.py").is_file() {
                                    cands.push(direct);
                                }
                                // No else: a direct hit must not hide a
                                // deeper sibling copy of the same package
                                // (4.9.3/bs4 sits BESIDE 4.9.3/py3k/bs4,
                                // and only the deeper one is python 3).
                                if let Ok(subs2) = fs::read_dir(&sub) {
                                    cands.extend(
                                        subs2
                                            .flatten()
                                            .map(|e| e.path().join(name))
                                            .filter(|d| d.join("__init__.py").is_file()),
                                    );
                                }
                            }
                        }
                        let score = |p: &Path| -> u32 {
                            let s = p.to_string_lossy().into_owned();
                            let versioned = s.split('/').any(|c| {
                                c.contains('-') && c.chars().any(|ch| ch.is_ascii_digit())
                            });
                            u32::from(versioned) * 2 + u32::from(s.contains("py3"))
                        };
                        // `max_by_key` returns the LAST maximum, so with an
                        // unsorted candidate list two equally-scored vendored
                        // copies were chosen by readdir order. Sorting makes
                        // the tie-break the path, which is a property of the
                        // tree.
                        cands.sort();
                        if let Some(pkg) = cands.into_iter().max_by_key(|p| score(p)) {
                            if upload_dir(&pkg, 8192, out)? {
                                queue.push(pkg);
                            }
                            break 'levels;
                        }
                    }
                }
                // Descendants: a sys.path.insert into the importer's OWN
                // subtree, carried through a variable the one-line splice
                // reader cannot chase (json_schema_compiler inserts
                // ppapi/generators and imports idl_parser from it; the
                // ppapi dir has no __init__.py, so package recursion never
                // descends there). Bounded walk for <name>.py or
                // <name>/__init__.py; the CONTAINING dir uploads and
                // queues so its own imports (idl_lexer -> ply) get chased.
                for name in &unsatisfied {
                    if let Some(holder) = find_module_below(&dir, name, 3) {
                        if upload_dir(&holder, 512, out)? {
                            queue.push(holder);
                        }
                    }
                }
            }
            // sys.path splices readable on one line still help for the
            // shapes the structural probes miss.
            for sp in python_syspath_dirs(&dir)? {
                if upload_dir(&sp, 8192, out)? {
                    queue.push(sp);
                }
            }
            // A *_project.py file is catapult's convention for a
            // vulcanize project definition: its os.path.join lines name
            // the DATA roots the tool searches (tracing/, polymer
            // components), assembled from attributes no import scan can
            // see. Each join line's quoted segments resolve against the
            // file's own dir, its parent, and the parent's third_party;
            // existing directories upload, same bound as everything
            // here.
            let has_project_file = fs::read_dir(&dir)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().ends_with("_project.py"));
            if has_project_file {
                let mut bases = vec![dir.clone()];
                if let Some(par) = dir.parent() {
                    bases.push(par.to_path_buf());
                    bases.push(par.join("third_party"));
                }
                for seg_path in python_join_segments(&dir)? {
                    for base in &bases {
                        let cand = lexical_join(base, &seg_path);
                        if cand.is_dir() && cand != dir {
                            if upload_dir(&cand, 8192, out)? {
                                queue.push(cand);
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// The memoized closure behind upload_python_closure, shared by Arc so
/// a caller that only needs to CHECK membership (the per-task python
/// sibling pass) iterates the one canonical copy instead of cloning
/// thousands of DerivedFiles per hit - the clone-on-hit was 15% of
/// driver cycles (malloc/free) on round 65's profile.
fn python_closure_cached(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &Path,
    start_dir: &Path,
) -> Result<Arc<Vec<DerivedFile>>> {
    static CLOSURE_MEMO: std::sync::OnceLock<
        std::sync::Mutex<HashMap<PathBuf, Arc<Vec<DerivedFile>>>>,
    > = std::sync::OnceLock::new();
    let memo = CLOSURE_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(hit) = memo.lock().unwrap().get(start_dir) {
        return Ok(hit.clone());
    }
    let mut fresh: Vec<DerivedFile> = Vec::new();
    upload_python_closure_uncached(rpc_client, build_dir, start_dir, &mut fresh)?;
    let arc = Arc::new(fresh);
    memo.lock()
        .unwrap()
        .insert(start_dir.to_path_buf(), arc.clone());
    Ok(arc)
}

/// Top-level module names imported by a python package's own files:
/// `import X` and `from X import ...`, first path segment only. Lines
/// are trimmed before matching, so INDENTED imports count too: this
/// scanner once matched column 0 only, reasoning that a try-block import
/// is optional - and chromium's json_parse.py disproved it with a
/// MANDATORY import inside try/finally whose only job is restoring
/// sys.path around it (import json_comment_eater, 65 task failures in
/// round 66). Over-collecting fails toward extra uploads, which the
/// per-dir caps bound; under-collecting fails the build.
fn python_import_names(pkg: &Path) -> Result<Vec<String>> {
    // Memoized like walk_dir_capped and for the same reason: the
    // closure runs per task and re-reads the same directories' file
    // bodies thousands of times otherwise.
    static NAMES_MEMO: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Vec<String>>>> =
        std::sync::OnceLock::new();
    let memo = NAMES_MEMO.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(hit) = memo.lock().unwrap().get(pkg) {
        return Ok(hit.clone());
    }
    let result = python_import_names_uncached(pkg)?;
    memo.lock()
        .unwrap()
        .insert(pkg.to_path_buf(), result.clone());
    Ok(result)
}

fn python_import_names_uncached(pkg: &Path) -> Result<Vec<String>> {
    let mut names = std::collections::BTreeSet::new();
    let entries = fs::read_dir(pkg)
        .map_err(|e| anyhow!("read_dir({}) for import scan: {e}", pkg.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "py") {
            continue;
        }
        let body = fs::read_to_string(&p)
            .map_err(|e| anyhow!("read({}) for import scan: {e}", p.display()))?;
        for line in body.lines() {
            let line = line.trim_start();
            let rest = if let Some(r) = line.strip_prefix("import ") {
                r
            } else if let Some(r) = line.strip_prefix("from ") {
                r
            } else {
                continue;
            };
            let first = rest.split([' ', '.', ',']).next().unwrap_or("");
            if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                names.insert(first.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// Directories the .py files in `dir` splice onto sys.path, read
/// textually: every line containing "sys.path" contributes its quoted
/// string segments, joined in order and resolved lexically against the
/// script directory. Existence-discriminated by the caller; a segment
/// list that is not a real relative path simply resolves to nothing.
fn python_syspath_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let entries = fs::read_dir(dir)
        .map_err(|e| anyhow!("read_dir({}) for sys.path scan: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "py") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&p) else {
            continue;
        };
        for line in body.lines() {
            if !line.contains("sys.path") {
                continue;
            }
            let mut segs: Vec<&str> = Vec::new();
            let mut rest = line;
            while let Some(start) = rest.find(['\'', '"']) {
                let quote = rest.as_bytes()[start] as char;
                let after = &rest[start + 1..];
                let Some(end) = after.find(quote) else { break };
                let seg = &after[..end];
                if !seg.is_empty() && !seg.contains(':') {
                    segs.push(seg);
                }
                rest = &after[end + 1..];
            }
            if segs.is_empty() {
                continue;
            }
            let rel: PathBuf = segs.iter().collect();
            let cand = lexical_join(dir, &rel);
            if cand.is_dir() && cand != dir && !found.contains(&cand) {
                found.push(cand);
            }
        }
    }
    Ok(found)
}

/// Quoted string segments of every os.path.join line in the *_project.py
/// files of `dir`, each line yielding one relative path. Used only for
/// catapult-style project definition files, whose join lines name data
/// roots; the caller resolves against candidate bases and keeps what
/// exists.
fn python_join_segments(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let entries = fs::read_dir(dir)
        .map_err(|e| anyhow!("read_dir({}) for project scan: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if !p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with("_project.py"))
        {
            continue;
        }
        let Ok(body) = fs::read_to_string(&p) else {
            continue;
        };
        for line in body.lines() {
            if !line.contains("os.path.join") {
                continue;
            }
            let mut segs: Vec<&str> = Vec::new();
            let mut rest = line;
            while let Some(start) = rest.find(['\'', '"']) {
                let quote = rest.as_bytes()[start] as char;
                let after = &rest[start + 1..];
                let Some(end) = after.find(quote) else { break };
                let seg = &after[..end];
                if !seg.is_empty() && !seg.contains(':') {
                    segs.push(seg);
                }
                rest = &after[end + 1..];
            }
            if !segs.is_empty() {
                let rel: PathBuf = segs.iter().collect();
                if !found.contains(&rel) {
                    found.push(rel);
                }
            }
        }
    }
    Ok(found)
}

/// The directory below `root` holding `<name>.py`, or the package dir
/// `<name>/` holding an `__init__.py` - whichever a runtime
/// sys.path.insert into the importer's own subtree would reach. BFS to
/// `depth`, skipping hidden dirs and node_modules; first hit wins.
fn find_module_below(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for d in frontier {
            let entries = fs::read_dir(&d).ok()?;
            for sub in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                let fname = sub.file_name().unwrap_or_default().to_string_lossy();
                if fname.starts_with('.') || fname == "node_modules" {
                    continue;
                }
                if sub.join(format!("{name}.py")).is_file() {
                    return Some(sub);
                }
                if fname == name && sub.join("__init__.py").is_file() {
                    return Some(sub);
                }
                next.push(sub);
            }
        }
        frontier = next;
    }
    None
}

/// Join a relative path onto a base LEXICALLY: each `..` pops a base
/// component instead of surviving into the result, so
/// gen/ui/webui/resources/js + ../../../../../../../../../../src/x
/// becomes ../../../../../src/x against the build dir - resolvable by
/// the existing five-up sandbox mirror, where the raw twenty-component
/// climb is not. Pops past the base accumulate as leading `..`.
fn lexical_join(base: &Path, rel: &Path) -> PathBuf {
    let mut stack: Vec<std::path::Component> = base.components().collect();
    for c in rel.components() {
        match c {
            std::path::Component::ParentDir => {
                if matches!(stack.last(), Some(std::path::Component::Normal(_))) {
                    stack.pop();
                } else {
                    stack.push(c);
                }
            }
            std::path::Component::CurDir => {}
            other => stack.push(other),
        }
    }
    stack.iter().map(|c| c.as_os_str()).collect()
}

fn new_opaque_files(
    rpc_client: &Arc<BuilderRpcClient>,
    build_dir: &std::path::Path,
    paths: Vec<PathBuf>,
) -> Result<Vec<DerivedFile>> {
    const PER_TASK_ADDS: usize = 8;
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(PER_TASK_ADDS) {
        let results: Vec<Result<DerivedFile>> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|p| scope.spawn(move || new_opaque_file(rpc_client, build_dir, p.clone())))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(anyhow!("upload thread panicked")))
                })
                .collect()
        });
        for r in results {
            out.push(r?);
        }
    }
    Ok(out)
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
mod python_closure_tests {
    use super::python_import_names;

    /// `find_module_below` carries the resolution: it decides which
    /// directory an import name refers to, and every wrong answer is
    /// either a missing input at build time or an unrelated tree uploaded.
    /// Four conditions, four cases, because a single fixture that trips
    /// two of them cannot tell them apart.
    #[test]
    fn find_module_below_finds_a_module_and_a_package_and_respects_its_bounds() {
        use super::find_module_below;
        use std::fs;
        let root = std::env::temp_dir().join(format!("nn-fmb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // lib/mod.py  -> a MODULE file inside a directory
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(root.join("lib/mod.py"), "").unwrap();
        // pkg/__init__.py -> a PACKAGE directory named for the import
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/__init__.py"), "").unwrap();
        // pkgless/ -> named right, but no __init__.py, so NOT a package
        fs::create_dir_all(root.join("pkgless")).unwrap();
        // .hidden and node_modules are skipped by name, and each holds a
        // module that would otherwise match
        fs::create_dir_all(root.join(".hidden/inner")).unwrap();
        fs::write(root.join(".hidden/inner/buried.py"), "").unwrap();
        fs::create_dir_all(root.join("node_modules/inner")).unwrap();
        fs::write(root.join("node_modules/inner/vendored.py"), "").unwrap();
        // deep/er/far.py sits two levels down
        fs::create_dir_all(root.join("deep/er")).unwrap();
        fs::write(root.join("deep/er/far.py"), "").unwrap();

        assert_eq!(find_module_below(&root, "mod", 2), Some(root.join("lib")));
        assert_eq!(find_module_below(&root, "pkg", 2), Some(root.join("pkg")));
        // A directory named for the import with no __init__.py is not a
        // package: taking it would upload an unrelated tree.
        assert_eq!(find_module_below(&root, "pkgless", 2), None);
        // DEPTH IS A BOUND, not a suggestion: far.py is two levels down
        // and must be invisible at depth 1.
        assert_eq!(find_module_below(&root, "far", 1), None);
        assert_eq!(
            find_module_below(&root, "far", 2),
            Some(root.join("deep/er"))
        );
        // The two skipped directory names, each proven separately.
        assert_eq!(find_module_below(&root, "buried", 3), None);
        assert_eq!(find_module_below(&root, "vendored", 3), None);
        fs::remove_dir_all(&root).unwrap();
    }

    /// The two quoted-segment scanners share a shape and each had every
    /// mutant of it survive. Both must join multi-segment paths, skip an
    /// empty segment and skip a segment carrying a colon (a URL or a
    /// Windows drive is not a path component here), and dedupe.
    #[test]
    fn join_segments_reads_multi_segment_paths_and_skips_the_junk() {
        use super::python_join_segments;
        use std::fs;
        use std::path::PathBuf;
        let dir = std::env::temp_dir().join(format!("nn-pjs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Only a *_project.py file is read; the other must be ignored.
        fs::write(dir.join("other.py"), "P = os.path.join('never', 'read')\n").unwrap();
        fs::write(
            dir.join("build_project.py"),
            "A = os.path.join('src', 'gen')\n\
             B = os.path.join('src', '', 'gen')\n\
             C = os.path.join('https://x', 'y')\n\
             D = os.path.join('src', 'gen')\n\
             E = 'not a join call'\n",
        )
        .unwrap();
        let got = python_join_segments(&dir).unwrap();
        // A and B collapse to the same path (the empty segment is
        // dropped, not joined as a component), D is a repeat, C's colon
        // segment is dropped leaving only 'y', and E is not a join line.
        assert_eq!(
            got,
            vec![PathBuf::from("src/gen"), PathBuf::from("y")],
            "{got:?}"
        );
        // A directory with no *_project.py yields nothing rather than
        // erroring: the scan is opportunistic.
        let empty = dir.join("sub");
        fs::create_dir_all(&empty).unwrap();
        assert!(python_join_segments(&empty).unwrap().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

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

    // The json_parse.py shape: a MANDATORY import indented inside a
    // try/finally whose only job is restoring sys.path around it. The
    // column-0-only scanner missed it (65 task failures, round 66).
    #[test]
    fn finds_indented_import_in_try_block() {
        let d = std::env::temp_dir().join(format!("nn-imports-ind-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("json_parse.py"),
            "import sys\ntry:\n  sys.path.insert(0, p)\n  import json_comment_eater\nfinally:\n  sys.path = s\n",
        )
        .unwrap();
        let names = python_import_names(&d).unwrap();
        assert!(names.contains(&"json_comment_eater".to_string()));
        std::fs::remove_dir_all(&d).unwrap();
    }

    // The idl_parser shape: sys.path.insert into the importer's OWN
    // subtree, path carried through a variable (json_schema_compiler ->
    // ppapi/generators/idl_parser.py, 64 task failures, round 72). The
    // probe must find a bare module two levels down, and a negative
    // control must stay empty.
    #[test]
    fn finds_module_in_own_subtree() {
        let d = std::env::temp_dir().join(format!("nn-below-{}", std::process::id()));
        std::fs::create_dir_all(d.join("ppapi/generators")).unwrap();
        std::fs::write(d.join("ppapi/generators/idl_parser.py"), "x = 1\n").unwrap();
        let hit = super::find_module_below(&d, "idl_parser", 3).unwrap();
        assert_eq!(hit, d.join("ppapi/generators"));
        assert!(super::find_module_below(&d, "no_such_module", 3).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn syspath_dirs_read_quoted_segments() {
        use super::python_syspath_dirs;
        use std::fs;
        let root = std::env::temp_dir().join(format!("sp-test-{}", std::process::id()));
        let tools = root.join("tools/polymer");
        let node = root.join("third_party/node");
        fs::create_dir_all(&tools).unwrap();
        fs::create_dir_all(&node).unwrap();
        fs::write(
            tools.join("css_to_wrapper.py"),
            "import sys, os\nsys.path.append(os.path.join(_HERE_PATH, '..', '..', 'third_party', 'node'))\nimport node\n",
        )
        .unwrap();
        let dirs = python_syspath_dirs(&tools).unwrap();
        assert_eq!(dirs, vec![node]);
        fs::remove_dir_all(&root).unwrap();
    }
}

/// What to do with the directory just taken off the closure queue.
#[derive(Debug, PartialEq, Eq)]
enum ClosureStep {
    /// Already walked. Costs nothing and must not count toward the cap.
    Seen,
    /// Over the cap. The closure would be partial, so refuse it.
    Refuse,
    Scan,
}

/// Dedup and cap are SEPARATE questions and were one expression, which made
/// the cap self-feeding: a directory dropped for being over the cap had
/// already been inserted, so the set kept growing while nothing more was
/// scanned. Ordering is the defect, so ordering is what this pins.
fn closure_step(newly_inserted: bool, visited_len: usize, cap: usize) -> ClosureStep {
    if !newly_inserted {
        ClosureStep::Seen
    } else if visited_len > cap {
        ClosureStep::Refuse
    } else {
        ClosureStep::Scan
    }
}

#[cfg(test)]
mod closure_step_tests {
    use super::{closure_step, ClosureStep};

    #[test]
    fn a_fresh_directory_under_the_cap_is_scanned() {
        assert_eq!(closure_step(true, 3, 64), ClosureStep::Scan);
    }

    #[test]
    fn over_the_cap_refuses_rather_than_truncating() {
        assert_eq!(closure_step(true, 65, 64), ClosureStep::Refuse);
    }

    /// THE ORDERING THAT WAS WRONG. A directory already walked is skipped
    /// because it is a duplicate, and that answer must not depend on how
    /// full the set is - a repeat arriving after the cap is still a repeat,
    /// not a reason to fail the build.
    #[test]
    fn a_duplicate_is_seen_even_when_the_set_is_over_the_cap() {
        assert_eq!(closure_step(false, 9_000, 64), ClosureStep::Seen);
    }
}
