use anyhow::Result;
use harmonia_store_derivation::derived_path::SingleDerivedPath;
use harmonia_store_path::StoreDir;
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn build_derived_files(
    rpc_client: &BuilderRpcClient,
    store_dir: &StoreDir,
    derived_files: &[DerivedFile],
) -> Result<HashMap<PathBuf, PathBuf>> {
    let derived_paths: Vec<_> = derived_files
        .iter()
        .map(|df| df.derived_path.clone())
        .collect();

    // Build derived paths so the Nix store paths exist on the host.
    let store_paths = rpc_client.build_paths(store_dir, &derived_paths)?;

    // Create mapping from build_path to actual store path
    let built_paths: HashMap<PathBuf, PathBuf> = derived_files
        .iter()
        .zip(store_paths.iter())
        .map(|(df, store_path)| {
            let actual_path = if let Some(rel_path) = &df.rel_path {
                store_path.to_absolute_path(store_dir).join(rel_path)
            } else {
                store_path.to_absolute_path(store_dir)
            };
            (df.build_path.clone(), actual_path)
        })
        .collect();

    Ok(built_paths)
}

pub fn symlink_derived_files(
    rpc_client: &BuilderRpcClient,
    store_dir: &StoreDir,
    prefix: &Path,
    derived_files: &[DerivedFile],
) -> Result<()> {
    let derived_paths: Vec<_> = derived_files
        .iter()
        .map(|df| df.derived_path.clone())
        .collect();
    let store_paths = rpc_client.build_paths(store_dir, &derived_paths)?;

    // Create new DerivedFiles with opaque store paths instead of placeholders
    let opaque_files: Vec<DerivedFile> = derived_files
        .iter()
        .zip(store_paths.iter())
        .map(|(df, store_path)| DerivedFile {
            derived_path: SingleDerivedPath::Opaque(store_path.clone()),
            build_path: df.build_path.clone(),
            rel_path: df.rel_path.clone(),
        })
        .collect();

    create_symlinks(prefix, store_dir, opaque_files.clone(), true)?;

    // PLACEHOLDER -> REAL OUTER PATH, by COPY. A task output that names an
    // outer output path carries the placeholder (task.rs outer_rewrite_map);
    // the build tree needs the real one, and a store file is read-only, so
    // such an output is materialized as a rewritten copy rather than a
    // symlink. Outputs with no placeholder stay symlinks.
    let restore = crate::task::outer_restore_map();
    if !restore.is_empty() {
        for df in &opaque_files {
            let dest = prefix.join(&df.build_path);
            if !dest.is_symlink() {
                continue;
            }
            let Ok(target) = std::fs::read_link(&dest) else { continue };
            if !target.is_file() {
                continue;
            }
            let data = std::fs::read(&target)?;
            if let Some(rewritten) = crate::task::rewrite_bytes(&data, &restore) {
                std::fs::remove_file(&dest)?;
                std::fs::write(&dest, rewritten)?;
                if let Ok(md) = std::fs::metadata(&target) {
                    let _ = std::fs::set_permissions(&dest, md.permissions());
                }
            }
        }
    }

    Ok(())
}
