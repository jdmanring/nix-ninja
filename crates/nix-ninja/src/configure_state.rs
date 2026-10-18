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

/// A later pass's record of what configure left, set by the walk
/// (`answer_listings_from`) and asked by every other listing of a build-tree
/// directory (`left_by_configure`). Unset on a first pass, so nothing a
/// first pass emits depends on it.
static LISTED: std::sync::Mutex<Option<ConfigureState>> = std::sync::Mutex::new(None);

/// Makes this process's listings of build-tree directories answer from
/// configure's record: called by a later pass's walk.
pub fn answer_listings_from(record: ConfigureState) {
    *LISTED.lock().unwrap() = Some(record);
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

fn left_by(record: Option<&ConfigureState>, build_dir: &Path, path: &Path) -> bool {
    record.is_none_or(|r| left_in(r, build_dir, path))
}

fn left_in(r: &ConfigureState, build_dir: &Path, path: &Path) -> bool {
    // THE RECORD KNOWS ONLY WHAT THE WALK ENTERED: not a directory the walk
    // skips (`walk_skips`), not what lies below a symlink (recorded as a
    // file, so a recorded file with more of the path below it is one), and
    // nothing outside the build dir, which the listing reads.
    match landing(
        build_dir,
        path,
        |d| r.dirs.contains(d),
        |l| r.files.contains(l),
    ) {
        Landing::Inside(rel) => {
            rel.is_empty() || r.files.contains(&rel) || rel.split('/').any(walk_skips)
        }
        Landing::Outside => true,
        Landing::Absent => false,
    }
}

/// Where a spelling lands, as `landing` answers it.
#[derive(Debug, PartialEq, Eq)]
pub enum Landing {
    /// Under the build dir, as the walk spells it (`""` for the build dir).
    Inside(String),
    /// Outside it, or through a link that does not resolve: the disk
    /// answers, as on the first pass.
    Outside,
    /// Nowhere on the first pass: a `..` after a name configure left no
    /// directory at.
    Absent,
}

/// Where `spelled` (relative to `build_dir`, or absolute) lands, ONE ANSWER
/// FOR EVERY PASS. It climbs as the kernel does: from the build dir's real
/// location, through every symlink configure left (`left_link`) with more
/// of the spelling to follow (a `..` after one climbs from its target, and
/// one leaving the tree is followed back in if the spelling returns), and
/// `..` after a name is a climb only where configure left a directory
/// (`left_dir`) there, since otherwise the first pass found nothing.
pub fn landing(
    build_dir: &Path,
    spelled: &Path,
    left_dir: impl Fn(&str) -> bool,
    left_link: impl Fn(&str) -> bool,
) -> Landing {
    let bd = std::fs::canonicalize(build_dir).unwrap_or_else(|_| build_dir.to_path_buf());
    // An absolute spelling is climbed from the root, lexically outside the
    // tree; under it, the tree's own spelling or its real location.
    let inside = |p: &Path| -> Option<String> {
        let r = p
            .strip_prefix(&bd)
            .or_else(|_| p.strip_prefix(build_dir))
            .ok()?;
        Some(normal_rel(r).unwrap_or_default())
    };
    let comps: Vec<_> = spelled.components().collect();
    let mut cur = if spelled.is_absolute() {
        PathBuf::from("/")
    } else {
        bd.clone()
    };
    let mut hops = 0;
    for (i, c) in comps.iter().enumerate() {
        match c {
            std::path::Component::ParentDir => {
                cur.pop();
                continue;
            }
            std::path::Component::Normal(n) => cur.push(n),
            _ => continue,
        }
        let more = &comps[i + 1..];
        if more.is_empty() {
            break;
        }
        let up = more.first() == Some(&std::path::Component::ParentDir);
        let Some(rel) = inside(&cur) else {
            // Outside the tree nothing changes between phases: the disk
            // answers what the kernel would.
            if cur.is_symlink() {
                hops += 1;
                match std::fs::canonicalize(&cur) {
                    Ok(t) if hops <= 40 => cur = t,
                    _ => return Landing::Outside,
                }
            } else if up && !cur.is_dir() {
                return Landing::Absent;
            }
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        if left_link(&rel) {
            hops += 1;
            match std::fs::canonicalize(&cur) {
                Ok(t) if hops <= 40 => cur = t,
                _ => return Landing::Outside,
            }
        } else if up && !left_dir(&rel) {
            return Landing::Absent;
        }
    }
    inside(&cur).map_or(Landing::Outside, Landing::Inside)
}

/// `rel` as the walk spells it (plain names joined by `/`, no `.`, no empty
/// component), or None for a path that is empty or leaves the tree.
fn normal_rel(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            std::path::Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
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
/// AN ERROR WHERE A LATER PASS WOULD READ THE PLACED FILE: configure's
/// copy that cannot be kept, or a path configure left that is gone before
/// placement replaces it.
pub fn keep_originals<'a>(
    build_dir: &Path,
    placed: impl IntoIterator<Item = &'a Path>,
) -> std::io::Result<()> {
    let state = state_dir(build_dir);
    match load_in(&state, build_dir) {
        Some(r) => keep_originals_in(&state, build_dir, &r, placed),
        None => Ok(()),
    }
}

fn keep_originals_in<'a>(
    state: &Path,
    build_dir: &Path,
    r: &ConfigureState,
    placed: impl IntoIterator<Item = &'a Path>,
) -> std::io::Result<()> {
    let orig = originals(state);
    let mut failed: Option<(PathBuf, std::io::Error)> = None;
    let mut keep = |rel: &Path| {
        let Some(key) = normal_rel(rel).filter(|k| r.files.contains(k)) else {
            return;
        };
        let (src, dest) = (build_dir.join(&key), orig.join(&key));
        if std::fs::symlink_metadata(&dest).is_ok() {
            return;
        }
        let res = std::fs::symlink_metadata(&src).and_then(|meta| {
            if !meta.is_file() && !meta.file_type().is_symlink() {
                return Ok(());
            }
            dest.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| copy_entry(&src, &dest, &meta))
        });
        if let Err(e) = res {
            failed.get_or_insert((rel.to_path_buf(), e));
        }
    };
    for rel in placed {
        // As the journal spells it, so `./m.json` is kept as `m.json` is.
        let Some(rel) = normal_rel(rel) else {
            continue;
        };
        let rel = Path::new(&rel);
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
            // Configure left it and it is gone: no copy to keep.
            Err(_) => keep(rel),
        }
    }
    match failed {
        None => Ok(()),
        Some((rel, e)) => Err(std::io::Error::other(format!(
            "could not keep configure's copy of {} ({e}), which placement replaces; a \
             later pass would read the placed one there",
            rel.display()
        ))),
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

/// `contents` at `path` through a name of its own and a rename, so a run
/// killed meanwhile leaves the old file or the new one, never a part.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)
        .and_then(|_| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
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
    // A new record is a new configure: what an earlier one left at a path
    // placement replaced is not this configure's. Not while a pass holds
    // files or directories aside: its restore asks these copies what gives
    // way.
    let orig = originals(state);
    if ["aside", "removed-dirs"]
        .iter()
        .any(|n| std::fs::symlink_metadata(state.join(n)).is_ok())
    {
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(&orig) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "nix-ninja: could not clear {} ({e}); a later pass may read an \
                 earlier configure's file where placement replaced one",
                orig.display()
            );
        }
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
/// says the caller's `Rewind` already holds the build dir's lock. AN ERROR
/// when the journal cannot be written: a later pass would read what this
/// one placed.
/// DEFER(a placed output outside the build dir carries a placeholder a later
/// pass reads): paths with `..` or absolute spellings are not journaled.
pub fn journal_placed<'a>(
    build_dir: &Path,
    placed: impl IntoIterator<Item = &'a Path>,
    held: bool,
) -> std::io::Result<()> {
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

fn journal_in<'a>(
    state: &Path,
    build_dir: &Path,
    placed: impl IntoIterator<Item = &'a Path>,
) -> std::io::Result<()> {
    let Some(r) = load_in(state, build_dir) else {
        return Ok(());
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
    let mut placed_keys: Vec<String> = Vec::new();
    for rel in placed {
        let Some(key) = normal_rel(rel) else {
            continue;
        };
        let rel = Path::new(&key);
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
        placed_keys.push(key.clone());
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
        // ONLY THIS PASS'S PLACEMENT MOVES A LINE, as for every other kind:
        // a path it did not place keeps the earlier pass's line, whether the
        // pass put configure's version back there or a hook edited it (that
        // edit stays visible to every later pass, as an edited F path does).
        let placed_here = placed_keys.iter().any(|p| {
            key == *p
                || key
                    .strip_prefix(p.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        });
        if !placed_here {
            continue;
        }
        let now = build_dir.join(rel);
        // Configure's version at the path is no change: a generator that
        // wrote identical bytes, or a pass that covered the path (a tree
        // around it) without writing it. An earlier pass's line stays.
        if same_entry(&now, e.path()) {
            continue;
        }
        entries.remove(&('O', key.clone()));
        if let Ok(m) = std::fs::symlink_metadata(&now) {
            entries.insert(('O', key.clone()), format!("O {} {key}", stamp(&m)));
        }
    }
    let lines: Vec<String> = entries.into_values().collect();
    write_lines(&path, build_dir, &lines, false).map_err(|e| {
        std::io::Error::other(format!(
            "could not journal this run's placement ({e}); a later pass would read \
             what it placed"
        ))
    })
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
        (k @ ("F" | "O"), rest) => {
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
    /// Configure's versions of the paths placement replaced.
    originals: PathBuf,
    /// How many of them this pass put back.
    restored: usize,
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
    /// AN ERROR WHERE THIS PASS WOULD READ ANOTHER TREE THAN THE FIRST: a
    /// placed file that cannot be set aside, configure's version that cannot
    /// be put back, a record of removed directories that cannot be written.
    /// Everything already moved comes back before it is returned.
    pub fn begin(build_dir: &Path, trusted: bool) -> std::io::Result<Option<Self>> {
        Self::begin_in(&state_dir(build_dir), build_dir, trusted)
    }

    fn begin_in(state: &Path, build_dir: &Path, trusted: bool) -> std::io::Result<Option<Self>> {
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
                return Ok(None);
            }
        };
        if lock.try_lock().is_err() {
            eprintln!(
                "nix-ninja: another driver holds this build directory's rewind; this one \
                 reads the directory as it stands"
            );
            return Ok(None);
        }
        let mut rewind = Rewind {
            build_dir: build_dir.to_path_buf(),
            aside: state.join("aside"),
            removed_dirs: read_removed(state),
            removed_file: state.join("removed-dirs"),
            originals: originals(state),
            restored: 0,
            done: false,
            _lock: Some(lock),
        };
        rewind.restore()?;
        if trusted {
            rewind.done = true;
            return Ok(None);
        }
        let journal_path = state.join("placed.v1");
        if journal_path.exists() && read_lines(&journal_path, build_dir).is_none() {
            eprintln!(
                "nix-ninja: the placement journal is for another build.ninja (a \
                 re-configure); this pass reads the directory as it stands"
            );
        }
        let Some(journal) = read_lines(&journal_path, build_dir) else {
            rewind.done = true;
            return Ok(None);
        };
        // Whatever fails from here, `rewind` drops and puts back what moved.
        let fail = |what: &str, rel: &Path, e: std::io::Error| {
            std::io::Error::other(format!(
                "could not {what} {} ({e}); this pass would read another build \
                 directory than the first, so it stops",
                rel.display()
            ))
        };
        let mut moved = 0usize;
        let mut changed = 0usize;
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
                    dest.parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|_| std::fs::rename(&src, &dest))
                        .map_err(|e| fail("set aside", Path::new(rel), e))?;
                    moved += 1;
                }
                Some(('D', rel, _)) => dirs.push(PathBuf::from(rel)),
                Some(('O', rel, Some(st))) => replaced.push((rel, st)),
                _ => {}
            }
        }
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        // Recorded BEFORE any goes, so a run that dies meanwhile recreates
        // them; recreating one that was never removed changes nothing.
        let candidates: String = rewind
            .removed_dirs
            .iter()
            .chain(dirs.iter().filter(|d| build_dir.join(d).is_dir()))
            .map(|d| format!("{}\n", d.display()))
            .collect();
        write_atomic(&rewind.removed_file, &candidates)
            .map_err(|e| fail("record the directories it removes in", state, e))?;
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
            let orig = rewind.originals.join(rel);
            // A copy left by a run killed between the copy and the rename.
            let name = src.file_name().unwrap_or_default().to_string_lossy();
            let stale = format!(".{name}.nn-orig.");
            if let Ok(entries) = src
                .parent()
                .map_or(Ok(None), |p| std::fs::read_dir(p).map(Some))
            {
                for e in entries.into_iter().flatten().flatten() {
                    if e.file_name().to_string_lossy().starts_with(&stale) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            match std::fs::symlink_metadata(&src) {
                Err(_) => {}
                Ok(m) if !m.is_dir() && placed_stamp == stamp(&m) => {
                    dest.parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|_| std::fs::rename(&src, &dest))
                        .map_err(|e| fail("set aside", Path::new(rel), e))?;
                }
                Ok(_) => {
                    changed += 1;
                    continue;
                }
            }
            // Through a name of its own and a rename, so the path never holds
            // a partial copy that reads as a placement of this pass.
            let res = std::fs::symlink_metadata(&orig).and_then(|meta| {
                src.parent().map_or(Ok(()), std::fs::create_dir_all)?;
                let tmp = src.with_file_name(format!("{stale}{}", std::process::id()));
                let _ = std::fs::remove_file(&tmp);
                copy_entry(&orig, &tmp, &meta)
                    .and_then(|_| std::fs::rename(&tmp, &src))
                    .inspect_err(|_| {
                        let _ = std::fs::remove_file(&tmp);
                    })
            });
            res.map_err(|e| fail("put configure's version back at", Path::new(rel), e))?;
            rewind.restored += 1;
            moved += 1;
        }
        let list: String = rewind
            .removed_dirs
            .iter()
            .map(|d| format!("{}\n", d.display()))
            .collect();
        write_atomic(&rewind.removed_file, &list)
            .map_err(|e| fail("record what it set aside in", state, e))?;
        eprintln!(
            "nix-ninja: an earlier pass placed outputs in this build directory; {moved} \
             file(s) and {} directory(ies) it placed are set aside until this pass has \
             placed its own, {} of them in place of configure's version; {changed} \
             changed since placement stay",
            rewind.removed_dirs.len(),
            rewind.restored
        );
        Ok(Some(rewind))
    }

    /// Moves back every file set aside whose place is still empty (a file
    /// this pass placed again stays as placed), and recreates the
    /// directories set aside with them.
    fn restore(&mut self) -> std::io::Result<()> {
        // CONFIGURE'S VERSION GIVES WAY to what is set aside at its path (a
        // file or tree in `aside`, or an emptied directory in `removed_dirs`),
        // first, since the placed thing may be a directory. Asked of the
        // files themselves, not of a record written after the copy, so a run
        // that died between the two cannot leave configure's version standing
        // and the placed one discarded. A path holding anything else was
        // placed again by this pass, or edited, and keeps it.
        for e in walkdir::WalkDir::new(&self.originals)
            .min_depth(1)
            .into_iter()
            .flatten()
        {
            let Ok(rel) = e.path().strip_prefix(&self.originals) else {
                continue;
            };
            let dest = self.build_dir.join(rel);
            let set_aside = std::fs::symlink_metadata(self.aside.join(rel)).is_ok()
                || self.removed_dirs.iter().any(|d| d == rel);
            if !e.file_type().is_dir()
                && confined(&rel.to_string_lossy())
                && set_aside
                && same_entry(&dest, e.path())
            {
                let _ = std::fs::remove_file(&dest);
            }
        }
        let mut failed: Option<(PathBuf, std::io::Error)> = None;
        // A directory that cannot be recreated stays on the record, so a
        // later run retries it: configure's file at its path is gone already.
        let mut still = String::new();
        for rel in self.removed_dirs.drain(..) {
            if let Err(e) = std::fs::create_dir_all(self.build_dir.join(&rel)) {
                still.push_str(&format!("{}\n", rel.display()));
                failed.get_or_insert((rel, e));
            }
        }
        if still.is_empty() {
            let _ = std::fs::remove_file(&self.removed_file);
        } else {
            let _ = write_atomic(&self.removed_file, &still);
        }
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
                // Configure's version still standing (it could not give way)
                // is not a placement of this pass: the placed file stays
                // aside and the run fails, rather than losing it.
                let orig = self.originals.join(rel);
                if std::fs::symlink_metadata(&orig).is_ok() && same_entry(&dest, &orig) {
                    failed.get_or_insert((
                        rel.to_path_buf(),
                        std::io::Error::other("configure's version is in its place"),
                    ));
                    continue;
                }
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

    /// A record holding `files` and every directory above them.
    fn record_of(files: &std::collections::HashSet<String>) -> ConfigureState {
        let mut dirs = std::collections::HashSet::new();
        for f in files {
            let mut p = Path::new(f).parent();
            while let Some(d) = p.filter(|d| !d.as_os_str().is_empty()) {
                dirs.insert(d.to_string_lossy().into_owned());
                p = d.parent();
            }
        }
        ConfigureState {
            files: files.clone(),
            dirs,
            ..Default::default()
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nn-cfgstate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    /// A rewind that is expected. The lock is retried for a moment: a fork
    /// in a parallel test thread can hold a copy of the lock's descriptor
    /// while the test, one process, takes the lock again; separate drivers
    /// never meet that.
    fn begin(state: &Path, bd: &Path) -> Rewind {
        begin_result(state, bd)
            .expect("no failure")
            .expect("a journal")
    }
    /// The same, for a rewind that may fail: the first answer that is not
    /// the lock held.
    fn begin_result(state: &Path, bd: &Path) -> std::io::Result<Option<Rewind>> {
        for _ in 0..200 {
            match Rewind::begin_in(state, bd, false) {
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                done => return done,
            }
        }
        Ok(None)
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
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        write(&bd.join("docs/index.html"), "hook");
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(&bd.join("include/Foo/b.h"), "HOOK-EDITED");

        let rewind = begin(&state, &bd);
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
            Rewind::begin_in(&state, &bd, false).unwrap().is_none(),
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
            Rewind::begin_in(&state, &bd, true).unwrap().is_none(),
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
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");

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
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        let journal = std::fs::read_to_string(state.join("placed.v1")).unwrap();
        assert!(
            !journal.lines().any(|l| l.ends_with(" same.h")),
            "identical bytes are no change:\n{journal}"
        );

        let rewind = begin(&state, &bd);
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
            // A climb that comes back into the build dir is asked as where
            // it lands.
            ("../build/meta/d/meta.json", true),
            ("../build/meta/d/w123.tmp", false),
            ("/build/pkg/src/../build/meta/d/w123.tmp", false),
        ] {
            assert_eq!(
                super::left_by(Some(&record_of(&files)), bd, Path::new(path)),
                read,
                "{path}"
            );
        }
        assert!(super::left_by(None, bd, Path::new("meta/d/w123.tmp")));
        // Below a symlink configure left (the walk recorded the link, not
        // what it reaches) and below a directory the walk skips, the record
        // cannot answer and the listing reads.
        let files: std::collections::HashSet<String> = ["python".to_string()].into();
        assert!(super::left_by(
            Some(&record_of(&files)),
            bd,
            Path::new("python/pkg/a.py")
        ));
        assert!(super::left_by(
            Some(&record_of(&files)),
            bd,
            Path::new("meson-private/x.dat")
        ));
        assert!(!super::left_by(
            Some(&record_of(&files)),
            bd,
            Path::new("pythonx/a.py")
        ));
        // Any spelling of a file configure left (CMake-joined dir arguments).
        let files: std::collections::HashSet<String> = ["meta/d/meta.json".to_string()].into();
        for spelled in [
            "meta/./d/meta.json",
            "meta//d/meta.json",
            "meta/d/./meta.json",
        ] {
            assert!(
                super::left_by(Some(&record_of(&files)), bd, Path::new(spelled)),
                "{spelled}"
            );
        }
    }

    /// A LINK LANDING BACK IN THE BUILD DIR is asked as where it lands: the
    /// file configure left there is read through it, a file written there
    /// between phases is not.
    #[test]
    fn a_listing_through_a_link_into_the_tree_asks_where_it_lands() {
        let d = scratch("listing-link");
        let bd = d.join("build");
        write(&bd.join("real/a.h"), "cfg");
        write(&bd.join("real/w.tmp"), "between phases");
        std::os::unix::fs::symlink("real", bd.join("alias")).unwrap();
        let files: std::collections::HashSet<String> =
            ["real/a.h".to_string(), "alias".to_string()].into();
        assert!(super::left_by(
            Some(&record_of(&files)),
            &bd,
            Path::new("alias/a.h")
        ));
        assert!(!super::left_by(
            Some(&record_of(&files)),
            &bd,
            Path::new("alias/w.tmp")
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE PASS AFTER A PASS THAT PLACED NOTHING STILL READS CONFIGURE'S
    /// VERSION. stdenv runs check before install, and a check pass whose
    /// targets leave out the replaced output journals without placing it;
    /// dropping the earlier pass's line then had the install pass read the
    /// placed file. A re-configure (a new record) forgets the old originals,
    /// and a run that died after putting configure's version back still
    /// gets the placed file back.
    #[test]
    fn configure_versions_survive_a_pass_that_places_nothing_and_a_crash() {
        let d = scratch("originals-life");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        write(&bd.join("m.json"), "");
        let record = ConfigureState {
            files: ["build.ninja", "m.json"].map(String::from).into(),
            ..Default::default()
        };
        save_in(&state, &bd, &record);
        let placed = [PathBuf::from("m.json")];
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");
        write(&bd.join("m.json"), "placed");
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        let read = || std::fs::read_to_string(bd.join("m.json")).unwrap();
        for pass in [2, 3] {
            let rewind = begin(&state, &bd);
            assert_eq!(read(), "", "pass {pass} reads configure's version");
            journal_in(&state, &bd, std::iter::empty()).expect("journaled");
            rewind.finish().expect("everything comes back");
            assert_eq!(read(), "placed", "after pass {pass}");
        }

        // A run that died after putting configure's version back. The lock
        // is retried: a fork in a parallel test thread can hold a copy of the
        // file descriptor for a moment, which separate drivers never see.
        let rewind = begin(&state, &bd);
        assert_eq!(read(), "");
        std::mem::forget(rewind);
        // The forgotten value still holds the lock; what it left on disk is
        // what a dead process leaves, and the next run's restore reads that.
        let mut next = Rewind {
            build_dir: bd.clone(),
            aside: state.join("aside"),
            removed_dirs: super::read_removed(&state),
            removed_file: state.join("removed-dirs"),
            originals: super::originals(&state),
            restored: 0,
            done: true,
            _lock: None,
        };
        next.restore().expect("everything comes back");
        assert_eq!(read(), "placed", "the placed file, not configure's");

        // A new configure: its own file at the path, a fresh record.
        write(&bd.join("m.json"), "configure B");
        save_in(&state, &bd, &record);
        assert!(
            !super::originals(&state).exists(),
            "the old originals are gone"
        );
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");
        assert_eq!(
            std::fs::read_to_string(super::originals(&state).join("m.json")).unwrap(),
            "configure B"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// WHEN CONFIGURE'S VERSION CANNOT GIVE WAY the placed file is kept
    /// aside and the run fails; it is never discarded as "placed again".
    #[test]
    fn a_placed_file_is_kept_when_configures_version_cannot_give_way() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("originals-stuck");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("sub/m.json"), "");
        write(&super::originals(&state).join("sub/m.json"), "");
        write(&state.join("aside/sub/m.json"), "placed");
        let ro = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(bd.join("sub"), ro).unwrap();
        let mut r = Rewind {
            build_dir: bd.clone(),
            aside: state.join("aside"),
            removed_dirs: Vec::new(),
            removed_file: state.join("removed-dirs"),
            originals: super::originals(&state),
            restored: 0,
            done: true,
            _lock: None,
        };
        let res = r.restore();
        std::fs::set_permissions(bd.join("sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(res.is_err(), "the run fails");
        assert_eq!(
            std::fs::read_to_string(state.join("aside/sub/m.json")).unwrap(),
            "placed",
            "and the placed file waits aside"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A pass 1 that placed `m` over configure's file: the record, the kept
    /// original and the journal line, as placement leaves them.
    fn placed_over_configure(tag: &str, spelled: &str) -> (PathBuf, PathBuf, PathBuf) {
        let d = scratch(tag);
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        write(&bd.join("D/m.json"), "");
        let record = ConfigureState {
            files: ["build.ninja", "D/m.json"].map(String::from).into(),
            dirs: ["D".to_string()].into(),
            ..Default::default()
        };
        save_in(&state, &bd, &record);
        let placed = [PathBuf::from(spelled)];
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");
        write(&bd.join("D/m.json"), "placed");
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        (d, bd, state)
    }

    /// A PASS THAT COVERS THE PATH WITHOUT WRITING IT (it places the tree
    /// around it, and the file already there is skipped) keeps the earlier
    /// pass's line, so the pass after it reads configure's version too.
    #[test]
    fn an_o_line_survives_a_pass_that_covers_the_path_without_writing_it() {
        let (d, bd, state) = placed_over_configure("o-covered", "D/m.json");
        let read = || std::fs::read_to_string(bd.join("D/m.json")).unwrap();
        let rewind = begin(&state, &bd);
        assert_eq!(read(), "", "pass 2 reads configure's version");
        journal_in(&state, &bd, [Path::new("D")]).expect("journaled");
        rewind.finish().expect("everything comes back");
        let rewind = begin(&state, &bd);
        assert_eq!(read(), "", "and so does pass 3");
        rewind.finish().expect("everything comes back");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A PLACED PATH SPELLED `./D/m.json` keeps configure's version as
    /// `D/m.json` does: the journal takes either spelling.
    #[test]
    fn a_leading_dot_placed_path_keeps_configures_version() {
        let (d, bd, state) = placed_over_configure("o-dot", "./D/m.json");
        let rewind = begin(&state, &bd);
        assert_eq!(std::fs::read_to_string(bd.join("D/m.json")).unwrap(), "");
        rewind.finish().expect("everything comes back");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// CONFIGURE'S VERSION THAT CANNOT BE PUT BACK STOPS THE PASS, with the
    /// placed file back in its place, never an empty path read as if
    /// nothing were there. A copy a killed run left beside it goes.
    #[test]
    fn a_failed_copy_back_stops_the_pass_and_leaves_the_placed_file() {
        use std::os::unix::fs::PermissionsExt;
        let (d, bd, state) = placed_over_configure("o-copyback", "D/m.json");
        let stale = bd.join("D/.m.json.nn-orig.1");
        write(&stale, "left by a killed run");
        let kept = super::originals(&state).join("D/m.json");
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o000)).unwrap();
        let res = begin_result(&state, &bd);
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(res.is_err(), "the pass stops");
        assert_eq!(
            std::fs::read_to_string(bd.join("D/m.json")).unwrap(),
            "placed",
            "and the placed file is where it was"
        );
        assert!(!stale.exists(), "the killed run's copy is gone");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A PLACEMENT THAT CANNOT BE JOURNALED STOPS THE RUN, as one that
    /// cannot keep configure's copy does: unjournaled, the next pass would
    /// read the placed file where pass 1 read configure's.
    #[test]
    fn an_unjournaled_placement_stops_the_run() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("journal-stuck");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        write(&bd.join("D/m.json"), "configure");
        let record = ConfigureState {
            files: ["build.ninja", "D/m.json"].map(String::from).into(),
            dirs: ["D".to_string()].into(),
            ..Default::default()
        };
        save_in(&state, &bd, &record);
        let placed = [PathBuf::from("D/m.json")];
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");
        write(&bd.join("D/m.json"), "placed");
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();
        let res = journal_in(&state, &bd, placed.iter().map(PathBuf::as_path));
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(res.is_err(), "the run stops");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A PLACED FILE THAT CANNOT BE SET ASIDE STOPS THE PASS: reading it
    /// would read another tree than the first pass did.
    #[test]
    fn a_placed_file_that_cannot_be_set_aside_stops_the_pass() {
        let d = scratch("f-stuck");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        let record = ConfigureState {
            files: ["build.ninja".to_string()].into(),
            ..Default::default()
        };
        save_in(&state, &bd, &record);
        write(&bd.join("gen.h"), "placed");
        journal_in(&state, &bd, [Path::new("gen.h")]).expect("journaled");
        // Nothing can be made below a file.
        write(&state.join("aside"), "in the way");
        assert!(begin_result(&state, &bd).is_err());
        assert_eq!(std::fs::read_to_string(bd.join("gen.h")).unwrap(), "placed");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A FILE CONFIGURE LEFT THAT IS GONE when placement replaces it has no
    /// copy to keep: the run stops rather than leave a later pass reading
    /// the placed one.
    #[test]
    fn a_configure_file_gone_before_placement_stops_the_run() {
        let d = scratch("orig-gone");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        let record = ConfigureState {
            files: ["build.ninja", "m.json"].map(String::from).into(),
            ..Default::default()
        };
        save_in(&state, &bd, &record);
        assert!(keep_originals_in(&state, &bd, &record, [Path::new("m.json")]).is_err());
        // A path configure did not leave has nothing to keep.
        assert!(keep_originals_in(&state, &bd, &record, [Path::new("new.h")]).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A DIRECTORY THAT CANNOT BE RECREATED STAYS ON THE RECORD: configure's
    /// file at its path went first, so dropping the record loses both, and
    /// a new configure's record does not clear the originals meanwhile.
    #[test]
    fn a_directory_that_cannot_come_back_stays_on_the_record() {
        let d = scratch("removed-stuck");
        let bd = d.join("build");
        let state = d.join("state");
        // `a` is a file now, so `a/b` cannot be made.
        write(&bd.join("a"), "in the way");
        write(&super::originals(&state).join("x"), "configure's");
        write(&state.join("removed-dirs"), "a/b\n");
        let mut r = Rewind {
            build_dir: bd.clone(),
            aside: state.join("aside"),
            removed_dirs: super::read_removed(&state),
            removed_file: state.join("removed-dirs"),
            originals: super::originals(&state),
            restored: 0,
            done: true,
            _lock: None,
        };
        assert!(r.restore().is_err(), "the run fails");
        assert_eq!(
            super::read_removed(&state),
            [PathBuf::from("a/b")],
            "and the directory waits on the record"
        );
        save_in(&state, &bd, &ConfigureState::default());
        assert!(
            super::originals(&state).join("x").exists(),
            "the originals are kept while it waits"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A HOOK'S EDIT OF A REPLACED PATH between passes reads the same on
    /// every later pass, as an edited placed file does: the edit is not
    /// journaled as a placement, so no later pass sets it aside.
    #[test]
    fn a_hook_edit_of_a_replaced_path_reads_the_same_on_every_later_pass() {
        let d = scratch("originals-hook");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        write(&bd.join("m.json"), "");
        let record = ConfigureState {
            files: ["build.ninja", "m.json"].map(String::from).into(),
            ..Default::default()
        };
        save_in(&state, &bd, &record);
        let placed = [PathBuf::from("m.json")];
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");
        write(&bd.join("m.json"), "placed");
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(&bd.join("m.json"), "HOOK");
        let read = || std::fs::read_to_string(bd.join("m.json")).unwrap();
        for pass in [2, 3] {
            let rewind = begin(&state, &bd);
            assert_eq!(read(), "HOOK", "pass {pass}");
            journal_in(&state, &bd, std::iter::empty()).expect("journaled");
            rewind.finish().expect("everything comes back");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// AN EMPTY DIRECTORY PLACED WHERE CONFIGURE LEFT A FILE comes back at
    /// the end of a pass that placed nothing; and a record written while a
    /// pass holds files aside does not clear configure's versions.
    #[test]
    fn an_empty_placed_directory_over_a_configure_file_comes_back() {
        let d = scratch("originals-emptydir");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        write(&bd.join("x"), "configure");
        let record = ConfigureState {
            files: ["build.ninja", "x"].map(String::from).into(),
            ..Default::default()
        };
        save_in(&state, &bd, &record);
        let placed = [PathBuf::from("x")];
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");
        std::fs::remove_file(bd.join("x")).unwrap();
        std::fs::create_dir(bd.join("x")).unwrap();
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        let rewind = begin(&state, &bd);
        assert_eq!(std::fs::read_to_string(bd.join("x")).unwrap(), "configure");
        // A record written now (a lost configure.v1) leaves the originals.
        write(&state.join("aside/.keep"), "");
        save_in(&state, &bd, &record);
        assert!(
            super::originals(&state).join("x").exists(),
            "originals kept"
        );
        std::fs::remove_file(state.join("aside/.keep")).unwrap();
        journal_in(&state, &bd, std::iter::empty()).expect("journaled");
        rewind.finish().expect("everything comes back");
        assert!(bd.join("x").is_dir(), "the placed directory is back");
        let _ = std::fs::remove_dir_all(&d);
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
