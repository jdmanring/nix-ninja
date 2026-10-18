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
//! So the first pass's walk records every directory and file configure left
//! (`save`), and a later pass that does not trust earlier invocations' state
//! (the resolve memo's switch: off inside a build) moves everything else
//! aside before it reads the directory and puts it back once it has placed
//! its own products (`Rewind`). Anything the later pass places again wins
//! over the copy moved aside. A changed build.ninja is a re-configure: no
//! record answers for it and the walk records afresh.

use std::collections::HashSet;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// Under `NIX_BUILD_TOP` inside a build (beside the NAR stamps), else in the
/// build dir, where the walk skips it as the driver's own state.
pub const FILE_NAME: &str = ".nix-ninja-configure-state.v1";
/// Where a later pass keeps what it moved aside, beside the record.
const ASIDE: &str = ".nix-ninja-rewind";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConfigureState {
    pub dirs: HashSet<String>,
    pub files: HashSet<String>,
}

/// The walk does not descend these, so the record knows nothing below them.
pub fn walk_skips(name: &str) -> bool {
    name.starts_with("meson-") || name == ASIDE
}

fn home(build_dir: &Path) -> PathBuf {
    std::env::var_os("NIX_BUILD_TOP")
        .map(PathBuf::from)
        .unwrap_or_else(|| build_dir.to_path_buf())
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

/// The record a first pass wrote for this configure, or None: no record, an
/// unreadable one, or one for another build.ninja.
pub fn load(build_dir: &Path) -> Option<ConfigureState> {
    load_from(&home(build_dir).join(FILE_NAME), build_dir)
}

fn load_from(record: &Path, build_dir: &Path) -> Option<ConfigureState> {
    let text = std::fs::read_to_string(record).ok()?;
    let mut lines = text.lines();
    if lines.next()? != header(build_dir)? {
        return None;
    }
    let mut s = ConfigureState::default();
    for line in lines {
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
    save_to(&home(build_dir).join(FILE_NAME), build_dir, s)
}

fn save_to(path: &Path, build_dir: &Path, s: &ConfigureState) {
    let write = || -> std::io::Result<()> {
        let head = header(build_dir).ok_or_else(|| std::io::Error::other("no build.ninja"))?;
        let tmp = path.with_extension("tmp");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        writeln!(f, "{head}")?;
        for (kind, set) in [("D", &s.dirs), ("F", &s.files)] {
            let mut v: Vec<&String> = set.iter().collect();
            v.sort();
            for rel in v {
                writeln!(f, "{kind} {rel}")?;
            }
        }
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        eprintln!(
            "nix-ninja: could not record the configure-time build directory ({e}); \
             a later pass reads the directory as it stands"
        );
    }
}

/// What a later pass moves aside: every file under `build_dir` the record
/// does not hold, and every directory it does not hold (deepest first), as
/// relative paths. The driver's own state files stay (`own_state`).
fn not_left_by_configure(
    build_dir: &Path,
    r: &ConfigureState,
    own_state: impl Fn(&Path) -> bool,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let walk = walkdir::WalkDir::new(build_dir)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !walk_skips(&e.file_name().to_string_lossy()));
    for e in walk.flatten() {
        let Ok(rel) = e.path().strip_prefix(build_dir) else {
            continue;
        };
        let key = rel.to_string_lossy();
        if e.file_type().is_dir() {
            if !r.dirs.contains(key.as_ref()) {
                dirs.push(rel.to_path_buf());
            }
        } else if !r.files.contains(key.as_ref()) && !own_state(e.path()) {
            files.push(rel.to_path_buf());
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    (files, dirs)
}

/// The later pass's view of the build dir as the first pass found it, held
/// for the life of the value; dropping it puts back whatever was moved aside
/// and not placed again.
pub struct Rewind {
    build_dir: PathBuf,
    aside: PathBuf,
}

impl Rewind {
    /// Puts back anything a run that never finished left aside, then, when
    /// earlier invocations' state is not trusted and a record answers for
    /// this configure, moves aside what configure did not leave. None when
    /// there is nothing to do.
    pub fn begin(
        build_dir: &Path,
        trusted: bool,
        own_state: impl Fn(&Path) -> bool,
    ) -> Option<Self> {
        let aside = home(build_dir).join(ASIDE);
        let rewind = Rewind {
            build_dir: build_dir.to_path_buf(),
            aside,
        };
        rewind.restore();
        if trusted {
            return None;
        }
        let r = load(build_dir)?;
        let (files, dirs) = not_left_by_configure(build_dir, &r, own_state);
        let mut moved = 0usize;
        for rel in &files {
            let dest = rewind.aside.join(rel);
            let ok = dest
                .parent()
                .is_some_and(|p| std::fs::create_dir_all(p).is_ok())
                && std::fs::rename(build_dir.join(rel), &dest).is_ok();
            if ok {
                moved += 1;
            } else {
                eprintln!(
                    "nix-ninja: could not move {} aside; this pass reads it",
                    rel.display()
                );
            }
        }
        for rel in &dirs {
            // Only an emptied directory goes; one still holding files the
            // walk skips stays.
            let _ = std::fs::remove_dir(build_dir.join(rel));
        }
        eprintln!(
            "nix-ninja: an earlier pass ran in this build directory; {moved} file(s) and \
             {} directory(ies) configure did not leave are set aside until this pass \
             has placed its outputs",
            dirs.len()
        );
        Some(rewind)
    }

    /// Moves back every file set aside whose place is still empty; a file
    /// this pass placed again stays as placed.
    fn restore(&self) {
        if std::fs::symlink_metadata(&self.aside).is_err() {
            return;
        }
        let walk = walkdir::WalkDir::new(&self.aside)
            .min_depth(1)
            .contents_first(true);
        for e in walk.into_iter().flatten() {
            let Ok(rel) = e.path().strip_prefix(&self.aside) else {
                continue;
            };
            if e.file_type().is_dir() {
                let _ = std::fs::remove_dir(e.path());
                continue;
            }
            let dest = self.build_dir.join(rel);
            if std::fs::symlink_metadata(&dest).is_ok() {
                let _ = std::fs::remove_file(e.path());
                continue;
            }
            let back = dest
                .parent()
                .is_some_and(|p| std::fs::create_dir_all(p).is_ok())
                && std::fs::rename(e.path(), &dest).is_ok();
            if !back {
                eprintln!(
                    "nix-ninja: could not put {} back from {}",
                    rel.display(),
                    self.aside.display()
                );
            }
        }
        let _ = std::fs::remove_dir(&self.aside);
    }
}

impl Drop for Rewind {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::{load_from, not_left_by_configure, save_to, ConfigureState, Rewind};
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nn-cfgstate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A record answers only for the build.ninja it was written against:
    /// read back unchanged, refused once build.ninja changes (a re-configure).
    #[test]
    fn a_record_answers_for_its_own_configure_only() {
        let d = scratch("roundtrip");
        let rec = d.join(super::FILE_NAME);
        std::fs::write(d.join("build.ninja"), "default all\n").unwrap();
        let s = ConfigureState {
            dirs: ["CMakeFiles/foo.dir".to_string()].into(),
            files: ["build.ninja".to_string(), "config.h".to_string()].into(),
        };
        assert_eq!(load_from(&rec, &d), None, "no record before the first pass");
        save_to(&rec, &d, &s);
        assert_eq!(load_from(&rec, &d), Some(s));
        std::fs::write(d.join("build.ninja"), "default all app\n").unwrap();
        assert_eq!(
            load_from(&rec, &d),
            None,
            "a changed build.ninja is a new configure"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE QT SHAPE. Configure left `include/Foo/fooexports.h` and an empty
    /// `CMakeFiles/foo.dir`; the build pass placed `include/Foo/a.h` (a tree
    /// interior no edge declares), an object into the empty directory, and
    /// a whole new directory. A later pass sets aside exactly what
    /// configure did not leave, the driver's own state excepted, and the
    /// directories go deepest first.
    #[test]
    fn a_later_pass_sets_aside_exactly_what_configure_did_not_leave() {
        let d = scratch("aside");
        for f in [
            "build.ninja",
            "include/Foo/fooexports.h",
            "include/Foo/a.h",
            "CMakeFiles/foo.dir/foo.c.o",
            "gen/sub/y.h",
            ".ninja_log",
            "meson-private/x",
        ] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "x").unwrap();
        }
        let r = ConfigureState {
            dirs: ["include", "include/Foo", "CMakeFiles", "CMakeFiles/foo.dir"]
                .map(String::from)
                .into(),
            files: ["build.ninja", "include/Foo/fooexports.h"]
                .map(String::from)
                .into(),
        };
        let (mut files, dirs) =
            not_left_by_configure(&d, &r, |p| p.file_name().is_some_and(|n| n == ".ninja_log"));
        files.sort();
        assert_eq!(
            files,
            [
                "CMakeFiles/foo.dir/foo.c.o",
                "gen/sub/y.h",
                "include/Foo/a.h"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(dirs, ["gen/sub", "gen"].map(PathBuf::from));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Dropping the rewind puts back what was set aside, except where the
    /// pass placed the path again: the new placement wins.
    #[test]
    fn what_was_set_aside_comes_back_unless_placed_again() {
        let d = scratch("restore");
        let bd = d.join("build");
        let aside = d.join("aside");
        for (p, v) in [("aside/kept.h", "old"), ("aside/re/placed.h", "old")] {
            let p = d.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, v).unwrap();
        }
        std::fs::create_dir_all(bd.join("re")).unwrap();
        std::fs::write(bd.join("re/placed.h"), "new").unwrap();
        drop(Rewind {
            build_dir: bd.clone(),
            aside: aside.clone(),
        });
        assert_eq!(std::fs::read_to_string(bd.join("kept.h")).unwrap(), "old");
        assert_eq!(
            std::fs::read_to_string(bd.join("re/placed.h")).unwrap(),
            "new"
        );
        assert!(!aside.exists(), "the aside directory is removed once empty");
        let _ = std::fs::remove_dir_all(&d);
        let _ = Path::new("");
    }
}
