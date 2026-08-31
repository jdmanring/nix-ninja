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

    create_symlinks(prefix, store_dir, opaque_files, true)?;

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
///   file. The rename below replaces the link itself instead.
/// - store files are read-only and a copy inherits the mode, so without the
///   permission fix the next run cannot replace it in place.
/// - the result must not itself be a symlink, because the whole reason
///   depfiles are copied is that `fs::metadata` follows a symlink into the
///   store and reads mtime 1, which the freshness guard treats as older than
///   every source. That made the feature inert for a day.
///
/// WRITTEN TO A SIBLING AND RENAMED, because a copy that fails part way
/// through is worse here than one that does not happen at all. `fs::copy`
/// writes in place, so a failure mid-write left a TRUNCATED depfile at the
/// destination carrying a fresh mtime, and the freshness guard then preferred
/// it to the scan: the next run compiled that unit against a short input list,
/// header edits stopped invalidating it, and the object was quietly wrong.
/// A rename either happens or does not.
///
/// The rename also subsumes the symlink case. Renaming over a symlink
/// replaces the link itself, where a copy would have written through it into
/// a read-only store file.
fn copy_over(src: &Path, dest: &Path) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = dest
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "depfile".to_string());
    // Unique per process AND per call: two restores of different outputs in
    // one directory must not share a temp.
    let tmp = dest.with_file_name(format!(".{name}.nn-tmp.{}.{n}", std::process::id()));

    let copied = std::fs::copy(src, &tmp).and_then(|_| {
        if let Ok(md) = std::fs::metadata(&tmp) {
            let mut perms = md.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(&tmp, perms);
        }
        std::fs::rename(&tmp, dest)
    });
    if copied.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    copied
}

#[cfg(test)]
mod copy_over_tests {
    use super::copy_over;

    /// THE REGRESSION. A copy that fails must leave what was there. The
    /// previous form removed the destination and then wrote in place, so a
    /// failure part way through left either nothing or a truncated file
    /// carrying a fresh mtime - and a fresh mtime is exactly what makes the
    /// freshness guard prefer a depfile to the scan.
    ///
    /// The failure is induced by handing it a directory as the source, which
    /// `fs::copy` refuses.
    #[test]
    fn a_failed_copy_leaves_the_destination_untouched() {
        let d = std::env::temp_dir().join(format!("nn-cov-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let dest = d.join("main.o.d");
        std::fs::write(&dest, b"main.o: main.c header.h\n").unwrap();
        let bad_src = d.join("a-directory");
        std::fs::create_dir_all(&bad_src).unwrap();

        assert!(copy_over(&bad_src, &dest).is_err());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "main.o: main.c header.h\n"
        );
        // and no temp is left behind
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("nn-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
        std::fs::remove_dir_all(&d).ok();
    }

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
