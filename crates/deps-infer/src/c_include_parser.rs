use crate::gcc_include_parser;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::fs::canonicalize;
use std::fs::File;
use std::hash::Hash;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

pub fn retrieve_c_includes(
    cmdline: &str,
    files: Vec<PathBuf>,
    virtual_paths: Option<HashMap<PathBuf, PathBuf>>,
) -> Result<Vec<PathBuf>> {
    let includes = gcc_include_parser::parse_include_dirs(cmdline)?;
    bfs_parse_includes(files, &includes, virtual_paths)
}

/// Recursively collect all dependencies using BFS
fn bfs_parse_includes(
    files: Vec<PathBuf>,
    include_dirs: &[PathBuf],
    virtual_paths: Option<HashMap<PathBuf, PathBuf>>,
) -> Result<Vec<PathBuf>> {
    let mut visited = rustc_hash::FxHashSet::default();
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    // CROSS-FILE COMPUTED INCLUDES. Defines accumulate over the whole walk
    // and uses wait until their macro appears - preprocessing order is not
    // modeled, which can only OVER-declare (harmless: an extra input), never
    // under-declare relative to the old behavior of dropping the use.
    let mut tu_defines: rustc_hash::FxHashMap<String, String> = Default::default();
    let mut pending_uses: Vec<(PathBuf, String)> = Vec::new();

    // Initialize queue with starting files
    for file in files {
        if visited.insert(file.clone()) {
            queue.push_back(file.clone());
            result.push(file);
        }
    }

    // Process queue in batches until empty
    while !queue.is_empty() {
        // Get all files currently in the queue
        let current_batch: Vec<PathBuf> = queue.drain(..).collect();

        // Process all files in the current batch in parallel
        let sources_with_includes = all_sources_and_includes(
            current_batch.into_iter().map(Ok::<_, std::io::Error>),
            include_dirs,
            virtual_paths.as_ref(),
        )?;

        // Process each source's includes
        for source in sources_with_includes {
            for include in source.includes {
                if visited.insert(include.clone()) {
                    queue.push_back(include.clone());
                    result.push(include);
                }
            }
            for (k, v) in source.path_defines {
                tu_defines.entry(k).or_insert(v);
            }
            if let Some(dir) = source.path.parent() {
                for u in source.macro_uses {
                    pending_uses.push((dir.to_path_buf(), u));
                }
            }
        }

        // Resolve any use whose macro is now defined, exactly as a quoted
        // directive from its includer: includer dir first, include dirs
        // after. Unresolved uses stay pending for a later batch's defines;
        // one never defined stays undeclared, and the task then fails
        // loudly, which is the old behavior.
        let mut still_pending = Vec::new();
        for (dir, name) in pending_uses.drain(..) {
            let Some(val) = tu_defines.get(&name) else {
                still_pending.push((dir, name));
                continue;
            };
            let tail = PathBuf::from(val);
            let resolved = try_resolve(&dir, &tail, virtual_paths.as_ref())
                .or_else(|| {
                    include_dirs
                        .iter()
                        .find_map(|i| try_resolve(i, &tail, virtual_paths.as_ref()))
                });
            if let Some(p) = resolved {
                let spelled = lexical_normalize(&dir.join(&tail));
                if spelled != p && spelled.is_relative() && visited.insert(spelled.clone()) {
                    result.push(spelled);
                }
                if visited.insert(p.clone()) {
                    queue.push_back(p.clone());
                    result.push(p);
                }
            }
        }
        pending_uses = still_pending;
    }

    Ok(result)
}

#[derive(Debug, PartialEq, PartialOrd)]
pub struct SourceWithIncludes {
    pub path: PathBuf,
    pub includes: Vec<PathBuf>,
    pub path_defines: Vec<(String, String)>,
    pub macro_uses: Vec<String>,
}

/// Given a list of paths, figure out their dependencies
pub fn all_sources_and_includes<I, E>(
    paths: I,
    includes: &[PathBuf],
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
) -> Result<Vec<SourceWithIncludes>>
where
    I: Iterator<Item = Result<PathBuf, E>>,
    E: Debug,
{
    let includes = Arc::new(Vec::from(includes));
    let virtual_paths = Arc::new(virtual_paths.cloned());
    let mut handles = Vec::new();

    for entry in paths {
        let path = match entry {
            Ok(value) => canonicalize_cached(value.clone(), virtual_paths.as_ref().as_ref())
                .map_err(|e| anyhow!("{:?}", e))?
                .ok_or(anyhow!(
                    "Required file not found {}",
                    value.to_string_lossy()
                ))?,
            Err(e) => return Err(anyhow!("{:?}", e)),
        };
        let includes = includes.clone();
        let virtual_paths = virtual_paths.clone();

        handles.push(std::thread::spawn(move || {
            let includes = match extract_includes(&path, &includes, virtual_paths.as_ref().as_ref())
            {
                Ok(value) => value,
                Err(e) => {
                    return Err(e);
                }
            };

            let (path_defines, macro_uses) = match scan_directives(&path) {
                // The scan is memoized, so this re-read is a cache hit.
                Ok(scan) => (scan.path_defines.clone(), scan.macro_uses.clone()),
                Err(_) => (Vec::new(), Vec::new()),
            };
            Ok(SourceWithIncludes { path, includes, path_defines, macro_uses })
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        let res = handle.join().map_err(|_| anyhow!("Join error"))?;
        results.push(res?);
    }

    Ok(results)
}

// Upstream PR 56 (obsidiansystems, open since 2026-07-23, no maintainer
// comment as of 2026-08-20). Adopted here rather than reinvented: a directive
// that pulls a file into the translation unit is a build INPUT, and one this
// driver did not infer is a missing input - the same failure shape as the
// eight input classes this fork already fixes, arriving through a directive
// nobody has hit yet. Cheaper to carry the upstream spelling now than to
// rediscover it as a ninth class from a resolved-derivation error.
static INCLUDE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r##"^\s*#\s*(?:include|embed)\s*(["<])([^">]*)[">]"##).unwrap()
});

/// One `#include`/`#embed` directive as written, before resolution.
/// `quoted` is true for `"name"`, false for `<name>`.
#[derive(Clone, Debug, PartialEq)]
pub struct Directive {
    pub quoted: bool,
    pub name: PathBuf,
}

// The DIRECTIVES of a file depend on its CONTENT alone; resolving them to
// paths depends on the include-dir set, which differs per translation unit.
// Splitting the two is clang-scan-deps' "minimized source" idea: the scan is
// cached content-keyed and shared across every TU, and only the cheap
// resolution re-runs per TU.
//
// Why it is worth having here: the BFS `visited` set is PER CALL, so a header
// included by N translation units was opened, read line by line and regex
// matched N times. On this graph N reaches the thousands for the common Qt
// and Chromium headers, and `discover` measured 3612 s of a 5199 s dyn phase.
//
// Validated by (mtime, size) so an edited header is re-scanned. Generated
// headers are written once by a task and then read, so they validate
// correctly rather than being pinned to a pre-generation read.
//
// DEFER(a person edits sources under a running driver): key on a content
// hash instead. A same-SIZE edit landing inside the filesystem's mtime
// granularity is invisible to this key, and the stale entry then pins a
// dependency set the file no longer has - silent, and in the direction that
// stops tracking a header rather than over-tracking one. It costs nothing
// for the build-from-a-store-checkout case this runs in, where sources do
// not change under the driver; it is exactly the case a developer editing
// in place would hit. The hash is one read per unique header, which a miss
// already pays, so the upgrade is cheap whenever that consumer appears.
// Raised by the specification session, addendum 730.
/// One file's scan: its include/embed directives, plus the raw material for
/// CROSS-FILE computed includes. `path_defines` are object-like macros whose
/// whole body is one string literal (path-shaped by construction);
/// `macro_uses` are `#include MACRO` tokens with no same-file define. The
/// same-file case resolves inside the scan (fftw); the cross-file case can
/// only resolve at the translation-unit walk, which sees every file's
/// defines (lzo: lzo1b_c.ch includes LZO_SEARCH_MATCH_INCLUDE_FILE, defined
/// in a config header two files away, 2026-08-23).
#[derive(Debug)]
pub struct ScanResult {
    pub directives: Vec<Directive>,
    pub path_defines: Vec<(String, String)>,
    pub macro_uses: Vec<String>,
}

type DirectiveCache =
    Arc<RwLock<rustc_hash::FxHashMap<PathBuf, (u64, u128, Arc<ScanResult>)>>>;
static DIRECTIVE_CACHE: LazyLock<DirectiveCache> = LazyLock::new(Default::default);

pub static SCAN_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SCAN_MISSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// (hits, misses) for the shared directive scan cache.
pub fn scan_stats() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (SCAN_HITS.load(Relaxed), SCAN_MISSES.load(Relaxed))
}

fn file_key(path: &Path) -> Option<(u64, u128)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((md.len(), mtime))
}

/// Parse a file's include directives, memoized across translation units.
pub fn scan_directives(path: &Path) -> Result<Arc<ScanResult>> {
    use std::sync::atomic::Ordering::Relaxed;
    let key = file_key(path);

    if let Some((len, mtime)) = key {
        if let Ok(cache) = DIRECTIVE_CACHE.read() {
            if let Some((l, m, dirs)) = cache.get(path) {
                if *l == len && *m == mtime {
                    SCAN_HITS.fetch_add(1, Relaxed);
                    return Ok(dirs.clone());
                }
            }
        }
    }

    let f =
        File::open(path).map_err(|e| anyhow!("Failed to open file {}: {}", path.display(), e))?;
    let mut directives = Vec::new();
    let mut macro_paths: std::collections::HashMap<String, String> = Default::default();
    let mut macro_uses: Vec<String> = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = match line {
            Ok(l) => l,
            // Usually this means the file isn't UTF-8 and we can skip.
            Err(_) => break,
        };
        if let Some(captures) = INCLUDE_REGEX.captures(&line) {
            directives.push(Directive {
                quoted: captures.get(1).unwrap().as_str() == "\"",
                name: PathBuf::from(captures.get(2).unwrap().as_str()),
            });
            continue;
        }
        // A COMPUTED INCLUDE THROUGH A SAME-FILE MACRO:
        //   #define SIMD_HEADER "simd-support/simd-sse2.h"
        //   #include SIMD_HEADER
        // The regex above sees neither, the header is never declared, and
        // the task dies "No such file or directory" (fftw, every SIMD
        // codelet, 2026-08-23). Only the one-file, string-literal form is
        // resolved - the general case is the preprocessor, which is not
        // this parser's job; an unresolved computed include still fails
        // loudly in the task, never silently.
        if let Some(rest) = line.trim_start().strip_prefix("#define ") {
            let mut it = rest.splitn(2, char::is_whitespace);
            if let (Some(name), Some(val)) = (it.next(), it.next()) {
                let val = val.trim();
                if val.len() > 2 && val.starts_with('"') && val.ends_with('"') && !name.contains('(') {
                    macro_paths.insert(name.to_string(), val[1..val.len() - 1].to_string());
                }
            }
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("#include ") {
            let token = rest.trim();
            if let Some(path) = macro_paths.get(token) {
                directives.push(Directive {
                    quoted: true,
                    name: PathBuf::from(path),
                });
            } else if !token.is_empty()
                && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                // A macro token with no same-file define: recorded for the
                // per-TU walk, where another file's define may resolve it.
                macro_uses.push(token.to_string());
            }
        }
    }

    let directives = Arc::new(ScanResult {
        directives,
        path_defines: macro_paths.into_iter().collect(),
        macro_uses,
    });
    SCAN_MISSES.fetch_add(1, Relaxed);
    if let Some((len, mtime)) = key {
        if let Ok(mut cache) = DIRECTIVE_CACHE.write() {
            cache.insert(path.to_path_buf(), (len, mtime, directives.clone()));
        }
    }
    Ok(directives)
}

/// Given a C-like source, try to resolve includes.
///
/// Includes are generally of the form `#include <name>` or `#include "name"`.
/// Also, C23 `#embed` resolves quoted names the same way.
pub fn extract_includes(
    path: &PathBuf,
    include_dirs: &[PathBuf],
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
) -> Result<Vec<PathBuf>> {
    let scan = scan_directives(path)?;
    let parent_dir = PathBuf::from(path.parent().unwrap());
    let mut result = Vec::new();

    // A HEADER REACHED THROUGH A SYMLINKED DIRECTORY IS DECLARED TWICE: at
    // its canonical path, which is what the cache and dedup key on, and at
    // the path AS SPELLED by the include dir plus the directive, which is
    // what the compiler will open inside the task sandbox. The sandbox
    // reproduces files, not the symlinks between them: alsa-lib's make runs
    // `ln -s ../include include/alsa` and its headers say
    // `#include <alsa/sound/type_compat.h>`; canonical alone put the file
    // at include/sound/type_compat.h and the compile died "No such file"
    // (2026-08-23, the first make package through the compiler drop-in).
    // Only relative spellings are added - an absolute spelling is a store
    // path or a system header, where symlinks are the store's own business.
    let mut push_both = |head: &Path, tail: &Path, canonical: PathBuf| {
        let spelled = lexical_normalize(&head.join(tail));
        if spelled != canonical && spelled.is_relative() {
            result.push(spelled);
        }
        result.push(canonical);
    };
    for d in scan.directives.iter() {
        if d.quoted {
            if let Some(p) = try_resolve(&parent_dir, &d.name, virtual_paths) {
                push_both(&parent_dir, &d.name, p);
                continue;
            }
        }
        if let Some((i, p)) = include_dirs
            .iter()
            .find_map(|i| try_resolve(i, &d.name, virtual_paths).map(|p| (i, p)))
        {
            push_both(i, &d.name, p);
        }
    }

    Ok(result)
}

/// Remove `.` and collapse `a/..` components without touching the
/// filesystem, so a symlink in the path is NOT followed - that is the point.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let last_is_parent = out
                    .components()
                    .next_back()
                    .is_some_and(|c| c == std::path::Component::ParentDir);
                if last_is_parent || !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn try_resolve(
    head: &Path,
    tail: &Path,
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
) -> Option<PathBuf> {
    canonicalize_cached(head.join(tail), virtual_paths).ok()?
}

// FxHash rather than SipHash: keys are long PathBufs probed a few times per
// include per include-dir, and hashing was 32% of driver CPU on the
// qtwebengine graph. Not DoS-facing - every key is a build-local path.
type PathCache = Arc<RwLock<rustc_hash::FxHashMap<PathBuf, Option<PathBuf>>>>;
static PATH_CACHE: LazyLock<PathCache> = LazyLock::new(Default::default);

pub fn canonicalize_cached<P>(
    path: P,
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
) -> Result<Option<PathBuf>, std::io::Error>
where
    P: AsRef<Path>,
    PathBuf: Borrow<P>,
    P: Hash + Eq,
{
    // Check virtual paths first if provided. Keyed lookup, not a pairwise
    // scan: PathBuf's Hash agrees with the Eq the scan used, and the map
    // grows with every materialized output, so the scan made each include
    // lookup O(V) - measured 25% of driver CPU in Components::next_back at
    // task 10,500 of the qtwebengine graph, the superlinear resolve climb.
    if let Some(virtual_paths) = virtual_paths {
        let key: &Path = path.as_ref();
        if let Some(actual_path) = virtual_paths.get::<Path>(key) {
            return Ok(Some(actual_path.clone()));
        }
    }

    {
        // Then try the cache.
        let cache = PATH_CACHE.read().unwrap();
        if let Some(cached) = cache.get(&path) {
            return Ok(cached.clone());
        }
    }

    // If cache-miss, then look it up ourselves.
    let result = if path.as_ref().exists() {
        Some(canonicalize(&path)?)
    } else {
        None
    };

    let mut cache = PATH_CACHE.write().unwrap();
    cache.insert(path.as_ref().to_path_buf(), result.clone());

    Ok(result)
}

#[cfg(test)]
mod tests {
    /// Both counter-touching tests take this: the scan counters are global,
    /// so two scanning tests in parallel see each other's increments.
    static SCAN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn computed_include_through_cross_file_define_is_declared() {
        // lzo, 2026-08-23: lzo1b_c.ch writes `#include
        // LZO_SEARCH_MATCH_INCLUDE_FILE` and the define lives in a config
        // header the same TU includes earlier. Same-file resolution cannot
        // see it; the walk-level table must.
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let d = std::env::temp_dir().join(format!("nnxf{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.h"),
            "#define SM_FILE \"sm_impl.h\"\n").unwrap();
        std::fs::write(d.join("body.h"),
            "#include SM_FILE\n").unwrap();
        std::fs::write(d.join("sm_impl.h"), "\n").unwrap();
        std::fs::write(d.join("main.c"),
            "#include \"config.h\"\n#include \"body.h\"\n").unwrap();
        let got = bfs_parse_includes(
            vec![d.join("main.c")], &[], None).unwrap();
        assert!(got.iter().any(|p| p.ends_with("sm_impl.h")),
            "cross-file computed include not declared: {:?}", got);
        // The negative control: a use whose macro is never defined stays
        // undeclared rather than inventing a path.
        std::fs::write(d.join("main2.c"),
            "#include NEVER_DEFINED\n").unwrap();
        let got2 = bfs_parse_includes(
            vec![d.join("main2.c")], &[], None).unwrap();
        assert_eq!(got2.len(), 1, "only the source itself: {:?}", got2);
    }

    #[test]
    fn computed_include_through_same_file_define_is_declared() {
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let f = std::env::temp_dir().join(format!("nn-ci-test-{}.c", std::process::id()));
        std::fs::write(&f, "#define SIMD_HEADER \"simd-support/simd-sse2.h\"\n#include SIMD_HEADER\n#include <math.h>\n#define FN(x) \"not-a-path\"\n#include UNKNOWN_MACRO\n").unwrap();
        let dirs = super::scan_directives(&f).unwrap();
        let names: Vec<String> = dirs.directives.iter().map(|d| d.name.to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"simd-support/simd-sse2.h".to_string()), "{names:?}");
        assert!(names.contains(&"math.h".to_string()));
        assert!(!names.iter().any(|n| n.contains("not-a-path")));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn lexical_normalize_keeps_symlinks_and_leading_parents() {
        use super::lexical_normalize;
        use std::path::PathBuf;
        assert_eq!(lexical_normalize(&PathBuf::from("../../include/./alsa/sound/x.h")),
                   PathBuf::from("../../include/alsa/sound/x.h"));
        assert_eq!(lexical_normalize(&PathBuf::from("a/b/../c.h")), PathBuf::from("a/c.h"));
        assert_eq!(lexical_normalize(&PathBuf::from("../a/../../c.h")), PathBuf::from("../../c.h"));
    }

    use super::*;

    // The virtual-path check moved from a pairwise Path == scan to a keyed
    // HashMap get. Those are equivalent only because PathBuf hashes over the
    // same components its Eq compares, which is what makes "a/./b" find an
    // entry keyed "a/b". This pins that: a differently spelled probe must
    // still hit, and a genuinely different path must miss.
    #[test]
    fn virtual_path_lookup_matches_scan_semantics() {
        let mut vp = HashMap::new();
        vp.insert(PathBuf::from("gen/a/b.h"), PathBuf::from("/store/x-b.h"));
        let hit = canonicalize_cached(PathBuf::from("gen/./a/b.h"), Some(&vp)).unwrap();
        assert_eq!(hit, Some(PathBuf::from("/store/x-b.h")));
        let miss = canonicalize_cached(PathBuf::from("gen/a/c.h"), Some(&vp)).unwrap();
        assert_eq!(miss, None);
    }

    // Upstream PR 56 ships the regex widening with no test. An inference rule
    // with no test is how the eight input classes this fork carries were each
    // found the expensive way - as a resolved-derivation failure thousands of
    // tasks into a build - so the rule gets pinned on arrival.
    //
    // The negative cases are the point. `embed` must match only as a
    // PREPROCESSOR DIRECTIVE: widening `include` to `(?:include|embed)` puts a
    // very common English word in a pattern that scans every line of every
    // source file, and a false positive here does not fail loudly - it invents
    // an input, which resolves to a missing store path much later.
    #[test]
    fn include_regex_covers_embed_without_over_matching() {
        let captured = |line: &str| {
            INCLUDE_REGEX
                .captures(line)
                .map(|c| (c[1].to_string(), c[2].to_string()))
        };

        // C23 #embed, both spellings, and the whitespace forms the regex allows
        assert_eq!(
            captured(r##"#embed "logo.png""##),
            Some(("\"".into(), "logo.png".into()))
        );
        assert_eq!(
            captured("  #  embed <data.bin>"),
            Some(("<".into(), "data.bin".into()))
        );
        // the directive it was widened from still works
        assert_eq!(
            captured(r##"#include "foo.h""##),
            Some(("\"".into(), "foo.h".into()))
        );

        // NOT directives: a word in prose, an identifier, a member call, and a
        // string mentioning the directive. Each would fabricate an input.
        assert_eq!(captured("// we embed \"logo.png\" here"), None);
        assert_eq!(captured("embed \"logo.png\""), None);
        assert_eq!(captured("x.embed(\"logo.png\");"), None);
        assert_eq!(captured(r##"const char *s = "#embed \"a\"";"##), None);
    }

    /// The scan cache must (a) actually hit across translation units, which
    /// is the whole point, and (b) NOTICE an edit, because a cache that never
    /// invalidates is worse than no cache: it pins a stale dependency set and
    /// the build silently stops tracking a header.
    #[test]
    fn directive_cache_hits_across_tus_and_invalidates_on_edit() {
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("nnscan{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let h = dir.join("shared.h");
        std::fs::File::create(&h)
            .unwrap()
            .write_all(b"#include \"a.h\"\n")
            .unwrap();

        let before = scan_stats();
        let first = scan_directives(&h).unwrap();
        assert_eq!(first.directives.len(), 1, "one directive parsed");
        assert_eq!(first.directives[0].name, PathBuf::from("a.h"));

        // Every later translation unit that reaches this header must hit.
        for _ in 0..5 {
            let again = scan_directives(&h).unwrap();
            assert_eq!(again.directives.len(), 1);
        }
        let after = scan_stats();
        assert_eq!(after.1 - before.1, 1, "parsed exactly once");
        assert_eq!(after.0 - before.0, 5, "five cross-TU hits");

        // An edit must be seen. Size differs here, and the guard also carries
        // mtime for a same-size edit.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::File::create(&h)
            .unwrap()
            .write_all(b"#include \"a.h\"\n#include <b.h>\n")
            .unwrap();
        let edited = scan_directives(&h).unwrap();
        assert_eq!(edited.directives.len(), 2, "edit was NOT seen - cache is pinning stale deps");
        assert!(!edited.directives[1].quoted, "angle-bracket form preserved");
        assert_eq!(scan_stats().1 - after.1, 1, "the edit cost one re-parse");

        std::fs::remove_dir_all(&dir).ok();
    }
}
