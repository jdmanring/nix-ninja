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
    // task.rs::scan_lto_flags. LTO compile tasks now never see a
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
                let tmp = unique_sibling(&dest);
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
    create_symlinks(prefix, store_dir, symlink_files.clone(), true)?;
    refresh_placed_mtimes(prefix, store_dir, &symlink_files);

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

/// Give every placed build product a plausible mtime, because `make` stats
/// THROUGH the symlink and nix gives every store file mtime 1.
///
/// A symlink into the store is right for IDENTITY and wrong for anything
/// `make` will stat: the object reads as older than every source, so the
/// whole tree is out of date on every pass. Measured across one round -
/// guile 135 objects compiled ONCE, gnutar 199 compiled TWICE, bison 166
/// compiled FOUR times, uniform within each package and tracking how many
/// passes that package's build makes. guile is the control: a package walked
/// once shows no repetition.
///
/// It is not only wasted work. A target that reads out of date is RELINKED
/// during `installcheck`, so at `-j24` two jobs link it at once and something
/// execs it mid-write - ETXTBSY - while at `-j1` the testsuite runs against a
/// tree `make` is still rebuilding and fails broadly with a perfectly good
/// binary. One cause at two widths.
///
/// SETTING THE SYMLINK'S OWN TIME CANNOT WORK, which is the obvious cheaper
/// fix: `utimensat` with `AT_SYMLINK_NOFOLLOW` sets the link, and `stat`
/// follows. Measured - `lstat` reads the new time and `stat` still reads 1.
///
/// So the bytes are placed instead, by REFLINK where the filesystem supports
/// it: copy-on-write costs no data blocks and lands with a fresh mtime, and
/// the store path stays the identity per-TU resumability rests on. Falls back
/// to a real copy across devices (`EXDEV`) or on a filesystem without it.
///
/// The time must be NOW rather than a sentinel. A sentinel above 1 satisfies
/// the comparison against store sources at mtime 1 and fails against a
/// GENERATED source carrying a real time; `make`'s treatment of equal
/// timestamps also varies, so a fix that lands equal rather than newer takes
/// four passes to two and reads as a partial success. The acceptance test is
/// bison reaching ONE, with guile as the floor.
///
/// Best effort per file: a failure here costs a redundant compile, which is
/// the behavior that has always been in force.
fn refresh_placed_mtimes(prefix: &Path, store_dir: &StoreDir, files: &[DerivedFile]) {
    for df in files {
        let src = df.absolute_path(store_dir);
        if src.is_dir() {
            continue;
        }
        let dest = prefix.join(&df.build_path);
        if !dest.is_symlink() {
            continue;
        }
        // A SYMLINK IS SOMETIMES A NAME RATHER THAN A PRODUCT, and replacing
        // one with content changes what the file MEANS. A versioned shared
        // library is the case that matters: `libfoo.so` and `libfoo.so.1` are
        // aliases of `libfoo.so.1.2.3` pointing at a SIBLING, and turning them
        // into three real files loses the soname relationship, freezes each
        // copy at the bytes it had when the copy was taken, and makes CMake's
        // install-time RPATH rewrite skip them because CMake believes it
        // created a link there.
        //
        // Only a link that RESOLVES TO THE FILE ABOUT TO BE COPIED is one
        // this function placed, and only those are what the mtime problem is
        // about: an object `make` will stat against its source. Asking where
        // the link actually points, rather than trusting the caller's list,
        // makes converting an alias structurally impossible rather than
        // merely unlikely. An alias resolves to a sibling and is skipped.
        if !placement_is_ours(&dest, &src) {
            continue;
        }
        let tmp = unique_sibling(&dest);
        if clone_or_copy(&src, &tmp).is_err() {
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        if let Ok(md) = std::fs::metadata(&src) {
            let mut perm = md.permissions();
            perm.set_mode(perm.mode() | 0o200);
            let _ = std::fs::set_permissions(&tmp, perm);
        }
        if std::fs::rename(&tmp, &dest).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// This symlink is the one this placement created, rather than a name the
/// build put there.
///
/// A versioned shared library is the case that makes the distinction real:
/// `libfoo.so` and `libfoo.so.1` are aliases of `libfoo.so.1.2.3` pointing at
/// a SIBLING. Replacing those with content loses the soname relationship,
/// freezes each copy at the bytes it held when the copy was taken, and makes
/// CMake's install-time RPATH rewrite skip them, because CMake believes it
/// created a link there and a link has no RPATH of its own to rewrite. Three
/// real files of identical size where there should be one file and two links
/// is the signature.
///
/// So the target is asked where it actually points rather than the caller's
/// list being trusted. An alias resolves to a sibling and is left alone.
fn placement_is_ours(dest: &Path, src: &Path) -> bool {
    match (std::fs::canonicalize(dest), std::fs::canonicalize(src)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Reflink `src` to `dest`, falling back to a byte copy.
///
/// `FICLONE` is the same ioctl `cp --reflink` uses. It fails with `EXDEV`
/// across devices and `EOPNOTSUPP` on a filesystem without copy-on-write,
/// and both are ordinary rather than exceptional - the fallback is a plain
/// copy, which is correct and only costs the bytes.
fn clone_or_copy(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    const FICLONE: libc::c_ulong = 0x4004_9409;
    let sf = std::fs::File::open(src)?;
    let df = std::fs::File::create(dest)?;
    let rc = unsafe { libc::ioctl(df.as_raw_fd(), FICLONE, sf.as_raw_fd()) };
    if rc == 0 {
        return Ok(());
    }
    drop(df);
    std::fs::copy(src, dest)?;
    Ok(())
}

#[cfg(test)]
mod write_restored_tests {
    use super::write_restored;
    use std::os::unix::fs::PermissionsExt;

    /// PER TEST, not per process. Both tests removed the shared directory on
    /// the way out, so whichever finished first deleted the other's file and
    /// the suite failed intermittently - only under the full run, because
    /// that is what schedules them together.
    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nn-wr-{tag}-{}", std::process::id()));
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
        let d = dir("exec");
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
        let d = dir("ro");
        let target = d.join("config.h");
        std::fs::write(&target, b"x").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        let tmp = d.join("config.h.tmp");
        write_restored(&tmp, b"x", &target).unwrap();
        assert_eq!(mode_of(&tmp), 0o644);
        std::fs::remove_dir_all(&d).ok();
    }
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

/// A hidden sibling of `dest` to write before renaming into place.
///
/// UNIQUE PER PROCESS AND PER CALL. The first version used `with_extension`,
/// which REPLACES the extension rather than appending, so `a.o` and `a.c`
/// both produced `a.nn-restore-tmp` and two writes into one directory raced
/// on a single file. The name is kept whole and the discriminator appended.
fn unique_sibling(dest: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = dest
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    dest.with_file_name(format!(".{name}.nn-tmp.{}.{n}", std::process::id()))
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
    let tmp = unique_sibling(dest);

    let copied = std::fs::copy(src, &tmp).and_then(|_| {
        // The source's mode plus owner write, which is a union rather than a
        // swap. `set_readonly(false)` ORs in every write bit, so a 0444 store
        // file became 0666 and the depfile landed world-writable.
        let mode = std::fs::metadata(src)?.permissions().mode() | 0o200;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
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
    use std::os::unix::fs::PermissionsExt;

    /// THE MUTANT THE FIRST TEST DID NOT KILL. A plain `fs::copy` onto the
    /// destination, with no temp and no pre-removal, survives a failure
    /// induced at `open` - so it had to be separated from atomicity by a
    /// case only the rename can pass. A store depfile is read-only, and a
    /// previous run leaves one here: copying ONTO it fails with EACCES,
    /// while renaming over it succeeds because the directory is writable.
    #[test]
    fn a_read_only_destination_is_replaced() {
        let d = std::env::temp_dir().join(format!("nn-ro-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let src = d.join("new.d");
        std::fs::write(&src, b"new\n").unwrap();
        let dest = d.join("old.d");
        std::fs::write(&dest, b"old\n").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o444)).unwrap();

        copy_over(&src, &dest).expect("rename must replace a read-only file");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// And it must not land world-writable: `set_readonly(false)` ORs in
    /// every write bit, so a 0444 source produced 0666.
    #[test]
    fn the_copy_is_not_world_writable() {
        let d = std::env::temp_dir().join(format!("nn-ww-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let src = d.join("a.d");
        std::fs::write(&src, b"x").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
        let dest = d.join("b.d");
        copy_over(&src, &dest).unwrap();
        let m = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o644, "expected owner-write only, got {m:o}");
        std::fs::remove_dir_all(&d).ok();
    }

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

/// Trim trailing EMPTY entries from one `OLD_RPATH` value.
///
/// `Some` only when something changed, so a caller can count real edits rather
/// than rewrites that changed nothing. `None` for a value that is entirely
/// empty entries: there is no non-empty sequence left for CMake to match, and
/// blanking it would change the meaning of the check rather than repair it.
fn trim_trailing_empty_rpath_entries(value: &str) -> Option<String> {
    let trimmed = value.trim_end_matches(':');
    if trimmed.is_empty() || trimmed.len() == value.len() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Reconcile CMake's install-time RPATH record with the binaries we actually
/// produce.
///
/// CMake links with a trailing separator - `-Wl,-rpath,/build/.../lib:` is
/// verbatim from a failing build - and that trailing colon is an EMPTY RPATH
/// ENTRY, which the loader reads as the current directory. `assemble_rpath`
/// drops it when it appends sibling-derivation library directories, because an
/// empty entry written into a store output is a current-directory library
/// search rather than fidelity to what was linked.
///
/// `file(RPATH_CHANGE)` then cannot find what it recorded and fails the
/// install, taking the package and everything downstream of it. CMake matches
/// `OLD_RPATH` as a COMPLETE ENTRY SEQUENCE bounded by a colon or the end of
/// the string, so `BASE:` requires an empty entry to still be there.
///
/// Repairing the RECORD rather than the binary is what keeps both properties.
/// Measured against CMake itself, on the binary from a failed build:
///
/// ```text
/// RUNPATH        OLD_RPATH   result
/// BASE::glibc    BASE:       pass    (untouched by us)
/// BASE:glibc     BASE:       FAIL    (what we produce)
/// BASE::glibc    BASE        pass    (untouched, after this trim)
/// BASE:glibc     BASE        pass    (ours, after this trim)
/// ```
///
/// The trimmed form matches BOTH, which is why this is safe to apply to every
/// entry rather than only to the binaries we rewrote - and we cannot tell
/// which those were from here anyway.
///
/// THE INSTALLED RESULT IS BYTE-IDENTICAL TO THE UNTOUCHED PATH, which is the
/// claim that matters and the one that is easy to get wrong. CMake deletes the
/// sequence it matched and writes `NEW_RPATH` in its place, so a shorter
/// `OLD_RPATH` consumes one less separator and the install leaves a LEADING
/// empty entry - another current-directory search, and reason to think this
/// trades one defect for another. It does not: an untouched binary produces
/// exactly the same leading empty entry, because CMake matched `BASE:` there
/// and deleted the same two entries. Measured on the c-ares binary from the
/// failed round, both paths yield the identical RUNPATH string.
///
/// So that residual entry is CMake's, is present for every CMake package built
/// this way, and is removed downstream by the fixup that owns RPATHs in a
/// finished output. It is not this function's to fix, and widening this to
/// touch it would be repairing someone else's contract from inside ours.
pub fn reconcile_cmake_install_rpaths(build_dir: &Path) -> Result<usize> {
    let mut edited = 0usize;
    for entry in walkdir::WalkDir::new(build_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() != "cmake_install.cmake" {
            continue;
        }
        let path = entry.path();
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        for line in text.split_inclusive('\n') {
            match rewritten_old_rpath_line(line) {
                Some(new_line) => {
                    out.push_str(&new_line);
                    changed = true;
                }
                None => out.push_str(line),
            }
        }
        if changed {
            std::fs::write(path, out)?;
            edited += 1;
        }
    }
    Ok(edited)
}

/// One line's rewrite, `Some` only when it changed.
///
/// Matched on the generated spelling rather than by parsing CMake: the file is
/// machine written and always emits `OLD_RPATH "<value>"` on its own line.
fn rewritten_old_rpath_line(line: &str) -> Option<String> {
    let key = "OLD_RPATH \"";
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = start + rest.find('"')?;
    let new_value = trim_trailing_empty_rpath_entries(&line[start..end])?;
    Some(format!("{}{}{}", &line[..start], new_value, &line[end..]))
}

#[cfg(test)]
mod unique_sibling_tests {
    use super::unique_sibling;
    use std::path::Path;

    /// THE REGRESSION. `with_extension` replaced the extension, so two files
    /// differing only in suffix collided on one temp name.
    #[test]
    fn two_names_differing_only_in_extension_do_not_collide() {
        let a = unique_sibling(Path::new("/b/a.o"));
        let b = unique_sibling(Path::new("/b/a.c"));
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("a.o"));
        assert!(b.to_string_lossy().contains("a.c"));
    }

    /// And two calls for the SAME name must differ, or two writes race.
    #[test]
    fn two_calls_for_one_name_do_not_collide() {
        let p = Path::new("/b/a.o");
        assert_ne!(unique_sibling(p), unique_sibling(p));
    }
}

#[cfg(test)]
mod cmake_install_rpath_tests {
    use super::{
        reconcile_cmake_install_rpaths, rewritten_old_rpath_line, trim_trailing_empty_rpath_entries,
    };

    /// The trim is `Some` only when it changes something, and refuses a value
    /// that is nothing but empty entries - `-Wl,-rpath,:::::::` appears in real
    /// build logs and has no sequence for CMake to match either way.
    #[test]
    fn trim_reports_only_real_changes() {
        assert_eq!(
            trim_trailing_empty_rpath_entries("/build/x/lib:"),
            Some("/build/x/lib".to_string())
        );
        assert_eq!(
            trim_trailing_empty_rpath_entries("/a:/b::"),
            Some("/a:/b".to_string())
        );
        // Already clean: nothing to do, and a caller counting edits must not
        // be told this file changed.
        assert_eq!(trim_trailing_empty_rpath_entries("/build/x/lib"), None);
        // Entirely empty entries: blanking it would change the meaning of the
        // check rather than repair it.
        assert_eq!(trim_trailing_empty_rpath_entries(":::::::"), None);
        assert_eq!(trim_trailing_empty_rpath_entries(""), None);
        // An INTERIOR empty entry is not ours to touch - only the trailing
        // ones are what `assemble_rpath` removes.
        assert_eq!(trim_trailing_empty_rpath_entries("/a::/b"), None);
    }

    /// Line rewriting keeps everything around the value, including CMake's
    /// indentation, and leaves lines that are not an OLD_RPATH alone.
    #[test]
    fn only_old_rpath_lines_are_rewritten() {
        assert_eq!(
            rewritten_old_rpath_line("         OLD_RPATH \"/build/x/lib:\"\n"),
            Some("         OLD_RPATH \"/build/x/lib\"\n".to_string())
        );
        // NEW_RPATH is CMake's output, not its expectation, and an empty one
        // is the normal install case. Touching it would be a real defect.
        assert_eq!(rewritten_old_rpath_line("         NEW_RPATH \"\"\n"), None);
        assert_eq!(rewritten_old_rpath_line("file(INSTALL ...)\n"), None);
        assert_eq!(
            rewritten_old_rpath_line("         OLD_RPATH \"/build/x/lib\"\n"),
            None
        );
    }

    /// End to end over a directory, because nine tests over a predicate once
    /// covered nothing about whether anything called it.
    #[test]
    fn the_pass_edits_generated_files_and_counts_them() {
        let dir = std::env::temp_dir().join(format!("nn-rpath-{}", std::process::id()));
        let sub = dir.join("src").join("tools");
        std::fs::create_dir_all(&sub).unwrap();

        let target = sub.join("cmake_install.cmake");
        std::fs::write(
            &target,
            "file(RPATH_CHANGE\n     FILE \"/out/bin/ahost\"\n                  OLD_RPATH \"/build/c/build/lib:\"\n     NEW_RPATH \"\")\n",
        )
        .unwrap();
        // A file that must NOT be touched, and a name that must not be picked
        // up by the walk.
        let bystander = dir.join("CMakeCache.txt");
        let bystander_text = "SOMETHING:STRING=OLD_RPATH \"/x:\"\n";
        std::fs::write(&bystander, bystander_text).unwrap();

        let edited = reconcile_cmake_install_rpaths(&dir).unwrap();
        assert_eq!(
            edited, 1,
            "one generated file carried a trailing empty entry"
        );

        let got = std::fs::read_to_string(&target).unwrap();
        assert!(got.contains("OLD_RPATH \"/build/c/build/lib\""));
        assert!(!got.contains("lib:\""));
        // NEW_RPATH survives untouched.
        assert!(got.contains("NEW_RPATH \"\""));
        assert_eq!(std::fs::read_to_string(&bystander).unwrap(), bystander_text);

        // Idempotent: a second pass finds nothing left to do.
        assert_eq!(reconcile_cmake_install_rpaths(&dir).unwrap(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod placed_mtime_tests {
    use super::{clone_or_copy, placement_is_ours, unique_sibling};

    /// THE ALIAS CASE, which is a regression this guard exists to prevent.
    /// Without it every versioned shared library in a placed tree becomes
    /// three real files, and the two that should be links keep whatever RPATH
    /// they had before the install rewrite.
    #[test]
    fn a_soname_alias_is_not_ours_and_a_placed_product_is() {
        use std::path::Path;
        let dir = std::env::temp_dir().join(format!("nn-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // The alias: libfoo.so -> libfoo.so.1.2.3, a SIBLING.
        let real = dir.join("libfoo.so.1.2.3");
        std::fs::write(&real, b"elf").unwrap();
        let alias = dir.join("libfoo.so");
        std::os::unix::fs::symlink("libfoo.so.1.2.3", &alias).unwrap();
        // `src` is the store object this placement would have copied, and it
        // is NOT what the alias points at.
        let store_obj = dir.join("store-object.o");
        std::fs::write(&store_obj, b"obj").unwrap();
        assert!(
            !placement_is_ours(&alias, &store_obj),
            "an alias pointing at a sibling must be left alone"
        );

        // The product: a link this placement made, pointing at the store file.
        let placed = dir.join("a.o");
        std::os::unix::fs::symlink(&store_obj, &placed).unwrap();
        assert!(placement_is_ours(&placed, &store_obj));

        // A dangling link answers no rather than panicking.
        let dangling = dir.join("gone.o");
        std::os::unix::fs::symlink(dir.join("nothing-here"), &dangling).unwrap();
        assert!(!placement_is_ours(&dangling, &store_obj));
        assert!(!placement_is_ours(Path::new("/nonexistent/x"), &store_obj));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A placed product must not read as older than a source, and the check
    /// is `stat` rather than `lstat` because that is what `make` calls.
    /// Pinning the SYMLINK case too: setting the link's own time leaves
    /// `stat` reading 1, so a fix that only touched the placement would pass
    /// an `lstat` assertion and change nothing about the rebuild.
    #[test]
    fn placed_product_is_newer_than_a_store_source() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("nn-mtime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store_file = dir.join("obj.o");
        std::fs::write(&store_file, b"object bytes").unwrap();
        // What nix gives every store file.
        let one = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        filetime_set(&store_file, one);
        assert_eq!(std::fs::metadata(&store_file).unwrap().mtime(), 1);

        let dest = dir.join("placed.o");
        std::os::unix::fs::symlink(&store_file, &dest).unwrap();
        assert_eq!(
            std::fs::metadata(&dest).unwrap().mtime(),
            1,
            "a symlink into the store reads as mtime 1 through stat"
        );

        let tmp = unique_sibling(&dest);
        clone_or_copy(&store_file, &tmp).unwrap();
        std::fs::rename(&tmp, &dest).unwrap();

        assert!(
            !dest.is_symlink(),
            "the placed product must not be a symlink"
        );
        assert!(
            std::fs::metadata(&dest).unwrap().mtime() > 1,
            "a placed product must be NEWER than a store source, not equal"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"object bytes");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn filetime_set(p: &std::path::Path, t: std::time::SystemTime) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(t))
            .unwrap();
    }
}
