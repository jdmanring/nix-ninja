use anyhow::{anyhow, Result};
use harmonia_store_derivation::derivation::Derivation;
use harmonia_store_derivation::derived_path::{OutputName, SingleDerivedPath};
use harmonia_store_path::{StoreDir, StorePath};
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::DerivedFile;
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use crate::task::discover_c_includes;

pub fn run(store_dir: &StoreDir, targets: Vec<String>) -> Result<()> {
    let input_drv = targets
        .first()
        .ok_or_else(|| anyhow!("Expected derivation path as argument"))?;

    let drv_json = fs::read_to_string(input_drv)?;
    let mut drv: Derivation = serde_json::from_str(&drv_json)?;
    println!("nix-ninja-dynamic-task: Processing derivation {}", drv.name);

    let rpc_client = Arc::new(BuilderRpcClient::connect_from_env(None)?);

    // Stage 1: Prepare build environment
    let (build_dir, built_paths) = prepare_build_environment(store_dir)?;

    // Stage 2: Discover dynamic dependencies
    let discovered =
        discover_dynamic_dependencies(&rpc_client, store_dir, &build_dir, &drv, built_paths)?;

    // A `..` spelling discovered HERE has to reach the final derivation:
    // this subtool emits the derivation, it does not run the command, so
    // creating the directory in this sandbox would help nobody.
    crate::task::declare_dotdot_dirs(&mut drv, &build_dir, &discovered.dotdot_dirs);

    // Stage 3: Update derivation with discovered dependencies
    let new_deps = update_derivation_with_discoveries(
        &mut drv,
        discovered.deps,
        discovered.store_paths,
        store_dir,
    )?;

    // Print discovery results
    if !new_deps.is_empty() {
        for dep in &new_deps {
            println!(
                "nix-ninja-dynamic-task: Discovered dependency: {}",
                dep.derived_path.root_path()
            );
        }
    } else {
        println!("nix-ninja-dynamic-task: No new dependencies discovered");
    }

    let drv_path = rpc_client.add_drv_to_store(store_dir, &drv)?;

    rpc_client.submit_output(
        &SingleDerivedPath::Opaque(drv_path.clone()),
        &OutputName::default(),
    )?;

    println!("nix-ninja-dynamic-task: Added derivation to store: {drv_path}");
    Ok(())
}

/// Stage 1: Prepare build environment by setting up directories, copying source,
/// and building derived files
fn prepare_build_environment(store_dir: &StoreDir) -> Result<(PathBuf, HashMap<PathBuf, PathBuf>)> {
    // Set up build directory using NIX_BUILD_TOP
    let build_top =
        env::var("NIX_BUILD_TOP").map_err(|_| anyhow!("Expected $NIX_BUILD_TOP to be set"))?;
    let source_dir = PathBuf::from(build_top).join("source");
    let build_dir = source_dir.join("build");
    fs::create_dir_all(&build_dir)?;
    env::set_current_dir(&build_dir)?;

    // Copy $src into source_dir so we can discover dependencies from $src.
    let src = env::var("src").map_err(|_| anyhow!("Expected $src to be set"))?;
    copy_dir_all(PathBuf::from(src), &source_dir)?;

    // Get NIX_NINJA_INPUTS from process environment, these are the built
    // inputs to a derivation that may have discovered inputs and should be
    // scanned.
    // Inline, or via nix's passAsFile (NIX_NINJA_INPUTSPath) when the
    // encoded set is too large for an env var - see build_task_derivation.
    let inputs = match env::var("NIX_NINJA_INPUTS") {
        Ok(v) => v,
        Err(_) => match env::var("NIX_NINJA_INPUTSPath") {
            Ok(p) => std::fs::read_to_string(&p)
                .map_err(|e| anyhow!("reading NIX_NINJA_INPUTSPath={p}: {e}"))?,
            Err(_) => {
                return Err(anyhow!(
                    "neither NIX_NINJA_INPUTS nor NIX_NINJA_INPUTSPath in process environment"
                ))
            }
        },
    };

    // Get built inputs for dynamic dependency discovery
    let derived_files: Vec<DerivedFile> = inputs
        .split_whitespace()
        .filter_map(|encoded| DerivedFile::from_encoded(store_dir, encoded).ok())
        .collect();

    // In derivation mode, built files are already available as store paths
    // Create the virtual paths mapping from the derived files
    let built_paths: HashMap<PathBuf, PathBuf> = derived_files
        .iter()
        .map(|df| (df.build_path.clone(), df.absolute_path(store_dir)))
        .collect();

    Ok((build_dir, built_paths))
}

/// The files the include scan is SEEDED with, drawn from a task's built
/// inputs.
///
/// A DIRECTORY-SHAPED BUILT INPUT IS NOT A TRANSLATION UNIT, AND SEEDING ONE
/// FAILS THE WHOLE TASK. An edge may declare a directory as its output -
/// `ninja-build-include-kernel-abi`, whose interior `include/kernel-abi` is a
/// directory - and every built input is seeded for the scan AND handed to it
/// as the virtual map. That PAIRING is what defeats the directory guard:
/// `canonicalize_cached` takes the virtual hit before reaching its own
/// `is_dir` check, hands back the store path, and the walk reads it:
///
/// ```text
/// Failed to read file <store>-ninja-build-include-kernel-abi/
///   include/kernel-abi: Is a directory (os error 21)
/// ```
///
/// 189 occurrences of the message in one round, 161 rdma-core and 28
/// llvm-tblgen, counted by the consumer over round 19. That is a count of
/// OCCURRENCES; CLAUDE.md's "185 task failures" is a count of TASKS and the
/// two are not the same unit. The guard `4e591a0` added is an ancestor of
/// that round's pin, which is verified here; that the round WAS pinned
/// there is the consumer's reading of their own lock.
/// Every edition needs clang.
///
/// The STORE side decides, because the map's value is what the walk reads.
///
/// ONLY THE SEED IS FILTERED, AND THE RESIDUAL IS NOT BENIGN. The virtual
/// entry stays, so an include that RESOLVES to that directory still
/// resolves; the downstream check then refuses it, and that refusal is
/// `.ok_or(anyhow!("Required file not found ..."))?` - a hard error that
/// fails the whole task, not a fall-through. So this covers the SEED route
/// and a task whose source includes the directory by name still dies, with
/// a different message. `#include <memory>` against a `memory/` directory
/// is the shape, and this tree has met it once already.
/// The seed route is what the round's 189 failures are.
///
/// THE ROOT FIX IS TWO LINES IN `canonicalize_cached` AND IS DECLINED ON
/// COST, WHICH IS THE ONLY GOOD REASON. Checking the virtual hit for a
/// directory at both bypasses - the direct probe and the lexically
/// normalized one below it - covers every route at once. That file is
/// `crates/deps-infer`, inside `nix-ninja-task`'s fileset, so it re-keys
/// every banked PLAIN task derivation. Land it when a batch is already
/// spending that. It would NOT break the generated-header class: a declared
/// but not yet written header is not a directory, so the guard does not see
/// it.
///
/// Extracted rather than written inline so a test can execute it. Three
/// defects have been repaired in this tree's placement code and none was
/// covered by a test that ran it.
pub fn scan_seeds(built_paths: &HashMap<PathBuf, PathBuf>) -> Vec<PathBuf> {
    built_paths
        .iter()
        .filter(|(_, store_path)| !store_path.is_dir())
        .map(|(build_path, _)| build_path.clone())
        .collect()
}

/// Stage 2: Discover dynamic dependencies by analyzing built files for includes
pub fn discover_dynamic_dependencies(
    rpc_client: &Arc<BuilderRpcClient>,
    store_dir: &StoreDir,
    build_dir: &Path,
    drv: &Derivation,
    built_paths: HashMap<PathBuf, PathBuf>,
) -> Result<crate::task::Discovered> {
    let cmdline_bytes = drv
        .args
        .first()
        .ok_or_else(|| anyhow!("No command line found in derivation"))?;
    let cmdline = std::str::from_utf8(cmdline_bytes)?;

    let files = scan_seeds(&built_paths);

    discover_c_includes(
        rpc_client,
        store_dir,
        build_dir,
        cmdline,
        files,
        Some(built_paths),
        // The dynamic task reconstructs its build dir fresh inside the
        // sandbox, so no prior run's depfile can exist there; the scan is
        // the only source. Upstream #17's read-back applies to the outer,
        // persistent build dir path only.
        None,
    )
}

/// Stage 3: Update derivation with discovered dependencies and store paths
/// Returns the list of new dependencies that were added
pub fn update_derivation_with_discoveries(
    drv: &mut Derivation,
    discovered_deps: Vec<DerivedFile>,
    discovered_store_paths: Vec<StorePath>,
    store_dir: &StoreDir,
) -> Result<Vec<DerivedFile>> {
    for store_path in &discovered_store_paths {
        drv.inputs
            .insert(SingleDerivedPath::Opaque(store_path.clone()));
    }

    // Get NIX_NINJA_INPUTS from derivation environment, these are the existing
    // inputs of the derivation without the discovered inputs.
    let key = b"NIX_NINJA_INPUTS";
    let drv_inputs = drv
        .env
        .iter()
        .find(|(k, _)| k.as_ref() == key)
        .map_or("", |(_, v)| std::str::from_utf8(v).unwrap());

    // Parse existing derivation inputs into a HashSet for deduplication
    let mut input_set: HashSet<String> = drv_inputs
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let mut new_deps = Vec::new();
    for derived_file in discovered_deps {
        let encoded = derived_file.to_encoded(store_dir);

        // Skip if already in input set
        if input_set.contains(&encoded) {
            continue;
        }

        new_deps.push(derived_file.clone());
        input_set.insert(encoded);
        drv.inputs.insert(derived_file.derived_path.clone());
    }

    if !new_deps.is_empty() {
        // Update NIX_NINJA_INPUTS with sorted list
        let mut inputs: Vec<String> = input_set.into_iter().collect();
        inputs.sort();
        drv.env.insert(
            b"NIX_NINJA_INPUTS"[..].into(),
            inputs.join(" ").into_bytes().into(),
        );
    }

    Ok(new_deps)
}

/// Recursively copies a directory and all its contents
fn copy_dir_all(src: PathBuf, dst: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;
    use walkdir::WalkDir;

    for entry in WalkDir::new(&src) {
        let entry = entry?;

        let relative_path = entry.path().strip_prefix(&src)?;
        let dest_path = dst.join(relative_path);

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file_type = entry.file_type();
        if file_type.is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else if file_type.is_symlink() {
            let target = fs::read_link(entry.path())?;
            symlink(target, dest_path)?;
        } else {
            fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE FIX, EXECUTED. `directory_seed_scan.rs` pins WHY this filter must
    /// exist, by reproducing the round's EISDIR through the scanner; this
    /// runs the filter itself. Reverting it fails here while those stay
    /// green either way, which is the contrast that says the coverage is
    /// real - three defects have been repaired in this tree's placement code
    /// and none was caught by a test that ran the code.
    ///
    /// INLINE RATHER THAN UNDER `tests/`, because `subtool` is a private
    /// module and widening it to admit a test would ship a wider API than
    /// the code needs. Same price either way: both are `crates/nix-ninja`.
    ///
    /// WHAT THIS DOES NOT PIN, said plainly because a green run reads like
    /// more than it is: that `discover_dynamic_dependencies` calls
    /// `scan_seeds` at all. Replacing that one call with
    /// `built_paths.keys()` leaves this green. Covering it needs a task
    /// driven through the dynamic path, which needs a daemon.
    #[test]
    fn scan_seeds_drops_a_directory_output_and_keeps_the_files() {
        let d = std::env::temp_dir().join(format!(
            "nn-seeds-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dir_out = d.join("ninja-build-include-kernel-abi/include/kernel-abi");
        std::fs::create_dir_all(&dir_out).unwrap();
        let file_out = d.join("ninja-build-verbs/verbs.h");
        std::fs::create_dir_all(file_out.parent().unwrap()).unwrap();
        std::fs::write(&file_out, b"#pragma once\n").unwrap();

        // THE BUILD-SIDE PATHS ARE DELIBERATELY NOT CREATED ON DISK, and
        // that is the test's sharpest property rather than an oversight.
        // It is what makes a filter written on the WRONG SIDE fail here:
        // `!build_path.is_dir()` keeps both entries, because neither build
        // path exists. Creating them to make the fixture look realistic
        // silently destroys that discrimination.
        let mut built_paths = HashMap::new();
        built_paths.insert(d.join("build/include/kernel-abi"), dir_out);
        built_paths.insert(d.join("build/verbs.h"), file_out);

        let seeds = scan_seeds(&built_paths);

        // BOTH HALVES. Dropping everything would also drop the directory,
        // and a scan seeded with nothing declares nothing - which is the
        // silent failure this class produces one phase later.
        assert_eq!(seeds.len(), 1, "exactly the file survives: {seeds:?}");
        assert!(
            seeds[0].ends_with("verbs.h"),
            "the real header must be kept: {seeds:?}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }
}
