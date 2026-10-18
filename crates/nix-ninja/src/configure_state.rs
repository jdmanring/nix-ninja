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
//! between phases stays where it is, and a configure file placement
//! overwrote stays too. A changed build.ninja is a re-configure: no record
//! answers for it and the walk records afresh.

use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// The walk does not descend these, so the record knows nothing below them.
const STATE_DIR: &str = ".nix-ninja-state";

pub fn walk_skips(name: &str) -> bool {
    name.starts_with("meson-") || name == STATE_DIR
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
    for line in read_lines(&state.join("configure.v1"), build_dir)? {
        match line.split_once(' ')? {
            ("D", rel) => s.dirs.insert(rel.to_string()),
            ("F", rel) => s.files.insert(rel.to_string()),
            _ => return None,
        };
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
    let lines: Vec<String> = s
        .dirs
        .iter()
        .map(|d| format!("D {d}"))
        .chain(s.files.iter().map(|f| format!("F {f}")))
        .collect();
    if let Err(e) = write_lines(&state.join("configure.v1"), build_dir, &lines, false) {
        eprintln!(
            "nix-ninja: could not record the configure-time build directory ({e}); \
             a later pass reads the directory as it stands"
        );
    }
}

/// Journals what this run's placement wrote: each of `placed` (relative
/// build paths) that exists, every file below one that is a directory, and
/// every ancestor directory configure did not leave. Called only once
/// placement has succeeded, so a run that never finished placing (a residue
/// re-exec) leaves its successor nothing of its own to set aside.
pub fn journal_placed<'a>(build_dir: &Path, placed: impl IntoIterator<Item = &'a Path>) {
    journal_in(&state_dir(build_dir), build_dir, placed)
}

fn journal_in<'a>(state: &Path, build_dir: &Path, placed: impl IntoIterator<Item = &'a Path>) {
    let Some(r) = load_in(state, build_dir) else {
        return;
    };
    let mut lines = BTreeSet::new();
    let mut add = |rel: &Path, is_dir: bool| {
        let key = rel.to_string_lossy().into_owned();
        if is_dir {
            if !r.dirs.contains(&key) {
                lines.insert(format!("D {key}"));
            }
        } else if !r.files.contains(&key) {
            lines.insert(format!("F {key}"));
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
            add(a, true);
            anc = a.parent();
        }
        if meta.is_dir() {
            for e in walkdir::WalkDir::new(&abs).into_iter().flatten() {
                if let Ok(r) = e.path().strip_prefix(build_dir) {
                    add(r, e.file_type().is_dir());
                }
            }
        } else {
            add(rel, false);
        }
    }
    let lines: Vec<String> = lines.into_iter().collect();
    if let Err(e) = write_lines(&state.join("placed.v1"), build_dir, &lines, true) {
        eprintln!(
            "nix-ninja: could not journal this run's placement ({e}); a later pass \
             reads what it placed"
        );
    }
}

/// The later pass's view of the build dir as the first pass found it, held
/// for the life of the value; dropping it puts back whatever was moved aside
/// and not placed again, and the directories removed with it.
pub struct Rewind {
    build_dir: PathBuf,
    aside: PathBuf,
    removed_dirs: Vec<PathBuf>,
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
        std::fs::create_dir_all(state).ok()?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(state.join("lock"))
            .ok()?;
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
            removed_dirs: Vec::new(),
            _lock: Some(lock),
        };
        rewind.restore();
        if trusted {
            return None;
        }
        let journal = read_lines(&state.join("placed.v1"), build_dir)?;
        let mut moved = 0usize;
        let mut failed: Option<(PathBuf, std::io::Error)> = None;
        let mut dirs: Vec<PathBuf> = Vec::new();
        for line in &journal {
            match line.split_once(' ') {
                Some(("F", rel)) => {
                    let (src, dest) = (build_dir.join(rel), rewind.aside.join(rel));
                    if std::fs::symlink_metadata(&src).is_err() {
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
                Some(("D", rel)) => dirs.push(PathBuf::from(rel)),
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
        eprintln!(
            "nix-ninja: an earlier pass placed outputs in this build directory; {moved} \
             file(s) and {} directory(ies) it placed are set aside until this pass has \
             placed its own",
            rewind.removed_dirs.len()
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
    fn restore(&mut self) {
        for rel in self.removed_dirs.drain(..) {
            let _ = std::fs::create_dir_all(self.build_dir.join(rel));
        }
        if std::fs::symlink_metadata(&self.aside).is_err() {
            return;
        }
        let mut failed: Option<(PathBuf, std::io::Error)> = None;
        let walk = walkdir::WalkDir::new(&self.aside)
            .min_depth(1)
            .contents_first(true);
        for e in walk.into_iter().flatten() {
            let Ok(rel) = e.path().strip_prefix(&self.aside) else {
                continue;
            };
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
        if let Some((rel, e)) = failed {
            eprintln!(
                "nix-ninja: could not put every set-aside file back (first: {}: {e}); \
                 the rest wait in {}",
                rel.display(),
                self.aside.display()
            );
        }
    }
}

impl Drop for Rewind {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::{journal_in, load_in, save_in, state_dir_in, ConfigureState, Rewind};
    use std::path::{Path, PathBuf};

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

    /// THE QT SHAPE, end to end through the journal. Configure left
    /// `include/Foo/fooexports.h` and an empty `CMakeFiles/foo.dir`; the
    /// build pass placed a whole tree output `include/Foo` (its interior
    /// `a.h` no edge declares), an object into the empty directory, and a
    /// header in a directory it created. A hook wrote `docs/index.html`
    /// between phases. The later pass sets aside exactly the placed paths,
    /// leaving configure's file and the hook's; dropping the rewind puts
    /// back what this pass did not place again and keeps what it did.
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
            },
        );
        write(&bd.join("include/Foo/a.h"), "placed");
        write(&bd.join("CMakeFiles/foo.dir/foo.c.o"), "placed");
        write(&bd.join("gen/sub/y.h"), "placed");
        let placed =
            ["include/Foo", "CMakeFiles/foo.dir/foo.c.o", "gen/sub/y.h"].map(PathBuf::from);
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path));
        write(&bd.join("docs/index.html"), "hook");

        let rewind = Rewind::begin_in(&state, &bd, false).expect("a journal to act on");
        for gone in [
            "include/Foo/a.h",
            "CMakeFiles/foo.dir/foo.c.o",
            "gen/sub/y.h",
            "gen",
        ] {
            assert!(!bd.join(gone).exists(), "{gone} should be set aside");
        }
        for kept in [
            "include/Foo/fooexports.h",
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
        // The pass places one output again; the rest come back on drop.
        write(&bd.join("gen/sub/y.h"), "new");
        drop(rewind);
        assert_eq!(
            std::fs::read_to_string(bd.join("gen/sub/y.h")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(bd.join("include/Foo/a.h")).unwrap(),
            "placed"
        );
        assert!(bd.join("CMakeFiles/foo.dir/foo.c.o").exists());
        assert!(
            Rewind::begin_in(&state, &bd, true).is_none(),
            "a run that trusts earlier state never rewinds"
        );
        assert!(bd.join("include/Foo/a.h").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
