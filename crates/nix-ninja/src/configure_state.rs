//! A later pass over a build directory resolves against the directory as the
//! first pass found it.
//!
//! The consumer runs the driver more than once over one build directory: the
//! build phase, then install and check with every output materialized. Each
//! pass places its products into the directory, and every route that asks
//! the disk (the build-dir walk, the include scan, readers, response files)
//! then answered the next pass differently: placed soname links read as
//! configure aliases, placed forwarding headers led the scan into source
//! headers it had not reached, directories existed that configure never
//! made. Each answer is written into task derivations, so every edge of the
//! install pass emitted a derivation the build pass had not, and qtbase
//! 6.11.2 was built twice.
//!
//! The first walk records the directories and files configure left
//! (`save`). After a run's placement has succeeded, the run journals what
//! placement wrote (`journal_placed`). A later pass that does not trust
//! earlier invocations' state (the resolve memo's switch: off inside a
//! build) moves the journaled paths aside before it reads the directory and
//! puts them back once it has placed its own outputs (`Rewind`). Only the
//! driver's own placements move: a file a hook or the build system wrote
//! between phases stays where it is. A PATH CONFIGURE LEFT THAT PLACEMENT
//! REPLACED (a 0-byte placeholder a generator's output lands on, qtbase's
//! `qt6core_metatypes.json`) is snapshotted before placement replaces it
//! (`keep_originals`), and a later pass reads configure's version for its
//! duration. A changed build.ninja is a re-configure: no record answers for
//! it and the walk records afresh.

use harmonia_store_derivation::derived_path::SingleDerivedPath;
use nix_ninja_task::derived_file::DerivedFile;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// The walk does not descend these, so the record knows nothing below them.
const STATE_DIR: &str = ".nix-ninja-state";

fn read_removed(state: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(state.join("removed-dirs"))
        .map(|s| s.lines().map(PathBuf::from).collect())
        .unwrap_or_default()
}

pub fn walk_skips(name: &str) -> bool {
    name.starts_with("meson-") || is_state_dir(name)
}

/// The driver's own state directory, which no walk or upload may take.
pub fn is_state_dir(name: &str) -> bool {
    name == STATE_DIR
}

/// This build dir's own state directory: under `NIX_BUILD_TOP` inside a
/// build, else in the build dir (walk-skipped), and keyed by the build dir
/// either way, so two trees in one build never share a record or an aside.
pub fn state_dir(build_dir: &Path) -> PathBuf {
    state_dir_in(
        std::env::var_os("NIX_BUILD_TOP")
            .map(PathBuf::from)
            .as_deref(),
        build_dir,
    )
}

fn state_dir_in(top: Option<&Path>, build_dir: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    build_dir.hash(&mut h);
    top.unwrap_or(build_dir)
        .join(STATE_DIR)
        .join(format!("{:016x}", h.finish()))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConfigureState {
    pub dirs: HashSet<String>,
    pub files: HashSet<String>,
    /// The directories that were empty.
    pub empty_dirs: HashSet<String>,
}

/// A later pass's record of the files configure left, set by the walk
/// (`answer_listings_from`) and asked by every other listing of a build-tree
/// directory (`left_by_configure`). Unset on a first pass, so nothing a
/// first pass emits depends on it.
static LISTED: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);

/// Makes this process's listings of build-tree directories answer from
/// configure's record: called by a later pass's walk.
pub fn answer_listings_from(files: HashSet<String>) {
    *LISTED.lock().unwrap() = Some(files);
}

/// Whether a later pass reads `path`, found by listing a directory: a file
/// under the build dir that configure did not leave is something written
/// between phases (a check run's log, bytecode, a hook's output; an earlier
/// pass's placement is set aside) and the first pass never saw it. A path
/// outside the build dir, or any path on a first pass, is read.
/// DEFER(a later pass re-keys on a file written between phases that an
/// argv or response-file token names directly): only directory listings ask.
pub fn left_by_configure(build_dir: &Path, path: &Path) -> bool {
    left_by(LISTED.lock().unwrap().as_ref(), build_dir, path)
}

fn left_by(files: Option<&HashSet<String>>, build_dir: &Path, path: &Path) -> bool {
    let Some(files) = files else {
        return true;
    };
    let abs = build_dir.join(path);
    match abs.strip_prefix(build_dir) {
        Ok(rel) if confined(&rel.to_string_lossy()) => files.contains(&*rel.to_string_lossy()),
        _ => true,
    }
}

/// Where placement keeps what configure left at a path before replacing it.
fn originals(state: &Path) -> PathBuf {
    state.join("originals")
}

/// Copies `src` to `dest` keeping what an upload hashes: a file's bytes and
/// mode (the executable bit is in the NAR), a symlink's link text (the
/// upload is named from the canonical path as well as the asked one).
fn copy_entry(src: &Path, dest: &Path, meta: &std::fs::Metadata) -> std::io::Result<()> {
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dest)
    } else {
        std::fs::copy(src, dest).map(|_| ())
    }
}

/// Whether `a` and `b` would upload alike: two symlinks with one link text,
/// or two files with the same bytes and executable bit.
fn same_entry(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) else {
        return false;
    };
    if ma.file_type().is_symlink() || mb.file_type().is_symlink() {
        return ma.file_type().is_symlink()
            && mb.file_type().is_symlink()
            && std::fs::read_link(a).ok() == std::fs::read_link(b).ok();
    }
    ma.is_file()
        && mb.is_file()
        && ma.len() == mb.len()
        && (ma.mode() & 0o111 != 0) == (mb.mode() & 0o111 != 0)
        && std::fs::read(a).ok() == std::fs::read(b).ok()
}

/// Snapshots what configure left at each of `placed` (relative build paths)
/// before placement replaces it, once per path: the file or symlink at the
/// path, and every one below it when the path is a directory configure
/// left. Called before anything is written; no record (the compiler route,
/// a first walk that never ran) means nothing to keep.
pub fn keep_originals<'a>(build_dir: &Path, placed: impl IntoIterator<Item = &'a Path>) {
    let state = state_dir(build_dir);
    if let Some(r) = load_in(&state, build_dir) {
        keep_originals_in(&state, build_dir, &r, placed);
    }
}

fn keep_originals_in<'a>(
    state: &Path,
    build_dir: &Path,
    r: &ConfigureState,
    placed: impl IntoIterator<Item = &'a Path>,
) {
    let orig = originals(state);
    let mut failed: Option<(PathBuf, std::io::Error)> = None;
    let mut keep = |rel: &Path| {
        if !r.files.contains(&*rel.to_string_lossy()) {
            return;
        }
        let (src, dest) = (build_dir.join(rel), orig.join(rel));
        if std::fs::symlink_metadata(&dest).is_ok() {
            return;
        }
        let Ok(meta) = std::fs::symlink_metadata(&src) else {
            return;
        };
        if !meta.is_file() && !meta.file_type().is_symlink() {
            return;
        }
        let res = dest
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| copy_entry(&src, &dest, &meta));
        if let Err(e) = res {
            failed.get_or_insert((rel.to_path_buf(), e));
        }
    };
    for rel in placed {
        if !confined(&rel.to_string_lossy()) {
            continue;
        }
        let abs = build_dir.join(rel);
        match std::fs::symlink_metadata(&abs) {
            Ok(m) if m.is_dir() => {
                for e in walkdir::WalkDir::new(&abs)
                    .min_depth(1)
                    .into_iter()
                    .flatten()
                {
                    if let (false, Ok(below)) =
                        (e.file_type().is_dir(), e.path().strip_prefix(build_dir))
                    {
                        keep(below);
                    }
                }
            }
            Ok(_) => keep(rel),
            Err(_) => {}
        }
    }
    if let Some((rel, e)) = failed {
        eprintln!(
            "nix-ninja: could not keep configure's copy of every path placement replaces \
             (first: {}: {e}); a later pass reads the placed one there",
            rel.display()
        );
    }
}

/// What identifies one configure: build.ninja's length and mtime.
fn header(build_dir: &Path) -> Option<String> {
    let m = std::fs::metadata(build_dir.join("build.ninja")).ok()?;
    Some(format!(
        "nix-ninja-configure-state v1 {} {}.{}",
        m.len(),
        m.mtime(),
        m.mtime_nsec()
    ))
}

fn read_lines(path: &Path, build_dir: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != header(build_dir)? {
        return None;
    }
    Some(lines.map(str::to_owned).collect())
}

/// Writes `lines` under this configure's header, through a file of this
/// process's own, so two drivers saving at once cannot interleave.
fn write_lines(
    path: &Path,
    build_dir: &Path,
    lines: &[String],
    append: bool,
) -> std::io::Result<()> {
    let Some(head) = header(build_dir) else {
        return Ok(());
    };
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut all: Vec<String> = if append {
        read_lines(path, build_dir).unwrap_or_default()
    } else {
        Vec::new()
    };
    all.extend(lines.iter().cloned());
    all.sort();
    all.dedup();
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    writeln!(f, "{head}")?;
    for l in &all {
        writeln!(f, "{l}")?;
    }
    f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// The record the first walk wrote for this configure, or None: no record,
/// an unreadable one, or one for another build.ninja.
pub fn load(build_dir: &Path) -> Option<ConfigureState> {
    load_in(&state_dir(build_dir), build_dir)
}

fn load_in(state: &Path, build_dir: &Path) -> Option<ConfigureState> {
    let mut s = ConfigureState::default();
    let mut malformed = 0usize;
    for line in read_lines(&state.join("configure.v1"), build_dir)? {
        match line.split_once(' ') {
            Some(("D", rel)) => s.dirs.insert(rel.to_string()),
            Some(("F", rel)) => s.files.insert(rel.to_string()),
            Some(("E", rel)) => s.empty_dirs.insert(rel.to_string()),
            _ => {
                malformed += 1;
                false
            }
        };
    }
    if malformed > 0 {
        eprintln!("nix-ninja: {malformed} unreadable line(s) in the configure record skipped");
    }
    Some(s)
}

/// Writes `s` as this configure's record. A failure is reported, not fatal:
/// without a record a later pass reads the directory as it stands, as every
/// pass did before.
pub fn save(build_dir: &Path, s: &ConfigureState) {
    save_in(&state_dir(build_dir), build_dir, s)
}

fn save_in(state: &Path, build_dir: &Path, s: &ConfigureState) {
    // A name holding a newline cannot be one line; it is left out, so a
    // later pass treats it as something configure did not leave.
    let lines: Vec<String> = s
        .dirs
        .iter()
        .map(|d| format!("D {d}"))
        .chain(s.files.iter().map(|f| format!("F {f}")))
        .chain(s.empty_dirs.iter().map(|e| format!("E {e}")))
        .filter(|l| !l.contains('\n'))
        .collect();
    if let Err(e) = write_lines(&state.join("configure.v1"), build_dir, &lines, false) {
        eprintln!(
            "nix-ninja: could not record the configure-time build directory ({e}); \
             a later pass reads the directory as it stands"
        );
    }
}

/// The paths a run's placement wrote, as `journal_placed` takes them: every
/// output an edge produces (a Built entry; the walk's and the scan's uploads
/// are Opaque and were never placed) and every collected depfile.
pub fn placed_paths<'a>(
    outputs: impl IntoIterator<Item = &'a DerivedFile>,
    depfiles: &'a [DerivedFile],
) -> Vec<&'a Path> {
    outputs
        .into_iter()
        .filter(|d| matches!(d.derived_path, SingleDerivedPath::Built { .. }))
        .chain(depfiles)
        .map(|d| d.build_path.as_path())
        .collect()
}

/// lstat's (mtime, size) of a placed file, as the journal records it: a
/// later pass sets aside only a file still exactly as placement left it.
fn stamp(meta: &std::fs::Metadata) -> String {
    format!("{}.{} {}", meta.mtime(), meta.mtime_nsec(), meta.len())
}

/// Journals what this run's placement wrote: each of `placed` (relative
/// build paths) that exists, every file below one that is a directory, and
/// every ancestor directory configure did not leave; a file with the stamp
/// it was placed with. Called only once placement has succeeded. `held`
/// says the caller's `Rewind` already holds the build dir's lock.
/// DEFER(a placed output outside the build dir carries a placeholder a later
/// pass reads): paths with `..` or absolute spellings are not journaled.
pub fn journal_placed<'a>(
    build_dir: &Path,
    placed: impl IntoIterator<Item = &'a Path>,
    held: bool,
) {
    let state = state_dir(build_dir);
    let _lock = if held { None } else { lock_blocking(&state) };
    if !held && _lock.is_none() {
        eprintln!(
            "nix-ninja: could not lock {} to journal this run's placement; journaling \
             unlocked",
            state.display()
        );
    }
    journal_in(&state, build_dir, placed)
}

fn lock_blocking(state: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(state).ok()?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("lock"))
        .ok()?;
    f.lock().ok()?;
    Some(f)
}

fn journal_in<'a>(state: &Path, build_dir: &Path, placed: impl IntoIterator<Item = &'a Path>) {
    let Some(r) = load_in(state, build_dir) else {
        return;
    };
    let path = state.join("placed.v1");
    // Keyed by kind and path, so a later pass's placement replaces the stamp
    // an earlier one recorded.
    let mut entries: std::collections::BTreeMap<(char, String), String> =
        read_lines(&path, build_dir)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|l| parse_journal(&l).map(|(k, rel, _)| ((k, rel.to_string()), l.clone())))
            .collect();
    let mut add = |rel: &Path, meta: &std::fs::Metadata| {
        let key = rel.to_string_lossy().into_owned();
        if key.contains('\n') {
            return;
        }
        if meta.is_dir() {
            if !r.dirs.contains(&key) {
                entries.insert(('D', key.clone()), format!("D {key}"));
            }
        } else if !r.files.contains(&key) {
            entries.insert(('F', key.clone()), format!("F {} {key}", stamp(meta)));
        }
    };
    for rel in placed {
        if !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            continue;
        }
        let abs = build_dir.join(rel);
        let Ok(meta) = std::fs::symlink_metadata(&abs) else {
            continue;
        };
        let mut anc = rel.parent();
        while let Some(a) = anc.filter(|a| !a.as_os_str().is_empty()) {
            if let Ok(m) = std::fs::symlink_metadata(build_dir.join(a)) {
                add(a, &m);
            }
            anc = a.parent();
        }
        if meta.is_dir() {
            for e in walkdir::WalkDir::new(&abs).into_iter().flatten() {
                if let (Ok(r), Ok(m)) = (e.path().strip_prefix(build_dir), e.metadata()) {
                    add(r, &m);
                }
            }
        } else {
            add(rel, &meta);
        }
    }
    // EVERY PATH PLACEMENT REPLACED, with the stamp of what is there now;
    // one holding exactly configure's version is no change (a generator
    // that rewrote identical bytes) and is not journaled.
    let orig = originals(state);
    for e in walkdir::WalkDir::new(&orig)
        .min_depth(1)
        .into_iter()
        .flatten()
    {
        let Ok(rel) = e.path().strip_prefix(&orig) else {
            continue;
        };
        let key = rel.to_string_lossy().into_owned();
        if e.file_type().is_dir() || !confined(&key) || key.contains('\n') {
            continue;
        }
        let now = build_dir.join(rel);
        entries.remove(&('O', key.clone()));
        if same_entry(&now, e.path()) {
            continue;
        }
        if let Ok(m) = std::fs::symlink_metadata(&now) {
            entries.insert(('O', key.clone()), format!("O {} {key}", stamp(&m)));
        }
    }
    let lines: Vec<String> = entries.into_values().collect();
    if let Err(e) = write_lines(&path, build_dir, &lines, false) {
        eprintln!(
            "nix-ninja: could not journal this run's placement ({e}); a later pass \
             reads what it placed"
        );
    }
}

/// Whether `rel` stays inside the build dir: plain names only, no root, no
/// `..`, no `.`. Every path read back from the journal is checked, so a
/// damaged journal cannot move a file outside the tree.
fn confined(rel: &str) -> bool {
    !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// One journal line: its kind, its path, and for a file its stamp. A path
/// that would leave the build dir is not a journal line.
fn parse_journal(line: &str) -> Option<(char, &str, Option<&str>)> {
    let parsed = parse_journal_unchecked(line)?;
    confined(parsed.1).then_some(parsed)
}

fn parse_journal_unchecked(line: &str) -> Option<(char, &str, Option<&str>)> {
    match line.split_once(' ')? {
        ("D", rel) => Some(('D', rel, None)),
        (k @ ("F" | "O" | "R"), rest) => {
            let (mtime, rest) = rest.split_once(' ')?;
            let (size, rel) = rest.split_once(' ')?;
            let st = &line[2..2 + mtime.len() + 1 + size.len()];
            Some((k.chars().next()?, rel, Some(st)))
        }
        _ => None,
    }
}

/// The later pass's view of the build dir as the first pass found it, held
/// for the life of the value; dropping it puts back whatever was moved aside
/// and not placed again, and the directories removed with it.
pub struct Rewind {
    build_dir: PathBuf,
    aside: PathBuf,
    removed_dirs: Vec<PathBuf>,
    /// Where `removed_dirs` is kept for a re-exec or a run that never ends.
    removed_file: PathBuf,
    /// Configure's versions put back over placed paths, with the stamp each
    /// was put back with: one still carrying it gives way to the placed one.
    restored: Vec<(PathBuf, String)>,
    /// Where `restored` is kept, as `removed_file` keeps `removed_dirs`.
    restored_file: PathBuf,
    /// Configure's versions of the paths placement replaced.
    originals: PathBuf,
    /// `finish` already put everything back.
    done: bool,
    /// Held exclusively for the rewind's life: a second driver in the same
    /// build dir meanwhile neither sets aside nor puts back.
    _lock: Option<std::fs::File>,
}

impl Rewind {
    /// Puts back anything a run that never finished left aside, then, when
    /// earlier invocations' state is not trusted, moves aside what earlier
    /// runs journaled as placed. None when there is nothing to do.
    pub fn begin(build_dir: &Path, trusted: bool) -> Option<Self> {
        Self::begin_in(&state_dir(build_dir), build_dir, trusted)
    }

    fn begin_in(state: &Path, build_dir: &Path, trusted: bool) -> Option<Self> {
        let lock = std::fs::create_dir_all(state).and_then(|_| {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(state.join("lock"))
        });
        let lock = match lock {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "nix-ninja: no rewind state at {} ({e}); this pass reads the \
                     directory as it stands",
                    state.display()
                );
                return None;
            }
        };
        if lock.try_lock().is_err() {
            eprintln!(
                "nix-ninja: another driver holds this build directory's rewind; this one \
                 reads the directory as it stands"
            );
            return None;
        }
        let mut rewind = Rewind {
            build_dir: build_dir.to_path_buf(),
            aside: state.join("aside"),
            removed_dirs: read_removed(state),
            removed_file: state.join("removed-dirs"),
            restored: std::fs::read_to_string(state.join("restored"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| match parse_journal(l)? {
                    ('R', rel, Some(st)) => Some((PathBuf::from(rel), st.to_string())),
                    _ => None,
                })
                .collect(),
            restored_file: state.join("restored"),
            originals: originals(state),
            done: false,
            _lock: Some(lock),
        };
        if let Err(e) = rewind.restore() {
            eprintln!("nix-ninja: {e}");
        }
        if trusted {
            return None;
        }
        let journal_path = state.join("placed.v1");
        if journal_path.exists() && read_lines(&journal_path, build_dir).is_none() {
            eprintln!(
                "nix-ninja: the placement journal is for another build.ninja (a \
                 re-configure); this pass reads the directory as it stands"
            );
        }
        let journal = read_lines(&journal_path, build_dir)?;
        let mut moved = 0usize;
        let mut changed = 0usize;
        let mut failed: Option<(PathBuf, std::io::Error)> = None;
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut replaced: Vec<(&str, &str)> = Vec::new();
        for line in &journal {
            match parse_journal(line) {
                Some(('F', rel, placed_stamp)) => {
                    let (src, dest) = (build_dir.join(rel), rewind.aside.join(rel));
                    let Ok(meta) = std::fs::symlink_metadata(&src) else {
                        continue;
                    };
                    // A FILE CHANGED SINCE PLACEMENT IS NOT PLACEMENT'S ANY
                    // MORE (a hook edited it, or replaced it with a
                    // directory): it stays visible, as it was before.
                    if placed_stamp != Some(stamp(&meta).as_str()) {
                        changed += 1;
                        continue;
                    }
                    let res = dest
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|_| std::fs::rename(&src, &dest));
                    match res {
                        Ok(()) => moved += 1,
                        Err(e) => {
                            failed.get_or_insert((PathBuf::from(rel), e));
                        }
                    }
                }
                Some(('D', rel, _)) => dirs.push(PathBuf::from(rel)),
                Some(('O', rel, Some(st))) => replaced.push((rel, st)),
                _ => {}
            }
        }
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for rel in dirs {
            // Only an emptied directory goes, and it comes back on drop.
            if std::fs::remove_dir(build_dir.join(&rel)).is_ok() {
                rewind.removed_dirs.push(rel);
            }
        }
        // A PATH CONFIGURE LEFT GETS CONFIGURE'S VERSION BACK, after the
        // directories (one placed where configure left a file is gone by now).
        for (rel, placed_stamp) in replaced {
            let (src, dest) = (build_dir.join(rel), rewind.aside.join(rel));
            let res = match std::fs::symlink_metadata(&src) {
                Err(_) => Ok(()),
                Ok(m) if !m.is_dir() && placed_stamp == stamp(&m) => dest
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|_| std::fs::rename(&src, &dest)),
                Ok(_) => {
                    changed += 1;
                    continue;
                }
            };
            let orig = rewind.originals.join(rel);
            let res = res.and_then(|_| {
                let meta = std::fs::symlink_metadata(&orig)?;
                src.parent().map_or(Ok(()), std::fs::create_dir_all)?;
                copy_entry(&orig, &src, &meta)?;
                std::fs::symlink_metadata(&src)
            });
            match res {
                Ok(m) => {
                    rewind.restored.push((PathBuf::from(rel), stamp(&m)));
                    moved += 1;
                }
                Err(e) => {
                    failed.get_or_insert((PathBuf::from(rel), e));
                }
            }
        }
        let list: String = rewind
            .removed_dirs
            .iter()
            .map(|d| format!("{}\n", d.display()))
            .collect();
        let restored: String = rewind
            .restored
            .iter()
            .map(|(rel, st)| format!("R {st} {}\n", rel.display()))
            .collect();
        if let Err(e) = std::fs::write(&rewind.removed_file, list)
            .and_then(|_| std::fs::write(&rewind.restored_file, restored))
        {
            eprintln!(
                "nix-ninja: could not record what this pass set aside ({e}); a run that \
                 dies before it ends leaves it aside"
            );
        }
        eprintln!(
            "nix-ninja: an earlier pass placed outputs in this build directory; {moved} \
             file(s) and {} directory(ies) it placed are set aside until this pass has \
             placed its own, {} of them in place of configure's version; {changed} \
             changed since placement stay",
            rewind.removed_dirs.len(),
            rewind.restored.len()
        );
        if let Some((rel, e)) = failed {
            eprintln!(
                "nix-ninja: could not set aside every placed file (first: {}: {e}); this \
                 pass reads those it could not",
                rel.display()
            );
        }
        Some(rewind)
    }

    /// Moves back every file set aside whose place is still empty (a file
    /// this pass placed again stays as placed), and recreates the
    /// directories set aside with them.
    fn restore(&mut self) -> std::io::Result<()> {
        for (rel, put_back) in self.restored.drain(..) {
            let dest = self.build_dir.join(&rel);
            if std::fs::symlink_metadata(&dest).is_ok_and(|m| stamp(&m) == put_back) {
                let _ = std::fs::remove_file(&dest);
            }
        }
        let _ = std::fs::remove_file(&self.restored_file);
        for rel in self.removed_dirs.drain(..) {
            let _ = std::fs::create_dir_all(self.build_dir.join(rel));
        }
        let _ = std::fs::remove_file(&self.removed_file);
        if std::fs::symlink_metadata(&self.aside).is_err() {
            return Ok(());
        }
        let mut failed: Option<(PathBuf, std::io::Error)> = None;
        let walk = walkdir::WalkDir::new(&self.aside)
            .min_depth(1)
            .contents_first(true);
        for e in walk.into_iter().flatten() {
            let Ok(rel) = e.path().strip_prefix(&self.aside) else {
                continue;
            };
            if !confined(&rel.to_string_lossy()) {
                continue;
            }
            if e.file_type().is_dir() {
                let _ = std::fs::create_dir_all(self.build_dir.join(rel));
                let _ = std::fs::remove_dir(e.path());
                continue;
            }
            let dest = self.build_dir.join(rel);
            if std::fs::symlink_metadata(&dest).is_ok() {
                let _ = std::fs::remove_file(e.path());
                continue;
            }
            let res = dest
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::rename(e.path(), &dest));
            if let Err(err) = res {
                failed.get_or_insert((rel.to_path_buf(), err));
            }
        }
        let _ = std::fs::remove_dir(&self.aside);
        match failed {
            None => Ok(()),
            Some((rel, e)) => Err(std::io::Error::other(format!(
                "could not put every set-aside file back (first: {}: {e}); the rest \
                 wait in {}",
                rel.display(),
                self.aside.display()
            ))),
        }
    }

    /// Puts everything back and says whether it all came back: the run's last
    /// step, so a file left aside fails the build rather than going missing
    /// from what the installer sees.
    pub fn finish(mut self) -> std::io::Result<()> {
        let r = self.restore();
        self.done = true;
        r
    }
}

impl Drop for Rewind {
    fn drop(&mut self) {
        if !self.done {
            if let Err(e) = self.restore() {
                eprintln!("nix-ninja: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        journal_in, keep_originals_in, load_in, placed_paths, save_in, state_dir_in,
        ConfigureState, Rewind,
    };
    use harmonia_store_derivation::derived_path::SingleDerivedPath;
    use harmonia_store_path::StoreDir;
    use nix_ninja_task::derived_file::DerivedFile;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nn-cfgstate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn write(p: &Path, v: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, v).unwrap();
    }

    /// A record answers only for the build.ninja it was written against:
    /// read back unchanged, refused once build.ninja changes (a re-configure).
    #[test]
    fn a_record_answers_for_its_own_configure_only() {
        let d = scratch("roundtrip");
        let state = d.join("state");
        write(&d.join("build.ninja"), "default all\n");
        let s = ConfigureState {
            dirs: ["CMakeFiles/foo.dir".to_string()].into(),
            files: ["build.ninja".to_string(), "config.h".to_string()].into(),
            empty_dirs: ["CMakeFiles/foo.dir".to_string()].into(),
        };
        assert_eq!(load_in(&state, &d), None, "no record before the first pass");
        save_in(&state, &d, &s);
        assert_eq!(load_in(&state, &d), Some(s));
        write(&d.join("build.ninja"), "default all app\n");
        assert_eq!(
            load_in(&state, &d),
            None,
            "a changed build.ninja is a new configure"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Two build dirs under one NIX_BUILD_TOP get two state directories.
    #[test]
    fn each_build_dir_has_its_own_state() {
        let top = Path::new("/build");
        assert_ne!(
            state_dir_in(Some(top), Path::new("/build/a/build")),
            state_dir_in(Some(top), Path::new("/build/b/build"))
        );
    }

    /// THE CALL SITE'S SHAPE: the driver's output map holds the walk's and
    /// the scan's uploads (Opaque) beside edge outputs (Built). Only Built
    /// entries and collected depfiles were placed; journaling an upload
    /// would set aside a file a hook wrote between passes.
    #[test]
    fn only_edge_outputs_and_depfiles_are_journaled() {
        let store = StoreDir::new(Path::new("/nix/store")).unwrap();
        let sp = |n: &str| {
            store
                .parse(&format!("/nix/store/caia63n1jh7d05nkq8mr30392vv3q170-{n}"))
                .unwrap()
        };
        let built = |p: &str| DerivedFile {
            derived_path: SingleDerivedPath::Built {
                drv_path: Arc::new(SingleDerivedPath::Opaque(sp("ninja-build.drv"))),
                output: "out".parse().unwrap(),
            },
            build_path: PathBuf::from(p),
            rel_path: None,
        };
        let upload = |p: &str| DerivedFile {
            derived_path: SingleDerivedPath::Opaque(sp("x")),
            build_path: PathBuf::from(p),
            rel_path: None,
        };
        let outputs = [
            built("foo.o"),
            upload("Testing/Temporary/LastTest.log"),
            built("app"),
        ];
        let depfiles = [built("foo.o.d")];
        let got = placed_paths(outputs.iter(), &depfiles);
        assert_eq!(got, ["foo.o", "app", "foo.o.d"].map(Path::new));
    }

    /// THE QT SHAPE, end to end through the journal. Configure left
    /// `include/Foo/fooexports.h` and an empty `CMakeFiles/foo.dir`; the
    /// build pass placed a whole tree output `include/Foo` (its interior
    /// `a.h` and `b.h` no edge declares), an object into the empty
    /// directory, a header in a directory it created, and an empty tree
    /// output `emptyout`. Between phases a hook wrote `docs/index.html` and
    /// EDITED the placed `include/Foo/b.h`. The later pass sets aside
    /// exactly the placed paths still as placed: configure's file, the
    /// hook's file and the edited one stay. Dropping the rewind puts back
    /// what this pass did not place again, keeps what it did, and recreates
    /// `emptyout`, which only the restore can bring back.
    #[test]
    fn a_later_pass_sets_aside_exactly_what_placement_wrote() {
        let d = scratch("journal");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        write(&bd.join("include/Foo/fooexports.h"), "cfg");
        std::fs::create_dir_all(bd.join("CMakeFiles/foo.dir")).unwrap();
        save_in(
            &state,
            &bd,
            &ConfigureState {
                dirs: ["include", "include/Foo", "CMakeFiles", "CMakeFiles/foo.dir"]
                    .map(String::from)
                    .into(),
                files: ["build.ninja", "include/Foo/fooexports.h"]
                    .map(String::from)
                    .into(),
                empty_dirs: ["CMakeFiles/foo.dir"].map(String::from).into(),
            },
        );
        write(&bd.join("include/Foo/a.h"), "placed");
        write(&bd.join("include/Foo/b.h"), "placed");
        write(&bd.join("CMakeFiles/foo.dir/foo.c.o"), "placed");
        write(&bd.join("gen/sub/y.h"), "placed");
        std::fs::create_dir_all(bd.join("emptyout")).unwrap();
        let placed = [
            "include/Foo",
            "CMakeFiles/foo.dir/foo.c.o",
            "gen/sub/y.h",
            "emptyout",
        ]
        .map(PathBuf::from);
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path));
        write(&bd.join("docs/index.html"), "hook");
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(&bd.join("include/Foo/b.h"), "HOOK-EDITED");

        let rewind = Rewind::begin_in(&state, &bd, false).expect("a journal to act on");
        for gone in [
            "include/Foo/a.h",
            "CMakeFiles/foo.dir/foo.c.o",
            "gen/sub/y.h",
            "gen",
            "emptyout",
        ] {
            assert!(!bd.join(gone).exists(), "{gone} should be set aside");
        }
        for kept in [
            "include/Foo/fooexports.h",
            "include/Foo/b.h",
            "docs/index.html",
            "CMakeFiles/foo.dir",
        ] {
            assert!(bd.join(kept).exists(), "{kept} should stay");
        }
        assert!(
            Rewind::begin_in(&state, &bd, false).is_none(),
            "a second driver in the same build dir leaves the rewind alone"
        );
        assert!(
            !bd.join("include/Foo/a.h").exists(),
            "and puts nothing back"
        );
        // The pass places one output again; the rest come back at the end.
        write(&bd.join("gen/sub/y.h"), "new");
        rewind.finish().expect("everything comes back");
        assert_eq!(
            std::fs::read_to_string(bd.join("gen/sub/y.h")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(bd.join("include/Foo/a.h")).unwrap(),
            "placed"
        );
        assert_eq!(
            std::fs::read_to_string(bd.join("include/Foo/b.h")).unwrap(),
            "HOOK-EDITED",
            "a hook's edit to a placed file survives"
        );
        assert!(bd.join("CMakeFiles/foo.dir/foo.c.o").exists());
        assert!(
            bd.join("emptyout").is_dir(),
            "a removed empty directory is recreated"
        );
        assert!(
            Rewind::begin_in(&state, &bd, true).is_none(),
            "a run that trusts earlier state never rewinds"
        );
        assert!(bd.join("include/Foo/a.h").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A PATH CONFIGURE LEFT THAT PLACEMENT REPLACED READS AS CONFIGURE LEFT
    /// IT on a later pass, and as placed once the pass ends. qtbase's shape:
    /// configure writes a 0-byte `qt6core_metatypes.json`, the moc edge's
    /// output lands on it, and the install pass uploaded the placed copy
    /// where the build pass had uploaded the placeholder, re-keying syncqt
    /// and every compile behind it. Beside it: a file replaced inside a
    /// directory configure left, a symlink replaced by a file and a file by
    /// a directory (the type comes back), an executable replaced by a plain
    /// file (the mode comes back), a file rewritten with identical bytes
    /// (left alone), and a path the later pass places again (its placement
    /// stands).
    #[test]
    fn a_path_configure_left_reads_as_configure_left_it_on_a_later_pass() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let d = scratch("originals");
        let bd = d.join("build");
        let state = d.join("state");
        let json = "src/corelib/qt6core_metatypes.json";
        write(&bd.join("build.ninja"), "default all\n");
        write(&bd.join(json), "");
        write(&bd.join("include/Foo/cfg.h"), "configure");
        write(&bd.join("lib/libx.so.1"), "elf");
        std::os::unix::fs::symlink("libx.so.1", bd.join("lib/libx.so")).unwrap();
        write(&bd.join("tool"), "#!/bin/sh\n");
        std::fs::set_permissions(bd.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&bd.join("same.h"), "same");
        write(&bd.join("tree"), "configure tree");
        write(&bd.join("again.h"), "configure again");
        let record = ConfigureState {
            dirs: ["src", "src/corelib", "include", "include/Foo", "lib"]
                .map(String::from)
                .into(),
            files: [
                "build.ninja",
                json,
                "include/Foo/cfg.h",
                "lib/libx.so.1",
                "lib/libx.so",
                "tool",
                "same.h",
                "tree",
                "again.h",
            ]
            .map(String::from)
            .into(),
            empty_dirs: Default::default(),
        };
        save_in(&state, &bd, &record);
        let placed = [
            json,
            "include/Foo",
            "lib/libx.so",
            "tool",
            "same.h",
            "tree",
            "again.h",
        ]
        .map(PathBuf::from);
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path));

        // Placement.
        write(&bd.join(json), &"m".repeat(385));
        write(&bd.join("include/Foo/cfg.h"), "placed");
        write(&bd.join("include/Foo/new.h"), "placed");
        std::fs::remove_file(bd.join("lib/libx.so")).unwrap();
        write(&bd.join("lib/libx.so"), "placed elf");
        std::fs::remove_file(bd.join("tool")).unwrap();
        write(&bd.join("tool"), "placed tool");
        write(&bd.join("same.h"), "same");
        std::fs::remove_file(bd.join("tree")).unwrap();
        write(&bd.join("tree/a.h"), "placed");
        write(&bd.join("again.h"), "placed again");
        let same_ino = std::fs::metadata(bd.join("same.h")).unwrap().ino();
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path));
        let journal = std::fs::read_to_string(state.join("placed.v1")).unwrap();
        assert!(
            !journal.lines().any(|l| l.ends_with(" same.h")),
            "identical bytes are no change:\n{journal}"
        );

        let rewind = Rewind::begin_in(&state, &bd, false).expect("a journal to act on");
        let read = |rel: &str| std::fs::read_to_string(bd.join(rel)).unwrap();
        assert_eq!(
            read(json),
            "",
            "configure's placeholder, not the placed json"
        );
        assert_eq!(read("include/Foo/cfg.h"), "configure");
        assert!(!bd.join("include/Foo/new.h").exists());
        assert_eq!(
            std::fs::read_link(bd.join("lib/libx.so")).unwrap(),
            Path::new("libx.so.1"),
            "configure's symlink comes back as a symlink"
        );
        assert_eq!(read("tool"), "#!/bin/sh\n");
        assert_ne!(
            std::fs::metadata(bd.join("tool")).unwrap().mode() & 0o111,
            0,
            "with its executable bit"
        );
        assert!(
            bd.join("tree").is_file(),
            "a file again where a directory was placed"
        );
        assert_eq!(read("tree"), "configure tree");
        assert_eq!(
            std::fs::metadata(bd.join("same.h")).unwrap().ino(),
            same_ino,
            "an identical file is not moved"
        );
        assert_eq!(read("again.h"), "configure again");
        // The pass places one path again; the rest come back at the end.
        std::fs::remove_file(bd.join("again.h")).unwrap();
        write(&bd.join("again.h"), "placed by the later pass");
        rewind.finish().expect("everything comes back");
        assert_eq!(read(json), "m".repeat(385));
        assert_eq!(read("include/Foo/cfg.h"), "placed");
        assert_eq!(read("include/Foo/new.h"), "placed");
        assert!(!bd.join("lib/libx.so").is_symlink());
        assert_eq!(read("lib/libx.so"), "placed elf");
        assert_eq!(read("tool"), "placed tool");
        assert_eq!(read("tree/a.h"), "placed");
        assert_eq!(read("again.h"), "placed by the later pass");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A later pass's directory listing reads only what configure left
    /// under the build dir, whatever the spelling; outside it, and on a first
    /// pass, it reads everything.
    #[test]
    fn a_later_listing_reads_only_what_configure_left() {
        let bd = Path::new("/build/pkg/build");
        let files: std::collections::HashSet<String> = ["meta/d/meta.json".to_string()].into();
        for (path, read) in [
            ("meta/d/meta.json", true),
            ("./meta/d/meta.json", true),
            ("/build/pkg/build/meta/d/meta.json", true),
            ("meta/d/w123.tmp", false),
            ("/build/pkg/build/meta/d/w123.tmp", false),
            ("../src/x.h", true),
            ("/nix/store/x-y/z.h", true),
        ] {
            assert_eq!(
                super::left_by(Some(&files), bd, Path::new(path)),
                read,
                "{path}"
            );
        }
        assert!(super::left_by(None, bd, Path::new("meta/d/w123.tmp")));
    }

    /// A damaged journal cannot move a file outside the build dir: a line
    /// whose path climbs out, is absolute or is empty is no journal line.
    #[test]
    fn a_journal_path_outside_the_build_dir_is_not_read() {
        use super::parse_journal;
        assert!(parse_journal("F 1.2 3 include/a.h").is_some());
        for bad in [
            "F 1.2 3 ../../etc/passwd",
            "F 1.2 3 /etc/passwd",
            "D ..",
            "D ./x",
            "D ",
        ] {
            assert!(parse_journal(bad).is_none(), "{bad}");
        }
    }
}
