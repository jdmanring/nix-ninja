use anyhow::Result;
use harmonia_store_derivation::derived_path::SingleDerivedPath;
use harmonia_store_path::StoreDir;
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
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

static RESTORE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

    // PLACEHOLDER -> REAL OUTER PATH, by COPY, and NEVER exposed as the
    // placeholder first. A task output that names an outer output path
    // carries the placeholder (task.rs outer_rewrite_map); the build tree
    // needs the real one, and a store file is read-only, so such an output
    // is materialized as a rewritten copy. Symlinking everything and THEN
    // replacing the placeholder-carrying ones leaves a window in which a
    // parallel consumer reads the placeholder, so restore-needing outputs
    // are written to a temp name and renamed into place: every visible
    // state is post-restore. Everything else stays a symlink.
    //
    // THIS PATH IS LOCAL MODE ONLY, AND THAT IS A GAP RATHER THAN A CHOICE.
    // The reasoning here previously ended at "nothing ever holds its bytes to
    // rewrite", which is true of the driver and does not make the result
    // correct: the consumer reads the task output through `builtins.outputOf`
    // and receives whatever the task wrote, placeholder included. Measured on
    // a store holding both kinds: of thirty `gcc-16.2.0` outputs, seventeen
    // carry the placeholder inside `cc1`, `cc1plus`, `lto1` and `collect2`,
    // in the compiler's built-in include search paths. Those builds succeed
    // only because the wrapper passes the same directories explicitly, so the
    // corruption is masked rather than absent.
    //
    // Rewriting to the outer output path would not fix it, because that is
    // not the path the consumer resolves; an artifact naming its own final
    // location is self-referential under content addressing. It needs a
    // design answer rather than a patch, and `submit_outer_output` now says
    // so out loud instead of shipping quietly.
    let restore = crate::task::outer_restore_map();
    let mut symlink_files: Vec<DerivedFile> = Vec::new();
    for (df, store_path) in opaque_files.iter().zip(store_paths.iter()) {
        let target = store_path.to_absolute_path(store_dir);
        let mut restored = false;
        // A derived file with a `rel_path` points INTO a store directory, so
        // the store path itself is not a file and the restore was skipped;
        // build_derived_files joins it for exactly this reason.
        let target = match &df.rel_path {
            Some(rel) => target.join(rel),
            None => target,
        };
        if !restore.is_empty() && target.is_file() {
            let data = std::fs::read(&target)?;
            if let Some(rewritten) = crate::task::rewrite_bytes(&data, &restore) {
                let dest = prefix.join(&df.build_path);
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                // UNIQUE PER CALL. `with_extension` REPLACES the extension,
                // so `a.o` and `a.c` both became `a.nn-restore-tmp` and two
                // restores in one directory raced on one temp file.
                let tmp = dest.with_file_name(format!(
                    ".{}.nn-restore-tmp.{}.{}",
                    dest.file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "out".into()),
                    std::process::id(),
                    RESTORE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                ));
                write_restored(&tmp, &rewritten, &target)?;
                if dest.is_symlink() || dest.exists() {
                    let _ = std::fs::remove_file(&dest);
                }
                std::fs::rename(&tmp, &dest)?;
                restored = true;
            }
        }
        if !restored {
            symlink_files.push(df.clone());
        }
    }

    create_symlinks(prefix, store_dir, symlink_files, true)?;

    Ok(())
}

/// Write a restored copy, carrying the store object's mode PLUS owner write.
///
/// A union, not a swap, and both halves were wrong on their own. The store's
/// mode alone leaves a read-only file in a writable build tree, so a later
/// step cannot replace it. A fresh file's mode alone drops the executable
/// bit: `fs::write` creates 0666 and clearing read-only only ORs write bits
/// in, so a generated script carrying an outer output path lands
/// non-executable.
fn write_restored(tmp: &Path, data: &[u8], target: &Path) -> std::io::Result<()> {
    std::fs::write(tmp, data)?;
    if let Ok(md) = std::fs::metadata(target) {
        let mode = md.permissions().mode() | 0o200;
        let _ = std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

#[cfg(test)]
mod write_restored_tests {
    use super::write_restored;
    use std::os::unix::fs::PermissionsExt;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nn-wr-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mode_of(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// THE REGRESSION. A store object is read-only, and an executable one is
    /// 0555. The restored copy must still be executable, or a generated
    /// script lands unrunnable.
    #[test]
    fn an_executable_store_object_stays_executable() {
        let d = dir();
        let target = d.join("gen.sh");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o555)).unwrap();
        let tmp = d.join("gen.sh.tmp");
        write_restored(&tmp, b"#!/bin/sh\n", &target).unwrap();
        assert_eq!(mode_of(&tmp), 0o755, "executable bit lost");
        std::fs::remove_dir_all(&d).ok();
    }

    /// And the half that was there first: the result must be writable, or the
    /// next run cannot replace it in the build tree.
    #[test]
    fn a_read_only_store_object_becomes_writable() {
        let d = dir();
        let target = d.join("config.h");
        std::fs::write(&target, b"x").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        let tmp = d.join("config.h.tmp");
        write_restored(&tmp, b"x", &target).unwrap();
        assert_eq!(mode_of(&tmp), 0o644);
        std::fs::remove_dir_all(&d).ok();
    }
}
