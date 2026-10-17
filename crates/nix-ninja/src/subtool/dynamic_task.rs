use anyhow::{anyhow, Result};
use harmonia_store_derivation::derivation::Derivation;
use harmonia_store_derivation::derived_path::{OutputName, SingleDerivedPath};
use harmonia_store_path::{StoreDir, StorePath};
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::{split_encoded_list, DerivedFile, ENCODED_LIST_SEP};
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use crate::task::{discover_c_includes, encoded_build_path};

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
    let derived_files: Vec<DerivedFile> = split_encoded_list(&inputs)
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
/// THE ROOT FIX LANDED AT `7b60645` AND THIS PARAGRAPH SPENT LONGER SAYING
/// OTHERWISE THAN THE FIX TOOK. `canonicalize_cached` carries `virtual_hit`,
/// which refuses a directory at BOTH bypasses, the direct probe and the
/// lexically normalized one below it, so every route is covered at the
/// resolution point and any further probe added to that function inherits
/// the refusal by construction. The text here described the fix as declined
/// on fileset cost and told a reader to land it with the next batch, which
/// is a state claim in a place that is read far more often than it is
/// revised: the state changed, the paragraph did not, and the next reader
/// pays for the analysis twice.
///
/// Extracted rather than written inline so a test can execute it. Three
/// defects have been repaired in this tree's placement code and none was
/// covered by a test that ran it.
pub fn scan_seeds(built_paths: &HashMap<PathBuf, PathBuf>) -> Vec<PathBuf> {
    built_paths
        .iter()
        .filter(|(_, store_path)| seed_readable(store_path))
        .map(|(build_path, _)| build_path.clone())
        .collect()
}

/// A seed must be OPENABLE AS A FILE, and `is_dir()` is not that predicate.
///
/// An ALIAS OUTPUT IS STORED AS LINK TEXT (`alias_link_target`): the .so a
/// versioned-library edge declares alongside its `libx.so.36.2.0` sibling is
/// its own store object holding only a relative link, because each declared
/// output is a separate realisation and the sibling lives in a DIFFERENT
/// object. Realised on its own, the link dangles. protobuf 36.2's four
/// protoc link edges each take `libutf8_validity.so` as a built input, the
/// filter passed it exactly as it passes a real file, and the walk died
///
/// ```text
/// Failed to read file <store>-ninja-build-third_party-utf8_range-
///   libutf8_validity.so/third_party/utf8_range/libutf8_validity.so:
///   No such file or directory (os error 2)
/// ```
///
/// 4 task(s) failed under keep-going, KDE round at pin fe4e173, 2026-10-10.
/// Same class as the directory seed above, one predicate over: the guard
/// asks the shape and not whether the walk can open it.
///
/// `try_exists()` follows the link, so a dangling one is `Ok(false)` here
/// rather than an error, and an existing real file is `Ok(true)`. The
/// "does not exist yet" case below the directory guard in `scan_seeds`'s
/// sibling (task.rs's static seed filter) does not apply HERE: every entry
/// of this map is a REALISED store object, so nothing is pending.
fn seed_readable(store_path: &Path) -> bool {
    !store_path.is_dir() && store_path.try_exists().unwrap_or(false)
}

/// A FILE INSIDE A REALISED DIRECTORY OUTPUT IS REACHABLE ONLY THROUGH ITS
/// OWN KEY. The map holds one entry per declared output, so a directory
/// output answers for its own path and nothing under it, and an include
/// naming a file inside it falls through to the disk, where the directory
/// has not been placed. qtbase 6.11.2: `mocs_compilation.cpp` includes
/// `MVTLVZ3TR3/moc_qxcbglintegrationplugin.cpp` from the
/// `XcbQpaPrivate_autogen` tree, the moc file was never read, and the
/// source header it includes went undeclared.
///
/// Adds one entry per regular file (or link) under each directory value,
/// keyed by its build path. Seeds are taken BEFORE this runs, so the walk
/// still reads only what an include reaches. An existing key is kept: an
/// individually declared output is authoritative over the tree's copy,
/// the order `create_symlinks_with` places them in.
pub fn expand_directory_outputs(built_paths: &mut HashMap<PathBuf, PathBuf>) {
    fn walk(build: &Path, store: &Path, out: &mut Vec<(PathBuf, PathBuf)>) {
        let Ok(entries) = fs::read_dir(store) else {
            return;
        };
        for e in entries.flatten() {
            let (b, s) = (build.join(e.file_name()), e.path());
            if s.is_dir() {
                walk(&b, &s, out);
            } else {
                out.push((b, s));
            }
        }
    }
    let mut extra = Vec::new();
    for (build, store) in built_paths.iter() {
        if store.is_dir() {
            walk(build, store, &mut extra);
        }
    }
    for (b, s) in extra {
        built_paths.entry(b).or_insert(s);
    }
}

/// The seeds and the resolution map one discovery reads, in that order:
/// seeds from the declared outputs alone, then the map widened to the
/// files inside directory outputs. A function so the order is tested.
pub fn scan_inputs(
    built_paths: HashMap<PathBuf, PathBuf>,
) -> (Vec<PathBuf>, HashMap<PathBuf, PathBuf>) {
    let files = scan_seeds(&built_paths);
    let mut built_paths = built_paths;
    expand_directory_outputs(&mut built_paths);
    (files, built_paths)
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

    let (files, built_paths) = scan_inputs(built_paths);

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
    let mut input_set: HashSet<String> = split_encoded_list(drv_inputs)
        .map(|s| s.to_string())
        .collect();

    // ONE BUILD PATH, ONE CLAIMANT, ON THE MERGE PATH TOO. The set above
    // dedupes whole ENCODED entries, so a second upload of one file under a
    // different store path is added beside the first and the task dies in
    // sandbox setup: "Two different files claim one build path." The pass
    // that exists for this in `build_task_derivation` groups by build path,
    // but it runs over the STATIC input set and never sees this merge, so
    // local mode (the compiler route) has no guard at all.
    //
    // Witnessed on virglrenderer 1.3.0: an LTO task's `config.h` is swapped
    // for a RAW re-upload naming the real outer output, discovery then
    // re-uploads the same file through the placeholder rewrite, and the two
    // spellings of one content reach one task. Both entries carry the
    // identical build path `config.h`, which is what says the collision is
    // here and not in the grouping key.
    //
    // THE EXISTING CLAIMANT WINS WHERE THERE IS ONE, and it is the only
    // available order: this function holds no rpc client, so it cannot
    // re-read the file the way the static pass does, and on an LTO task the
    // declared input was chosen deliberately over the rewritten one. A file
    // that really changed mid-scan therefore keeps the older bytes here
    // rather than failing, which is what the static pass would call stale.
    //
    // WHERE BOTH CLAIMANTS ARE OFFERS there is no declared input to prefer
    // and the first the scan emitted wins, which is arbitrary but total.
    // Said explicitly because the sentence above does not cover it, and a
    // reader would otherwise take a guarantee this site cannot give.
    //
    // `discovered_store_paths` needs no seat here: an include resolving
    // inside the store takes that branch and returns, so the two vectors
    // are disjoint, and such an input carries no build path to collide on.
    //
    // THE ONE LEGITIMATE COLLISION INSIDE `discovered_deps` IS SAFE AND WAS
    // CHECKED RATHER THAN ASSUMED, because dropping it would take nss's
    // staged headers with it. A header found inside the outer output is
    // re-staged with a COMPOSED build path under `.nn-outer`, and the same
    // header can also be scanned at that mirrored path on disk. Both then
    // claim one build path under two store paths, since the upload name
    // comes from the path asked about, so before this guard such a task
    // refused with exactly the message above. The bytes are the same file,
    // so keeping either is correct.
    let mut claimed: HashSet<String> = input_set
        .iter()
        .map(|e| encoded_build_path(e).to_string())
        .collect();

    let mut new_deps = Vec::new();
    for derived_file in discovered_deps {
        let encoded = derived_file.to_encoded(store_dir);

        // Skip if already in input set
        if input_set.contains(&encoded) {
            continue;
        }

        let bp = encoded_build_path(&encoded).to_string();
        if !claimed.insert(bp.clone()) {
            eprintln!(
                "nix-ninja: {bp} is already claimed by a declared input; keeping that one and dropping the discovered upload"
            );
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
            inputs.join(ENCODED_LIST_SEP).into_bytes().into(),
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

    /// THE SECOND MEMBER OF THE CLASS, EXECUTED. An alias output's store
    /// object is a DANGLING LINK (link text naming a sibling in a different
    /// object), so `is_dir()` passes it and the walk dies ENOENT reading
    /// it - protobuf 36.2's four protoc link edges, 2026-10-10. Reverting
    /// `seed_readable` fails here while the directory test above stays
    /// green, which is what says this arm is real and not a restatement.
    ///
    /// THE LINK IS CREATED DANGLING ON PURPOSE and points at a sibling
    /// that exists NOWHERE in the fixture - not in another store object,
    /// not in the temp dir. Making it resolvable here would pass both the
    /// old and new filters and the test would assert nothing, the fake
    /// pass this corpus has shipped before.
    #[test]
    fn scan_seeds_drops_an_alias_output_whose_link_dangles() {
        let d = std::env::temp_dir().join(format!(
            "nn-seeds-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // The alias: a store object holding only the link text.
        let alias_out = d.join("ninja-build-third_party-utf8_range-libutf8_validity.so/third_party/utf8_range/libutf8_validity.so");
        std::fs::create_dir_all(alias_out.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("libutf8_validity.so.36.2.0", &alias_out).unwrap();
        let file_out = d.join("ninja-build-verbs/verbs.h");
        std::fs::create_dir_all(file_out.parent().unwrap()).unwrap();
        std::fs::write(&file_out, b"#pragma once\n").unwrap();

        let mut built_paths = HashMap::new();
        built_paths.insert(
            d.join("build/third_party/utf8_range/libutf8_validity.so"),
            alias_out,
        );
        built_paths.insert(d.join("build/verbs.h"), file_out);

        let seeds = scan_seeds(&built_paths);

        assert_eq!(seeds.len(), 1, "exactly the real file survives: {seeds:?}");
        assert!(
            seeds[0].ends_with("verbs.h"),
            "the real header must be kept: {seeds:?}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// ONE BUILD PATH REACHES THE MERGED TASK UNDER ONE STORE PATH.
    ///
    /// The fixture is the real pair, from a round that failed 110 tasks on
    /// it: `config.h` declared as the RAW re-upload naming virglrenderer's
    /// own outer output, and discovery offering the same build path as the
    /// upload carrying the outer-output placeholder. Both spell the build
    /// path `config.h`, so the collision is not a second spelling and the
    /// grouping key in `build_task_derivation`'s pass is not the defect;
    /// that pass never ran, because it does not cover this merge.
    ///
    /// THE SECOND ASSERTION IS THE CONTROL. A function that added nothing
    /// would satisfy the first, and that is the failure mode this merge
    /// already has in the other direction, so an unclaimed build path has
    /// to arrive in the same call.
    #[test]
    fn a_claimed_build_path_takes_no_second_upload() {
        let store_dir = StoreDir::new(std::path::Path::new("/nix/store")).unwrap();
        let mut drv = Derivation::new(
            "ninja-build".parse().unwrap(),
            b"x86_64-linux"[..].into(),
            b"/nn-task/bin/nix-ninja-task"[..].into(),
        );

        let raw = "/nix/store/hz96sb8291vrix4ln92a6jbacypqh8qb-config.h";
        drv.env.insert(
            b"NIX_NINJA_INPUTS"[..].into(),
            format!("{raw}:config.h:").into_bytes().into(),
        );

        let offer = |path: &str, bp: &str| DerivedFile {
            derived_path: SingleDerivedPath::Opaque(store_dir.parse(path).unwrap()),
            build_path: PathBuf::from(bp),
            rel_path: None,
        };
        let rewritten = offer(
            "/nix/store/f6m77zjnky9zy1vsiwfz3d9886x4pn3b-config.h",
            "config.h",
        );
        let fresh = offer(
            "/nix/store/fixs6b76qaj5m5h3xbyjzkgwlqcd5480-prog.h",
            "src/prog.h",
        );

        let new_deps = update_derivation_with_discoveries(
            &mut drv,
            vec![rewritten.clone(), fresh.clone()],
            Vec::new(),
            &store_dir,
        )
        .unwrap();

        assert!(
            !drv.inputs.contains(&rewritten.derived_path),
            "the second claimant of config.h must not become an input"
        );
        assert!(
            drv.inputs.contains(&fresh.derived_path),
            "an unclaimed build path must still be added: {:?}",
            drv.inputs
        );
        assert_eq!(new_deps.len(), 1, "only the unclaimed offer is a new dep");

        let emitted = std::str::from_utf8(
            drv.env
                .iter()
                .find(|(k, _)| k.as_ref() == b"NIX_NINJA_INPUTS")
                .map(|(_, v)| v.as_ref())
                .expect("NIX_NINJA_INPUTS"),
        )
        .unwrap()
        .to_owned();
        let claimants = split_encoded_list(&emitted)
            .filter(|e| encoded_build_path(e) == "config.h")
            .count();
        assert_eq!(claimants, 1, "one claimant of config.h survives: {emitted}");
        assert!(
            emitted.contains(raw),
            "the declared claimant is the one kept: {emitted}"
        );
    }

    /// TWO OFFERS AND NO DECLARED INPUT, which the case above cannot reach
    /// and the guard's comment claims nothing about beyond totality. The
    /// merge has no bytes to compare and no client to re-read with, so the
    /// first offer wins; what matters is that exactly one survives.
    #[test]
    fn two_offers_for_one_build_path_resolve_to_one() {
        let store_dir = StoreDir::new(std::path::Path::new("/nix/store")).unwrap();
        let mut drv = Derivation::new(
            "ninja-build".parse().unwrap(),
            b"x86_64-linux"[..].into(),
            b"/nn-task/bin/nix-ninja-task"[..].into(),
        );

        let offer = |path: &str| DerivedFile {
            derived_path: SingleDerivedPath::Opaque(store_dir.parse(path).unwrap()),
            build_path: PathBuf::from("config.h"),
            rel_path: None,
        };
        let first = offer("/nix/store/f6m77zjnky9zy1vsiwfz3d9886x4pn3b-config.h");
        let second = offer("/nix/store/hz96sb8291vrix4ln92a6jbacypqh8qb-config.h");

        let new_deps = update_derivation_with_discoveries(
            &mut drv,
            vec![first.clone(), second.clone()],
            Vec::new(),
            &store_dir,
        )
        .unwrap();

        assert_eq!(new_deps.len(), 1, "exactly one offer survives");
        assert!(
            drv.inputs.contains(&first.derived_path) && !drv.inputs.contains(&second.derived_path),
            "the first offer is the one kept: {:?}",
            drv.inputs
        );
    }

    /// THE TRANSITIVE HALF OF THE KEYING KNOB, and it is the only property
    /// that makes a dynamic task derivation free of the task binary's store
    /// path. `build_dynamic_task_derivation` emits the dynamic derivation
    /// with `driver_builder_path` and embeds a plain derivation already
    /// emitted through `task_builder_path`; this function is what re-reads
    /// that plain derivation inside the sandbox, and the knob's coverage
    /// depends on it leaving the builder alone rather than re-deriving it.
    ///
    /// WRITTEN AFTER A RETRACTION, which is why it exists at all: the
    /// dynamic class was recorded as UNCOVERED on the strength of a store
    /// search whose corpus predated the knob and could only return zero.
    /// The property is established by reading, and a read is exactly what
    /// a later edit does not repeat. A mutant reassigning `drv.builder`
    /// here fails this and nothing else in the tree.
    #[test]
    fn discoveries_do_not_touch_the_builder() {
        let store_dir = StoreDir::new(std::path::Path::new("/nix/store")).unwrap();
        let builder = b"/nn-task/bin/nix-ninja-task";
        let mut drv = Derivation::new(
            "ninja-build".parse().unwrap(),
            b"x86_64-linux"[..].into(),
            builder[..].into(),
        );

        let discovered: StorePath = store_dir
            .parse("/nix/store/fixs6b76qaj5m5h3xbyjzkgwlqcd5480-prog.c")
            .unwrap();
        let new_deps = update_derivation_with_discoveries(
            &mut drv,
            Vec::new(),
            vec![discovered.clone()],
            &store_dir,
        )
        .unwrap();

        // THE CONTROL, and without it the builder assertion is satisfied by
        // a function that returned early and did nothing. The discovered
        // store path must have landed as an input.
        assert!(new_deps.is_empty(), "no derived files were offered");
        assert!(
            drv.inputs.contains(&SingleDerivedPath::Opaque(discovered)),
            "the discovered store path must become an input: {:?}",
            drv.inputs
        );
        assert_eq!(
            drv.builder.as_ref(),
            &builder[..],
            "the embedded plain derivation keeps the builder it was emitted with"
        );
    }

    fn automoc_scratch() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("nn-automoc-{}-{nanos}", std::process::id()));
        fs::create_dir(&p).unwrap();
        p.canonicalize().unwrap()
    }

    /// A MOC FILE INSIDE A REALISED DIRECTORY OUTPUT CLIMBS BACK TO ITS SOURCE.
    ///
    /// qtbase 6.11.2, `XcbQpaPrivate_autogen/mocs_compilation.cpp`, the KDE round
    /// of 2026-10-17: AUTOMOC writes the moc files into hash-named directories of
    /// the autogen tree, and each includes its source header by a climb out of
    /// the build tree:
    ///
    /// ```text
    /// mocs_compilation.cpp:        #include "MVTLVZ3TR3/moc_qxcbglintegrationplugin.cpp"
    /// moc_qxcbglintegrationplugin: #include "../../../../../../../src/plugins/platforms/xcb/gl_integrations/qxcbglintegrationplugin.h"
    /// ```
    ///
    /// Discovery missed both steps. The tree is ONE map entry, so the moc file
    /// under it fell through to a disk the tree was never placed on
    /// (`expand_directory_outputs`), and the climb was resolved physically
    /// through a directory that exists only in the realised output, where the
    /// sandbox has it real (the lexical probe in `canonicalize_cached`). The
    /// header went undeclared and the compile died on it.
    ///
    /// The fixture builds its own "store" tree; the autogen build path is absent
    /// from the build directory, which is the state discovery runs in.
    #[test]
    fn a_moc_climb_out_of_a_realised_tree_declares_its_source_header() {
        let t = automoc_scratch();
        let src = t.join("q/src/gl");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("plugin.h"), "").unwrap();
        let build = t.join("q/build");
        fs::create_dir_all(&build).unwrap();

        // The realised outputs, each in its own object as the store holds them.
        let store_tree = t.join("store/ag");
        fs::create_dir_all(store_tree.join("MVTLVZ3TR3")).unwrap();
        // MVTLVZ3TR3 -> ag -> a -> build -> q
        fs::write(
            store_tree.join("MVTLVZ3TR3/moc_plugin.cpp"),
            "#include \"../../../../src/gl/plugin.h\"\n",
        )
        .unwrap();
        let store_mc = t.join("store/mc/mocs_compilation.cpp");
        fs::create_dir_all(store_mc.parent().unwrap()).unwrap();
        fs::write(&store_mc, "#include \"MVTLVZ3TR3/moc_plugin.cpp\"\n").unwrap();

        let ag = build.join("a/ag");
        let mc = ag.join("mocs_compilation.cpp");
        let mut built = HashMap::new();
        built.insert(mc.clone(), store_mc);
        built.insert(ag.clone(), store_tree);
        let (seeds, map) = scan_inputs(built);
        assert_eq!(seeds, vec![mc.clone()], "a directory output became a seed");

        let cmd = format!("g++ -I{} -c {}", src.display(), mc.display());
        let scan =
            deps_infer::c_include_parser::retrieve_c_includes_checked(&cmd, seeds, Some(map))
                .unwrap();
        let _ = fs::remove_dir_all(&t);

        assert!(
            scan.includes.contains(&src.join("plugin.h")),
            "the moc file's source header was not declared; includes: {:?}",
            scan.includes
        );
    }
}
