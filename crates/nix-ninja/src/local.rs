use anyhow::Result;
use harmonia_store_derivation::derived_path::SingleDerivedPath;
use harmonia_store_path::StoreDir;
use nix_builder_rpc_client::{BuilderRpcClient, Patience};
use nix_ninja_task::derived_file::{create_symlinks, DerivedFile};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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
    let store_paths = rpc_client.build_paths(store_dir, &derived_paths, Patience::Escalating)?;

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

fn opaque_twin(df: &DerivedFile, store_path: &harmonia_store_path::StorePath) -> DerivedFile {
    DerivedFile {
        derived_path: SingleDerivedPath::Opaque(store_path.clone()),
        build_path: df.build_path.clone(),
        rel_path: df.rel_path.clone(),
    }
}

/// The outputs a set of placed links point at and nobody asked for. A
/// versioned library's aliases are stored as their link text (a sibling
/// name); when only an alias reaches the placement set, the library it
/// names must come too or the placed link dangles. One step: the caller
/// realises what this returns and asks again, so a chain of aliases
/// resolves one link per round.
pub fn link_targets_to_place(
    realised: &[DerivedFile],
    store_dir: &StoreDir,
    all_outputs: &HashMap<PathBuf, DerivedFile>,
    have: &std::collections::HashSet<PathBuf>,
) -> Vec<DerivedFile> {
    let mut out = Vec::new();
    let mut added = std::collections::HashSet::new();
    for df in realised {
        let object = df.absolute_path(store_dir);
        let Ok(target) = std::fs::read_link(&object) else {
            continue;
        };
        if target.is_absolute() || target.components().count() != 1 {
            continue;
        }
        let sibling = match df.build_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.join(&target),
            _ => target.clone(),
        };
        if have.contains(&sibling) || !added.insert(sibling.clone()) {
            continue;
        }
        if let Some(t) = all_outputs.get(&sibling) {
            out.push(t.clone());
        }
    }
    out
}

pub fn symlink_derived_files(
    rpc_client: &BuilderRpcClient,
    store_dir: &StoreDir,
    prefix: &Path,
    derived_files: &[DerivedFile],
    all_outputs: &HashMap<PathBuf, DerivedFile>,
) -> Result<()> {
    let mut derived_files: Vec<DerivedFile> = derived_files.to_vec();
    let mut store_paths = Vec::new();
    let mut scanned = 0;
    // A placed link needs its target beside it, and the target may be an
    // output nobody requested (openfec: `all` names the alias phony, the
    // installer copies through it, and the library it pointed at was never
    // placed; configuration B, 2026-09-04). Realise, read the links, add
    // their targets, and go round until nothing new appears.
    loop {
        let derived_paths: Vec<_> = derived_files[scanned..]
            .iter()
            .map(|df| df.derived_path.clone())
            .collect();
        store_paths.extend(rpc_client.build_paths(
            store_dir,
            &derived_paths,
            Patience::Escalating,
        )?);
        let have: std::collections::HashSet<PathBuf> = derived_files
            .iter()
            .map(|df| df.build_path.clone())
            .collect();
        let realised: Vec<DerivedFile> = derived_files[scanned..]
            .iter()
            .zip(store_paths[scanned..].iter())
            .map(|(df, sp)| opaque_twin(df, sp))
            .collect();
        scanned = derived_files.len();
        let extra = link_targets_to_place(&realised, store_dir, all_outputs, &have);
        if extra.is_empty() {
            break;
        }
        eprintln!(
            "nix-ninja: placing {} link target(s) beside the requested outputs",
            extra.len()
        );
        derived_files.extend(extra);
    }
    place_realised(store_dir, prefix, &derived_files, &store_paths)
}

/// Place realised outputs: the half of `symlink_derived_files` after the
/// daemon has answered, so a test can drive every placement route.
fn place_realised(
    store_dir: &StoreDir,
    prefix: &Path,
    derived_files: &[DerivedFile],
    store_paths: &[harmonia_store_path::StorePath],
) -> Result<()> {
    // Create new DerivedFiles with opaque store paths instead of placeholders
    let opaque_files: Vec<DerivedFile> = derived_files
        .iter()
        .zip(store_paths.iter())
        .map(|(df, store_path)| opaque_twin(df, store_path))
        .collect();

    // What configure left where an output lands is kept before anything is
    // written there, so a later pass can read it (configure_state). Never on
    // the compiler route, which keeps no record.
    if std::env::var_os("NIX_NINJA_NO_BUILD_DIR_SCAN").is_none() {
        crate::configure_state::keep_originals(
            prefix,
            opaque_files.iter().map(|d| d.build_path.as_path()),
        )?;
    }

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
    // NIX_NINJA_DIAG: where each placement comes from. The question a
    // wrong placement raises is which registration won the node, and the
    // build log is the only place a sandboxed run can answer it.
    if std::env::var_os("NIX_NINJA_DIAG").is_some() {
        for df in &opaque_files {
            eprintln!(
                "nix-ninja: DIAG placing {} from {}",
                df.build_path.display(),
                df.absolute_path(store_dir).display()
            );
        }
    }
    let mut symlink_files: Vec<DerivedFile> = Vec::new();
    let mut residue: Vec<(PathBuf, String)> = Vec::new();
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
            let rewritten = crate::task::rewrite_bytes(&data, &restore);
            // A PLACEHOLDER THE WHOLE-PATH SWAP COULD NOT REACH IS REFUSED,
            // neither guessed at nor built again. The driver used to re-run
            // itself with every task keyed to the real outer paths, which
            // rebuilt the run a second time (a whole package on the ninja
            // route), and then to swap the hash's surviving pieces in place,
            // which compiler output proved cannot be done from the bytes
            // alone (a piece's edge matches its neighbour by chance). A file
            // still carrying it is NOT placed: it is wrong in a way nothing
            // downstream detects.
            let after = rewritten.as_deref().unwrap_or(&data);
            if let Some(p) = crate::task::placeholder_residue(after, &restore) {
                residue.push((df.build_path.clone(), p));
                continue;
            }
            if let Some(rewritten) = rewritten {
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
    // Residue outranks a placement error: it names the file the restore
    // could not reach, and returning the other error would lose the finding.
    let placed = create_symlinks(prefix, store_dir, symlink_files.clone(), true);
    if !residue.is_empty() {
        return Err(PlaceholderResidue(residue).into());
    }
    placed?;
    refresh_placed_mtimes(prefix, store_dir, &symlink_files)?;
    scrub_placed_runpaths(prefix, &opaque_files)?;
    Ok(())
}

/// Scrub every placement that is a regular file by now, whichever route
/// made it one: a link `refresh_placed_mtimes` turned into bytes, a
/// `$ORIGIN`-relative file `create_symlinks` copied, or a placeholder
/// restore. Placed links and directories are not read. A failure leaves
/// the file as placed and is counted on one line, since the only later
/// symptom is the fixup audit's refusal.
///
/// DEFER(a refused library inside a placed directory output): the files of
/// a directory input are not walked here, though `refresh_tree_mtimes`
/// walks them.
fn scrub_placed_runpaths(prefix: &Path, files: &[DerivedFile]) -> Result<()> {
    let mut failed: Vec<String> = Vec::new();
    for df in files {
        let dest = prefix.join(&df.build_path);
        if !dest.is_file() || dest.is_symlink() {
            continue;
        }
        if let Err(e) = scrub_placed_runpath(&dest, dest.parent().unwrap_or(Path::new(""))) {
            failed.push(format!("{}: {e}", dest.display()));
        }
    }
    match failed.first() {
        Some(first) => Err(anyhow::anyhow!(
            "runpath scrub failed on {} placed file(s); first {first}",
            failed.len()
        )),
        None => Ok(()),
    }
}

/// Outputs left unplaced because the restore could not reach their
/// placeholder (`task::placeholder_residue`). `cli::run` fails the run with
/// it; nothing is built again. Only the placement in this file
/// scans, so an output placed by the dynamic or outer-stage paths is not
/// covered.
#[derive(Debug)]
pub struct PlaceholderResidue(pub Vec<(PathBuf, String)>);

impl std::fmt::Display for PlaceholderResidue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "outer-output placeholder survived the restore in")?;
        for (path, placeholder) in &self.0 {
            write!(f, " {} ({placeholder})", path.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for PlaceholderResidue {}

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
fn refresh_placed_mtimes(
    prefix: &Path,
    store_dir: &StoreDir,
    files: &[DerivedFile],
) -> std::io::Result<()> {
    for df in files {
        let src = df.absolute_path(store_dir);
        let dest = prefix.join(&df.build_path);
        // A TREE IS PLACED FILE BY FILE, SO IT IS REFRESHED FILE BY FILE.
        // `create_symlinks` links a directory input's CONTENTS rather than
        // the directory, and each of those links is a placement with the
        // same mtime 1 that every other placement has. Skipping the whole
        // input here left an autogen or syncqt tree reading as older than
        // every source, which is the rebuild-every-pass defect this
        // function exists for. Reachable from a plain target since target
        // resolution began expanding an edge's co-outputs; before that it
        // needed NIX_NINJA_MATERIALIZE_ALL.
        if src.is_dir() {
            refresh_tree_mtimes(&src, &dest)?;
            continue;
        }
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
        refresh_one(&dest, &src)?;
    }
    Ok(())
}

/// Replace one placed symlink with the bytes it points at, carrying a fresh
/// mtime and the store mode plus owner write. A failure is the run's: the
/// link it leaves reads as older than every source to real ninja.
fn refresh_one(dest: &Path, src: &Path) -> std::io::Result<()> {
    if std::env::var_os("NIX_NINJA_DIAG").is_some() {
        eprintln!(
            "nix-ninja: DIAG refresh copies {} from {}",
            dest.display(),
            src.display()
        );
    }
    let tmp = unique_sibling(dest);
    let res = clone_or_copy(src, &tmp)
        .and_then(|_| std::fs::metadata(src))
        .and_then(|md| {
            let mut perm = md.permissions();
            perm.set_mode(perm.mode() | 0o200);
            std::fs::set_permissions(&tmp, perm)
        })
        .and_then(|_| std::fs::rename(&tmp, dest));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.map_err(|e| std::io::Error::other(format!("refreshing {}: {e}", dest.display())))
}

/// Every file placed under a directory input, walked in step with the
/// per-file links `create_symlinks` wrote for it.
///
/// The links point into the store object, so each one answers
/// `placement_is_ours` on its own; a file the BUILD TREE already held was
/// skipped at placement and is a real file rather than a link, so it is
/// skipped here too and keeps the time it has.
fn refresh_tree_mtimes(src: &Path, dest: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            refresh_tree_mtimes(&from, &to)?;
        } else if to.is_symlink() && placement_is_ours(&to, &from) {
            refresh_one(&to, &from)?;
        }
    }
    Ok(())
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
    // THE SOURCE IS ASKED FIRST, AND THE CANONICAL COMPARISON BELOW CANNOT
    // ANSWER THIS. A link edge declares all three of `libfoo.so.1.2.3`,
    // `libfoo.so.1` and `libfoo.so` as outputs, so each is a `DerivedFile` and
    // the two aliases are stored as SYMLINKS. Placing one and then copying it
    // resolves both sides of that comparison to the same real library, so the
    // comparison says yes and the alias becomes a third copy - which is the
    // defect, not a case the comparison excludes.
    //
    // An output that is a link in the store is a NAME. Materialising it as
    // content is what loses the soname relationship, so it is refused on the
    // source's own type rather than on where anything resolves to.
    if std::fs::symlink_metadata(src).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
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

/// Drop from a placed ELF file's runpath every absolute entry that repeats
/// the expansion of an EARLIER `$ORIGIN` entry against `dir`, the directory
/// the file is placed in.
///
/// The task binary resolves a needed library through a `$ORIGIN` climb into
/// the build tree and appends that directory as an absolute entry as well
/// (weston's plugins: `$ORIGIN/../frontend` then `/build/source/build/frontend`).
/// The fixup's `patchelf --shrink-rpath` keeps it, since the library is
/// there, and the audit then refuses the installed file for naming `/build`.
/// Every other entry, empty and `$ORIGIN` included, stays in place and in
/// order. The string is rewritten in place, NUL-padded to its old length,
/// the way meson's depfixer does; the rest of the file and its mode are
/// untouched.
///
/// The drop changes no lookup in the build tree: the loader reaches that
/// directory through the earlier `$ORIGIN` entry first. That holds while
/// the lexical expansion and the kernel's agree, which a symlinked
/// directory between the file and the climb's target would break; and an
/// in-place write reaches every hard link of the file, where placements
/// are fresh inodes.
///
/// `Ok` for anything the scrub does not apply to (not ELF64 little-endian,
/// not an executable or shared library, no runpath, nothing to drop); `Err`
/// from the open, read, seek or write.
///
/// DEFER(a refused install naming a deepened task build dir): the
/// expansion assumes the placed file's dir is the one the task ran in.
/// DEFER(an ELF32 output refused for the same entry): ELF64 only.
fn scrub_placed_runpath(path: &Path, dir: &Path) -> std::io::Result<()> {
    // Read-only until there is something to drop: most placements are not
    // rewritten, and a write open of a running or read-only file fails.
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut head = [0u8; 0x40];
    if f.read_exact(&mut head).is_err() || !crate::task::is_elf64_le(&head) {
        return Ok(());
    }
    // e_type: ET_EXEC 2, ET_DYN 3. An object file carries no runpath, and
    // objects are most of what is placed, so they are never read whole.
    if !matches!(u16::from_le_bytes([head[0x10], head[0x11]]), 2 | 3) {
        return Ok(());
    }
    let mut data = head.to_vec();
    f.read_to_end(&mut data)?;
    let Some((off, len)) = runpath_string(&data) else {
        return Ok(());
    };
    let Some(new) = std::str::from_utf8(&data[off..off + len])
        .ok()
        .and_then(|old| scrub_runpath(old, dir))
    else {
        return Ok(());
    };
    let mut bytes = new.into_bytes();
    bytes.resize(len, 0);
    // Every placement route leaves the file owner-writable: a refresh and a
    // restore add owner write, and `create_symlinks` clears read-only on the
    // copies it makes. The write goes only to the inode that was read.
    let mut w = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let (r, m) = (f.metadata()?, w.metadata()?);
    if (r.dev(), r.ino()) != (m.dev(), m.ino()) {
        return Err(std::io::Error::other("replaced between read and write"));
    }
    w.seek(SeekFrom::Start(off as u64))?;
    w.write_all(&bytes)
}

/// The file offset and length of an ELF64 little-endian file's
/// `DT_RUNPATH` string (else `DT_RPATH`), mapped through the `PT_LOAD`
/// segments the way the loader maps `DT_STRTAB`; `None` for anything the
/// walk cannot read.
fn runpath_string(data: &[u8]) -> Option<(usize, usize)> {
    let at = |o: usize, n: usize| data.get(o..o.checked_add(n)?);
    let u16le = |o| Some(u16::from_le_bytes(at(o, 2)?.try_into().ok()?) as usize);
    let u32le = |o| Some(u32::from_le_bytes(at(o, 4)?.try_into().ok()?));
    let u64le = |o| usize::try_from(u64::from_le_bytes(at(o, 8)?.try_into().ok()?)).ok();
    // A field read past the end of the address space is a miss, not a panic.
    let fld = |base: usize, off: usize| u64le(base.saturating_add(off));
    // Elf64_Phdr: p_type u32 at +0, p_offset +8, p_vaddr +0x10, p_filesz +0x20.
    let (phoff, phentsize) = (u64le(0x20)?, u16le(0x36)?);
    let phdrs = (0..u16le(0x38)?).filter_map(|i| phoff.checked_add(i.checked_mul(phentsize)?));
    let dynamic = phdrs.clone().find(|&p| u32le(p) == Some(2))?;
    let (dyn_off, dyn_size) = (fld(dynamic, 8)?, fld(dynamic, 0x20)?);
    // Elf64_Dyn: d_tag at +0, d_val at +8. DT_STRTAB is an address; the
    // runpath tags are offsets into that table.
    let (mut strtab, mut runpath, mut rpath) = (None, None, None);
    for e in (dyn_off..dyn_off.saturating_add(dyn_size)).step_by(16) {
        match u64le(e)? {
            0 => break,
            5 => strtab = fld(e, 8),
            29 => runpath = fld(e, 8),
            15 => rpath = fld(e, 8),
            _ => {}
        }
    }
    let vaddr = strtab?.checked_add(runpath.or(rpath)?)?;
    let off = phdrs.filter(|&p| u32le(p) == Some(1)).find_map(|p| {
        let (o, va, sz) = (fld(p, 8)?, fld(p, 0x10)?, fld(p, 0x20)?);
        if !(va..va.checked_add(sz)?).contains(&vaddr) {
            return None;
        }
        o.checked_add(vaddr - va)
    })?;
    let len = data.get(off..)?.iter().position(|&b| b == 0)?;
    Some((off, len))
}

/// `runpath` without each absolute entry equal to the lexical expansion of
/// an earlier `$ORIGIN` entry against `dir`; `None` when nothing is dropped.
pub(crate) fn scrub_runpath(runpath: &str, dir: &Path) -> Option<String> {
    let mut origins: Vec<PathBuf> = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = false;
    for e in runpath.split(':') {
        let rest = e
            .strip_prefix("$ORIGIN")
            .or_else(|| e.strip_prefix("${ORIGIN}"))
            .filter(|r| r.is_empty() || r.starts_with('/'));
        if let Some(rest) = rest {
            let climb = Path::new(rest.trim_start_matches('/'));
            origins.push(crate::task::lexical_join(dir, climb));
        } else if e.starts_with('/') && origins.iter().any(|o| o == Path::new(e)) {
            dropped = true;
            continue;
        }
        kept.push(e);
    }
    dropped.then(|| kept.join(":"))
}

#[cfg(test)]
mod scrub_runpath_tests {
    use super::{place_realised, scrub_runpath, SingleDerivedPath};
    use harmonia_store_path::StoreDir;
    use nix_ninja_task::derived_file::DerivedFile;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[test]
    fn only_a_later_absolute_repeat_of_an_origin_expansion_is_dropped() {
        let dir = Path::new("/b/plugin");
        let cases: &[(&str, Option<&str>)] = &[
            // weston's shape: the climb, the store entries, the repeat.
            (
                "$ORIGIN/../frontend:/s/lib:/b/frontend",
                Some("$ORIGIN/../frontend:/s/lib"),
            ),
            (
                "${ORIGIN}/../frontend:/b/frontend",
                Some("${ORIGIN}/../frontend"),
            ),
            ("$ORIGIN:::/b/plugin", Some("$ORIGIN::")),
            // An absolute entry ahead of the climb is a first occurrence.
            ("/b/frontend:$ORIGIN/../frontend", None),
            // An empty entry is not `$ORIGIN`, whatever the loader does with it.
            (":/b/plugin", None),
            // Read as `$ORIGIN` + `AL/../..`, this would land on /b/frontend.
            ("$ORIGINAL/../../frontend:/b/frontend", None),
            ("$ORIGIN/../frontend:/b/other", None),
            ("/s/lib", None),
        ];
        for (runpath, want) in cases {
            assert_eq!(scrub_runpath(runpath, dir).as_deref(), *want, "{runpath}");
        }
    }

    fn have(tool: &str) -> bool {
        Command::new(tool).arg("--version").output().is_ok()
    }

    /// `readelf -d`'s runpath (or rpath) line: a reader independent of the
    /// one under test.
    fn readelf_runpath(f: &Path) -> String {
        let out = Command::new("readelf").arg("-d").arg(f).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.lines()
            .find(|l| l.contains("(RUNPATH)") || l.contains("(RPATH)"))
            .and_then(|l| Some(l.split_once('[')?.1.trim_end_matches(']').to_string()))
            .unwrap_or_else(|| panic!("no runpath in {}: {text}", f.display()))
    }

    /// `$out` for the restore arm, and the placeholder a task output carries
    /// in its place (`outer_rewrite_map`, which hashes the output name).
    const OUT: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-wtest-1";

    fn with_out<T>(out: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _env = crate::task::OUT_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match out {
            Some(o) => {
                std::env::set_var("outputs", "out");
                std::env::set_var("out", o);
            }
            None => {
                std::env::remove_var("outputs");
                std::env::remove_var("out");
            }
        }
        let r = f();
        std::env::remove_var("outputs");
        std::env::remove_var("out");
        r
    }

    fn placeholder() -> String {
        with_out(Some(OUT), crate::task::outer_rewrite_map)[0]
            .1
            .clone()
    }

    /// Link with gcc into a store-shaped dir (read-only, as the store is)
    /// and place it through `place_realised`, the half of
    /// `symlink_derived_files` after the daemon answers, so production picks
    /// the route: a link refreshed into bytes, a `$ORIGIN`-relative copy, or
    /// a placeholder restore when `out` is set.
    fn place(
        d: &Path,
        tag: &str,
        args: &[&str],
        build_path: &str,
        out: Option<&str>,
    ) -> (PathBuf, PathBuf) {
        let store_root = d.join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let unit = "int f(void) { return 1; }\nint main(void) { return f(); }\n";
        std::fs::write(d.join("x.c"), unit).unwrap();
        let src = store_root.join(format!(
            "1ccccccccccccccccccccccccccccccc-ninja-build-{tag}"
        ));
        let st = Command::new("gcc")
            .args(args)
            .arg(d.join("x.c"))
            .arg("-o")
            .arg(&src)
            .status()
            .unwrap();
        assert!(st.success(), "gcc {args:?}");
        let ro = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&src, ro).unwrap();
        let store_dir = StoreDir::new(&store_root).unwrap();
        let df = DerivedFile::from_encoded(&store_dir, &format!("{}:{build_path}:", src.display()))
            .unwrap();
        let SingleDerivedPath::Opaque(sp) = df.derived_path.clone() else {
            unreachable!("an encoded store file is opaque")
        };
        let build = d.join("build");
        std::fs::create_dir_all(&build).unwrap();
        with_out(out, || place_realised(&store_dir, &build, &[df], &[sp])).unwrap();
        let dest = build.join(build_path);
        assert!(!dest.is_symlink(), "{build_path} was placed as bytes");
        (src, dest)
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nn-rps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// The bytes differ only inside one window shorter than `max`.
    fn differ_within(a: &[u8], b: &[u8], max: usize) -> bool {
        let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        a.len() == b.len() && diff.last().is_some_and(|l| l - diff[0] < max)
    }

    #[test]
    fn a_placed_library_loses_the_build_tree_repeat_and_nothing_else() {
        if !have("gcc") || !have("readelf") {
            eprintln!("SKIP: gcc and readelf are both needed to link a real library");
            return;
        }
        // The non-PIE executable is ET_EXEC with its first PT_LOAD at
        // 0x400000 against file offset 0, so `.dynstr`'s address and file
        // offset differ; in a shared library they coincide. p_paddr equals
        // p_vaddr in anything ld emits, so reading one for the other is not
        // a defect these arms can see.
        let fake = placeholder();
        for (tag, dtags, exec, restored) in [
            ("runpath", "--enable-new-dtags", false, false),
            ("rpath", "--disable-new-dtags", false, false),
            ("exec", "--enable-new-dtags", true, false),
            // weston's plugins: the stdenv self-rpath arrives as the `out`
            // placeholder, so the file takes the restore route.
            ("restored", "--enable-new-dtags", false, true),
        ] {
            let d = scratch(tag);
            let repeat = format!("{}/build/frontend", d.display());
            let mid = if restored {
                format!("{fake}/lib")
            } else {
                "/s/lib".into()
            };
            let flag = format!("-Wl,{dtags},-rpath,$ORIGIN/../frontend:{mid}:{repeat}");
            let args: &[&str] = if exec {
                &["-no-pie", &flag]
            } else {
                &["-shared", "-fPIC", &flag]
            };
            let out = restored.then_some(OUT);
            let (src, dest) = place(&d, tag, args, "plugin/x", out);
            if exec {
                let h = Command::new("readelf")
                    .arg("-h")
                    .arg(&src)
                    .output()
                    .unwrap();
                let h = String::from_utf8_lossy(&h.stdout).into_owned();
                assert!(h.contains("EXEC (Executable file)"), "{tag}: {h}");
            }
            let before = readelf_runpath(&src);
            let after = readelf_runpath(&dest);
            // Control: the real linker wrote the shape the scrub is for.
            let shape = format!("$ORIGIN/../frontend:{mid}:{repeat}");
            assert!(before.contains(&shape), "{tag}: {before}");
            let want: Vec<&str> = before.split(':').filter(|e| *e != repeat).collect();
            let want = want.join(":").replace(&fake, OUT);
            assert_eq!(after, want, "{tag}: only the repeat goes");
            // The restore's swap lies inside the same string, so one window
            // still holds every changed byte.
            let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dest).unwrap());
            assert!(
                differ_within(&a, &b, before.len()),
                "{tag}: only the string moved"
            );
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn an_object_and_a_library_without_the_repeat_are_left_byte_for_byte() {
        if !have("gcc") || !have("readelf") {
            eprintln!("SKIP: gcc and readelf are both needed to link a real library");
            return;
        }
        // Placed one level shallower, the climb lands outside the tree and
        // the same absolute entry repeats nothing.
        let d = scratch("norep");
        let flag = format!(
            "-Wl,-rpath,$ORIGIN/../frontend:{}/build/frontend",
            d.display()
        );
        let (src, dest) = place(&d, "norep", &["-shared", "-fPIC", &flag], "x.so", None);
        assert_eq!(std::fs::read(&src).unwrap(), std::fs::read(&dest).unwrap());
        let _ = std::fs::remove_dir_all(&d);
        // An object carrying a runpath-shaped string is never read as one.
        let d = scratch("obj");
        let (src, dest) = place(&d, "obj", &["-c", "-fPIC"], "plugin/x.o", None);
        assert_eq!(std::fs::read(&src).unwrap(), std::fs::read(&dest).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod refresh_tree_mtimes_tests {
    use super::refresh_placed_mtimes;
    use harmonia_store_path::StoreDir;
    use nix_ninja_task::derived_file::DerivedFile;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nn-rtm-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A placed TREE gets the same treatment as a placed file, and nothing
    /// else in the directory does.
    ///
    /// `make` stats THROUGH a symlink and every store file reads as mtime 1,
    /// so a link left under an autogen tree is older than every source and
    /// the tree rebuilds on every pass. The two things that must NOT be
    /// touched are in the same directory: a file the build tree already held
    /// (a real file, never a placement) and a link the build itself made to a
    /// sibling, which is a NAME rather than a product.
    #[test]
    fn a_placed_tree_is_refreshed_and_the_build_s_own_files_are_not() {
        // THROUGH refresh_placed_mtimes, NOT through the tree walk it calls.
        // Written the direct way first, this passed with the call site
        // deleted - the fourth inert test in this tree, and the rule that
        // keeps being relearned is to pin the WIRING.
        let d = dir("tree");
        let store_root = d.join("store");
        let hash = "1ccccccccccccccccccccccccccccccc";
        let store = store_root.join(format!("{hash}-ninja-build-gen"));
        let build = d.join("build/gen");
        std::fs::create_dir_all(store.join("sub")).unwrap();
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(store.join("moc_a.cpp"), b"generated").unwrap();
        std::fs::write(store.join("sub/moc_b.cpp"), b"generated deeper").unwrap();
        // BOTH NEGATIVES MUST LIE INSIDE THE STORE TREE, because that is what
        // the walk reads. Placed only in the build tree they are unreachable
        // and their assertions hold whatever the guard does: mutating the
        // guard to `true` leaves them green, so a negative outside the walked
        // tree asserts nothing about the guard it is written for.
        std::fs::write(store.join("kept.cpp"), b"the store's copy").unwrap();
        std::os::unix::fs::symlink("moc_a.cpp", store.join("alias.cpp")).unwrap();
        std::fs::create_dir_all(build.join("sub")).unwrap();

        // What create_symlinks would have written for the tree.
        std::os::unix::fs::symlink(store.join("moc_a.cpp"), build.join("moc_a.cpp")).unwrap();
        std::os::unix::fs::symlink(store.join("sub/moc_b.cpp"), build.join("sub/moc_b.cpp"))
            .unwrap();
        // The build tree's own file, which link_tree skipped at placement
        // because it was already there, so it is a REAL FILE rather than a
        // placement and must not be overwritten by the store's copy. And the
        // build's own alias to a sibling, which is a NAME rather than a
        // product: `placement_is_ours` must refuse it.
        std::fs::write(build.join("kept.cpp"), b"the build tree's").unwrap();
        std::os::unix::fs::symlink("kept.cpp", build.join("alias.cpp")).unwrap();

        let store_dir = StoreDir::new(&store_root).unwrap();
        let df =
            DerivedFile::from_encoded(&store_dir, &format!("{}:gen:", store.display())).unwrap();
        refresh_placed_mtimes(&d.join("build"), &store_dir, &[df]).expect("refreshed");

        for placed in ["moc_a.cpp", "sub/moc_b.cpp"] {
            assert!(
                !build.join(placed).is_symlink(),
                "{placed} was placed by us and must become bytes with a real mtime"
            );
        }
        assert_eq!(
            std::fs::read(build.join("moc_a.cpp")).unwrap(),
            b"generated",
            "and the bytes are the ones the link pointed at"
        );
        assert!(
            build.join("alias.cpp").is_symlink(),
            "a link the build made to a sibling is a NAME and stays a link"
        );
        assert_eq!(
            std::fs::read(build.join("kept.cpp")).unwrap(),
            b"the build tree's",
            "and a real file the build tree already held is untouched"
        );
        std::fs::remove_dir_all(&d).ok();
    }
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
    patience: Patience,
) -> Result<usize> {
    let derived_paths: Vec<_> = derived_files
        .iter()
        .map(|df| df.derived_path.clone())
        .collect();
    // PATIENCE IS THE CALLER'S TO CHOOSE AND THE TWO CALLERS DIFFER, which
    // one Single here got wrong. The depfile pass is best effort by its own
    // contract - a miss costs a rescan. The `-v` transcript is NOT: CMake's
    // compiler ABI detection PARSES it for CMAKE_<LANG>_IMPLICIT_LINK_
    // LIBRARIES, so an empty one configures a project that records an empty
    // implicit-link list and every later Fortran link fails, three steps
    // from the cause. That is this tree's liblapack blocker 4, and giving
    // the transcript one allowance re-armed it.
    // UNPINNED BY ANY TEST, said rather than faked: which patience each call
    // site passes needs a daemon that stalls on demand to exercise, and a
    // test reading the source back would pass whatever the code did.
    let store_paths = rpc_client.build_paths(store_dir, &derived_paths, patience)?;

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
        // The fixture must refuse a plain write, or root passes this with
        // the rename replaced by an in-place write.
        assert!(
            std::fs::write(&dest, b"probe").is_err(),
            "0444 does not block this uid (root?), the arm cannot discriminate"
        );

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
mod placed_mtime_tests {
    use super::{clone_or_copy, placement_is_ours, unique_sibling};

    /// THE CASE THE FIRST GUARD MISSED, and it is the one brotli actually
    /// has. When the alias is ITSELF a declared output, both sides of a
    /// canonical comparison resolve to the same real library, so comparing
    /// resolved paths says yes and the alias is copied anyway. Only the
    /// source's own type distinguishes a name from a product.
    #[test]
    fn an_alias_that_is_itself_the_placed_output_is_refused() {
        let dir = std::env::temp_dir().join(format!("nn-alias2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The "store": a real library and an alias beside it, as a link edge
        // stores its three outputs.
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("libfoo.so.1.2.3"), b"elf").unwrap();
        let src_alias = store.join("libfoo.so");
        std::os::unix::fs::symlink("libfoo.so.1.2.3", &src_alias).unwrap();
        // The build tree: the placement's own symlink at the alias name.
        let build = dir.join("build");
        std::fs::create_dir_all(&build).unwrap();
        let dest = build.join("libfoo.so");
        std::os::unix::fs::symlink(&src_alias, &dest).unwrap();

        // Both resolve to the same real library, so a resolved-path
        // comparison alone cannot refuse this.
        assert_eq!(
            std::fs::canonicalize(&dest).unwrap(),
            std::fs::canonicalize(&src_alias).unwrap()
        );
        assert!(
            !placement_is_ours(&dest, &src_alias),
            "an output that is a link in the store is a name, not a product"
        );

        // And the real library beside it is still placed.
        let src_real = store.join("libfoo.so.1.2.3");
        let dest_real = build.join("libfoo.so.1.2.3");
        std::os::unix::fs::symlink(&src_real, &dest_real).unwrap();
        assert!(placement_is_ours(&dest_real, &src_real));

        let _ = std::fs::remove_dir_all(&dir);
    }

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
