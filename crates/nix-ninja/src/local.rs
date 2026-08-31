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
    // is materialized as a rewritten copy. The first version symlinked
    // everything and THEN replaced the placeholder-carrying ones - a window
    // in which a parallel consumer reads the placeholder. Restore-needing
    // outputs are now written to a temp name and renamed into place, so
    // every visible state is post-restore; everything else stays a
    // symlink, created afterwards.
    // THE WINDOW WAS REAL AND WAS NOT bison's CAUSE. This comment first
    // credited the temp-and-rename with fixing bison's 698-of-744
    // installcheck failure (PKGDATADIR left as the placeholder); the
    // failure recurred 2026-08-24 with the window closed, because a slim
    // LTO object carries its literals inside compressed, checksummed IR
    // that no byte rewrite can reach - measured both ways in
    // task.rs::cmdline_is_lto. LTO compile tasks now never see a
    // placeholder; this restore stays correct for everything else.
    //
    // THIS PATH IS LOCAL MODE ONLY, AND THAT IS A GAP RATHER THAN A CHOICE.
    // The rewrite map is built from `$out` and `outputs`, which exist only
    // when the driver runs INSIDE the outer derivation - and there the
    // artifact is handed to the consumer as a derivation output, so nothing
    // ever holds its bytes to rewrite. An output carrying the placeholder is
    // therefore correct here and shipped as-is there. The LTO carve-out above
    // says an LTO task's output cannot be restored; that is true of every
    // output on the output-derivation path, and this code read as though the
    // restore covered it. It needs a design answer, not a patch.
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
                std::fs::write(&tmp, rewritten)?;
                // NOT the store's permissions: a store file is read-only and
                // this lands in the build tree, where a later step may have
                // to replace it.
                if let Ok(md) = std::fs::metadata(&tmp) {
                    let mut perms = md.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = std::fs::set_permissions(&tmp, perms);
                }
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

/// UPSTREAM #17: materialize collected depfiles as COPIES, not symlinks.
///
/// `symlink_derived_files` is right for build products - a symlink into the
/// store costs nothing and the store path is the identity. It is WRONG for a
/// depfile, and getting that wrong made the whole read-back inert for a day:
/// nix gives every store file mtime 1, `fs::metadata` follows the symlink, so
/// `depfile_read_back`'s freshness guard saw a file older than every source
/// and fell back to the scan on every run. The collection ran, printed its
/// count, and bought nothing.
///
/// A copy lands with the current mtime, which is what makes the guard mean
/// what it says. Best effort per file: a depfile that fails to copy costs a
/// scan next run, which is the behavior that has always been in force.
pub fn copy_derived_files(
    rpc_client: &BuilderRpcClient,
    store_dir: &StoreDir,
    prefix: &Path,
    derived_files: &[DerivedFile],
) -> Result<usize> {
    let derived_paths: Vec<_> = derived_files
        .iter()
        .map(|df| df.derived_path.clone())
        .collect();
    let store_paths = rpc_client.build_paths(store_dir, &derived_paths)?;

    let mut copied = 0usize;
    for (df, store_path) in derived_files.iter().zip(store_paths.iter()) {
        let mut src = store_path.to_absolute_path(store_dir);
        if let Some(rel) = &df.rel_path {
            src = src.join(rel);
        }
        if !src.is_file() {
            continue;
        }
        let dest = prefix.join(&df.build_path);
        if let Some(parent) = dest.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        if copy_over(&src, &dest).is_err() {
            continue;
        }
        copied += 1;
    }
    Ok(copied)
}

/// Copy `src` over `dest`, replacing whatever is there, and leave the result
/// WRITABLE and a REGULAR FILE.
///
/// Both properties are the point and neither is incidental:
///
/// - a previous run leaves a SYMLINK into the store at this path, and
///   `fs::copy` onto a symlink writes THROUGH it, into a read-only store
///   file. Remove first.
/// - store files are read-only and a copy inherits the mode, so without the
///   permission fix the next run cannot replace it in place.
/// - the result must not itself be a symlink, because the whole reason
///   depfiles are copied is that `fs::metadata` follows a symlink into the
///   store and reads mtime 1, which the freshness guard treats as older than
///   every source. That made the feature inert for a day.
fn copy_over(src: &Path, dest: &Path) -> std::io::Result<()> {
    if dest.is_symlink() || dest.exists() {
        let _ = std::fs::remove_file(dest);
    }
    std::fs::copy(src, dest)?;
    if let Ok(md) = std::fs::metadata(dest) {
        let mut perms = md.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(dest, perms);
    }
    Ok(())
}

#[cfg(test)]
mod copy_over_tests {
    use super::copy_over;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "nn-copyover-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// THE REGRESSION. Reverting to a symlink is what made the depfile
    /// read-back inert: a symlinked destination reads its TARGET's mtime.
    #[test]
    fn the_destination_is_a_real_file_not_a_link() {
        let d = tmp("real");
        let src = d.join("src.d");
        std::fs::write(&src, b"a.o: a.c\n").unwrap();
        let dest = d.join("out.d");
        copy_over(&src, &dest).unwrap();
        assert!(
            !dest.is_symlink(),
            "a symlink here reads the target's mtime"
        );
        assert!(dest.is_file());
        assert_eq!(std::fs::read(&dest).unwrap(), b"a.o: a.c\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A previous run's symlink into a read-only store must be REPLACED, not
    /// written through - writing through it fails, or worse, succeeds against
    /// something that is not ours.
    #[test]
    fn an_existing_symlink_is_replaced_rather_than_followed() {
        let d = tmp("link");
        let victim = d.join("victim");
        std::fs::write(&victim, b"DO NOT TOUCH").unwrap();
        let dest = d.join("out.d");
        std::os::unix::fs::symlink(&victim, &dest).unwrap();

        let src = d.join("src.d");
        std::fs::write(&src, b"new").unwrap();
        copy_over(&src, &dest).unwrap();

        assert!(!dest.is_symlink());
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"DO NOT TOUCH",
            "the symlink target must be untouched"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// A copy of a read-only source must be replaceable next run.
    #[test]
    fn the_copy_is_writable_even_when_the_source_is_not() {
        let d = tmp("perm");
        let src = d.join("src.d");
        std::fs::write(&src, b"x").unwrap();
        let mut p = std::fs::metadata(&src).unwrap().permissions();
        p.set_readonly(true);
        std::fs::set_permissions(&src, p).unwrap();

        let dest = d.join("out.d");
        copy_over(&src, &dest).unwrap();
        assert!(!std::fs::metadata(&dest).unwrap().permissions().readonly());
        std::fs::remove_dir_all(&d).ok();
    }
}
