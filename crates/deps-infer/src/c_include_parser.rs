use crate::gcc_include_parser;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::fs::canonicalize;
use std::hash::Hash;

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

pub fn retrieve_c_includes(
    cmdline: &str,
    files: Vec<PathBuf>,
    virtual_paths: Option<HashMap<PathBuf, PathBuf>>,
) -> Result<Vec<PathBuf>> {
    Ok(retrieve_c_includes_checked(cmdline, files, virtual_paths)?.0)
}

/// The same walk, plus whether the scan KNOWS it is incomplete.
///
/// The bool is true when some translation unit carries an `#include` this
/// parser cannot resolve - a function-like macro call, a concatenation - or
/// a plain macro use that no file in the walk ever defined. The caller is
/// expected to fall back to the preprocessor for that TU. Returning it
/// separately keeps the fast path fast: nothing extra runs for the
/// overwhelming majority of translation units, where the scan is exact.
pub fn retrieve_c_includes_checked(
    cmdline: &str,
    files: Vec<PathBuf>,
    virtual_paths: Option<HashMap<PathBuf, PathBuf>>,
) -> Result<(Vec<PathBuf>, bool)> {
    let includes = gcc_include_parser::parse_include_dirs(cmdline)?;
    bfs_parse_includes(files, &includes, virtual_paths)
}

/// Recursively collect all dependencies using BFS
fn bfs_parse_includes(
    files: Vec<PathBuf>,
    include_dirs: &[PathBuf],
    virtual_paths: Option<HashMap<PathBuf, PathBuf>>,
) -> Result<(Vec<PathBuf>, bool)> {
    // Set by any file carrying a directive this parser cannot expand.
    let mut incomplete = false;
    let mut visited = rustc_hash::FxHashSet::default();
    let mut result = Vec::new();
    let mut queue = VecDeque::new();
    // CROSS-FILE COMPUTED INCLUDES. Defines accumulate over the whole walk
    // and uses wait until their macro appears - preprocessing order is not
    // modeled, which can only OVER-declare (harmless: an extra input), never
    // under-declare relative to the old behavior of dropping the use.
    // Name -> every value seen this TU, because a use resolves against
    // ALL of them (guarded defaults vs root overrides; over-declaration is
    // the safe direction). Uses are kept for the whole walk so a value
    // arriving after the use still resolves; `resolved` dedups the pairs.
    let mut tu_defines: rustc_hash::FxHashMap<String, Vec<String>> = Default::default();
    let mut pending_uses: Vec<(PathBuf, String)> = Vec::new();
    let mut resolved: rustc_hash::FxHashSet<(PathBuf, String, String)> = Default::default();

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
                let vals = tu_defines.entry(k).or_default();
                if !vals.contains(&v) {
                    vals.push(v);
                }
            }
            if !source.computed_unresolvable.is_empty() {
                incomplete = true;
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
        for (dir, name) in pending_uses.iter() {
            let Some(vals) = tu_defines.get(name) else {
                continue;
            };
            for val in vals {
                let key = (dir.clone(), name.clone(), val.clone());
                if resolved.contains(&key) {
                    continue;
                }
                let tail = PathBuf::from(val);
                // The spelled declaration must join the HEAD THAT RESOLVED,
                // not the use's includer dir: libpng's mips_init.c names
                // "contrib/mips-msa/linux.c", which resolves through -I.
                // (the build root), and joining the includer dir instead
                // fabricated mips/contrib/... - a path that exists nowhere
                // and hard-failed the upload (2026-08-23).
                let hit = try_resolve(dir, &tail, virtual_paths.as_ref())
                    .map(|p| (dir.clone(), p))
                    .or_else(|| {
                        include_dirs.iter().find_map(|i| {
                            try_resolve(i, &tail, virtual_paths.as_ref()).map(|p| (i.clone(), p))
                        })
                    });
                if let Some((head, p)) = hit {
                    resolved.insert(key);
                    let spelled = lexical_normalize(&head.join(&tail));
                    if spelled != p && spelled.is_relative() && visited.insert(spelled.clone()) {
                        result.push(spelled);
                    }
                    if visited.insert(p.clone()) {
                        queue.push_back(p.clone());
                        result.push(p);
                    }
                }
            }
        }
    }

    // A use whose macro no file ever defined is the OTHER way the scan can
    // be wrong, and it had the same silent ending: the loop above simply
    // skips it. It is the same verdict - this walk cannot say what that
    // directive names - so it takes the same fallback.
    if pending_uses
        .iter()
        .any(|(_, name)| !tu_defines.contains_key(name))
    {
        incomplete = true;
    }

    Ok((result, incomplete))
}

#[derive(Debug, PartialEq, PartialOrd)]
pub struct SourceWithIncludes {
    pub path: PathBuf,
    pub includes: Vec<PathBuf>,
    pub path_defines: Vec<(String, String)>,
    pub macro_uses: Vec<String>,
    pub computed_unresolvable: Vec<String>,
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
        // The canonical path is where the CONTENT is read (a virtual or
        // symlinked file resolves to its real bytes); the path AS QUEUED
        // is where the compiler stands when it resolves the file's quoted
        // includes, so resolution must run from the spelled parent. nspr,
        // 2026-08-23: canonicalizing before the scan collapsed
        // dist/include/nspr/prtypes.h (a symlink) to its source location,
        // so its "obsolete/protypes.h" was declared only at the source
        // spelling and the sandbox compile died at the dist one.
        let (spelled, path) = match entry {
            Ok(value) => {
                let canon = canonicalize_cached(value.clone(), virtual_paths.as_ref().as_ref())
                    .map_err(|e| anyhow!("{:?}", e))?
                    .ok_or(anyhow!(
                        "Required file not found {}",
                        value.to_string_lossy()
                    ))?;
                (value, canon)
            }
            Err(e) => return Err(anyhow!("{:?}", e)),
        };
        let includes = includes.clone();
        let virtual_paths = virtual_paths.clone();

        handles.push(std::thread::spawn(move || {
            let mut includes =
                match extract_includes(&path, &spelled, &includes, virtual_paths.as_ref().as_ref())
                {
                    Ok(value) => value,
                    Err(e) => {
                        return Err(e);
                    }
                };
            // A preprocessed Fortran source needs the file its line markers
            // name, which no include directive mentions.
            for origin in fortran_pp_origins(&path, &spelled) {
                if !includes.contains(&origin) {
                    includes.push(origin);
                }
            }

            let (path_defines, macro_uses, computed_unresolvable) = match scan_directives(&path) {
                // The scan is memoized, so this re-read is a cache hit.
                Ok(scan) => (
                    scan.path_defines.clone(),
                    scan.macro_uses.clone(),
                    scan.computed_unresolvable.clone(),
                ),
                Err(_) => (Vec::new(), Vec::new(), Vec::new()),
            };
            // `path` carries the SPELLED location onward: bfs resolves a
            // later computed include from this file's parent, and the
            // compiler resolves from where the file was reached, not from
            // its canonical home.
            Ok(SourceWithIncludes {
                path: spelled,
                includes,
                path_defines,
                macro_uses,
                computed_unresolvable,
            })
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
static INCLUDE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r##"^\s*#\s*(?:include|embed)\s*(["<])([^">]*)[">]"##).unwrap());

/// nasm/yasm spell an include `%include "name"` - always quoted, resolved
/// through the includer's directory and then -I dirs, same as a quoted C
/// include. The C regex is anchored on `#`, so .asm sources scanned as
/// zero includes and their x86inc.asm-style helpers were never uploaded:
/// libvmaf's cpuid.asm died `unable to open include file` with the file
/// sitting in the source tree (tenth class, 2026-08-23).
static NASM_INCLUDE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*%\s*include\s+"([^"]*)""#).unwrap());

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
    /// `#include` tokens this parser can NEVER resolve: function-like macro
    /// calls and concatenations, which need real macro expansion. Recorded
    /// rather than dropped, because a scan that cannot say it is incomplete
    /// is indistinguishable from one that found everything, and the caller
    /// then declares an input set it has no reason to trust. cmake's kwsys
    /// is the measured case: `#include KWSYS_HEADER(Directory.hxx)` with
    /// `-DKWSYS_NAMESPACE=cmsys`, resolvable only by the preprocessor.
    pub computed_unresolvable: Vec<String>,
}

type DirectiveCache = Arc<RwLock<rustc_hash::FxHashMap<PathBuf, (u64, u128, Arc<ScanResult>)>>>;
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

/// Strip `#\s*<kw>\s+` from a directive line, returning the trimmed rest.
/// The main INCLUDE_REGEX tolerates whitespace after the `#` and so must
/// this scanner: lzo writes `#  define LZO_SEARCH_MATCH_INCLUDE_FILE ...`
/// and `#  include LZO_SEARCH_MATCH_INCLUDE_FILE` (two spaces), which the
/// glued-literal `strip_prefix("#define ")` missed while the ordinary
/// include regex matched fine - so only the computed-include half went
/// blind, and the task died on the undeclared header (2026-08-23).
fn directive_rest<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let s = line.trim_start().strip_prefix('#')?.trim_start();
    let rest = s.strip_prefix(kw)?;
    if rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
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

    // BYTES, NOT lines(): BufReader::lines errors on the first line that
    // is not valid UTF-8, and the old `Err(_) => break` ABANDONED the rest
    // of the file - a comment said "skip" while the code said stop. groff's
    // lbp.cpp is ISO-8859 (an accented author name on LINE 2), so the scan
    // saw zero of its includes, the task shipped with only the TU, and the
    // compile died on <config.h> (2026-08-23, seventh class). Directives
    // are ASCII; a lossy per-line decode preserves every one of them and
    // mangles only the prose the scanner never reads.
    let data = std::fs::read(path)
        .map_err(|e| anyhow!("Failed to read file {}: {}", path.display(), e))?;
    let mut directives = Vec::new();
    let mut macro_paths: std::collections::HashMap<String, String> = Default::default();
    let mut macro_uses: Vec<String> = Vec::new();
    let mut computed_unresolvable: Vec<String> = Vec::new();
    for raw in data.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        let line = line.as_ref();
        if let Some(captures) = INCLUDE_REGEX.captures(line) {
            directives.push(Directive {
                quoted: captures.get(1).unwrap().as_str() == "\"",
                name: PathBuf::from(captures.get(2).unwrap().as_str()),
            });
            continue;
        }
        if let Some(captures) = NASM_INCLUDE_REGEX.captures(line) {
            directives.push(Directive {
                quoted: true,
                name: PathBuf::from(captures.get(1).unwrap().as_str()),
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
        if let Some(rest) = directive_rest(line, "define") {
            let mut it = rest.splitn(2, char::is_whitespace);
            if let (Some(name), Some(val)) = (it.next(), it.next()) {
                let val = val.trim();
                if val.len() > 2
                    && val.starts_with('"')
                    && val.ends_with('"')
                    && !name.contains('(')
                {
                    macro_paths.insert(name.to_string(), val[1..val.len() - 1].to_string());
                }
            }
            continue;
        }
        if let Some(rest) = directive_rest(line, "include") {
            let token = rest.trim();
            if let Some(path) = macro_paths.get(token) {
                directives.push(Directive {
                    quoted: true,
                    name: PathBuf::from(path),
                });
                // AND still a use for the walk. A same-file define is often
                // a GUARDED DEFAULT (#if !defined ... #define ... "x.ch")
                // that the TU's root file overrides two files up, so the
                // same-file value alone under-declares exactly the header
                // the preprocessor picks (lzo, third failure, 2026-08-23).
                // The walk over-declares both candidates; an extra input is
                // harmless, a missing one kills the task.
                if token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    macro_uses.push(token.to_string());
                }
            } else if !token.is_empty()
                && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                // A macro token with no same-file define: recorded for the
                // per-TU walk, where another file's define may resolve it.
                macro_uses.push(token.to_string());
            } else if !token.is_empty() {
                // ANYTHING ELSE IS BEYOND THIS PARSER, AND SAYING SO IS THE
                // POINT. A function-like call or a concatenation needs macro
                // expansion with the command line's -D set. Dropping it
                // silently is what let cmake's kwsys reach a task with no
                // cmsys/ headers declared; recording it lets the caller
                // fall back to the preprocessor for this TU alone.
                computed_unresolvable.push(token.to_string());
            }
        }
    }

    let directives = Arc::new(ScanResult {
        directives,
        path_defines: macro_paths.into_iter().collect(),
        macro_uses,
        computed_unresolvable,
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
/// Is this path one the caller declared as a build-dir file that may not
/// exist yet? `virtual_paths` is built as build_path -> build_path, so a
/// queued include can match either side; both are checked rather than
/// assuming the identity mapping, which is the caller's choice and not this
/// module's contract. Linear over the values only on a read failure, which
/// is rare by construction.
pub fn is_declared_virtual(path: &Path, virtual_paths: Option<&HashMap<PathBuf, PathBuf>>) -> bool {
    let Some(vp) = virtual_paths else {
        return false;
    };
    // KEY LOOKUP ONLY. The caller builds this map as build_path -> build_path
    // and `canonicalize_cached` returns the VALUE for a key hit, so the path
    // the BFS queues is literally a key here. A `values().any()` fallback
    // looked defensive and was an O(V) scan over the same map whose pairwise
    // scanning was once measured at 25% of driver CPU - see
    // canonicalize_cached. Defensive code on a hot map is not free.
    vp.contains_key(path)
}

/// The original source a preprocessed Fortran file was made from.
///
/// A COMPILER OPENS A FILE THE GRAPH NEVER DECLARED. CMake compiles Fortran in
/// two edges: one preprocesses `x.f` into `x.f-pp.f`, and a second compiles the
/// `-pp.f` with `-fpreprocessed`. Only the `-pp.f` is declared on that second
/// edge, because under ordinary ninja the original is simply still on disk.
/// gfortran follows the `# 1 "x.f"` line markers back to it and dies
/// `Fatal Error: ...: No such file or directory` when it is absent, which is
/// what a sandbox that materializes only declared inputs gives it.
///
/// Measured on liblapack 2026-08-31, whose ILP64 variant GENERATES its sources
/// into the build directory, so the file is not merely undeclared here but
/// absent until another task writes it.
///
/// Scoped to CMake's `-pp.` spelling rather than applied to every file: line
/// markers appear in any preprocessed output, and treating them as
/// dependencies everywhere would declare the whole system include set of any
/// preprocessed C this scanner is ever handed.
fn fortran_pp_origins(path: &Path, spelled: &Path) -> Vec<PathBuf> {
    let is_pp = spelled
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("-pp."));
    if !is_pp {
        return Vec::new();
    }
    let Ok(text) = std::fs::read(path) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for line in String::from_utf8_lossy(&text).lines() {
        let Some(rest) = line.strip_prefix("# ") else {
            continue;
        };
        // `# <lineno> "<file>"`, and the line number is what separates a
        // marker from a comment that happens to start with a hash.
        let mut parts = rest.splitn(2, ' ');
        if !parts.next().is_some_and(|n| n.parse::<u64>().is_ok()) {
            continue;
        }
        let Some(quoted) = parts.next() else { continue };
        let Some(name) = quoted
            .strip_prefix('"')
            .and_then(|q| q.split('"').next())
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        // `<built-in>` and `<command-line>` are the preprocessor naming
        // itself, not files. Queuing one makes the scan demand a path that
        // cannot exist, which fails the edge rather than the lookup.
        if name.starts_with('<') {
            continue;
        }
        let candidate = PathBuf::from(name);
        // The marker names the `-pp.f` itself as well as its origin; the
        // scanner is being asked what ELSE this file needs.
        if candidate == spelled || candidate == path {
            continue;
        }
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

pub fn extract_includes(
    path: &Path,
    spelled: &Path,
    include_dirs: &[PathBuf],
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
) -> Result<Vec<PathBuf>> {
    // A GENERATED HEADER IS RESOLVABLE BEFORE IT EXISTS, AND SCANNING IT
    // IS WHAT KILLED THE FIRST REAL PACKAGE TO CARRY ONE. `virtual_paths`
    // deliberately resolves a declared-but-absent build-dir file so the
    // include is DECLARED; canonicalize_cached returns it with no existence
    // check, the BFS queues it like any other header, and this read then
    // died `No such file or directory`, failing the whole task derivation.
    // Measured 2026-08-30 driving `example-nix`: meson generates
    // `src/nix/nix.p/unpack-channel.nix.gen.hh` with a CUSTOM_COMMAND and
    // declares it ORDER-ONLY (`||`) on every compile edge in `src/nix`, so
    // `#include "unpack-channel.nix.gen.hh"` resolves through `-I` to a
    // path nothing has written yet. This is failure class 3 (libvmaf's
    // `vcs_version.h`) and class 4 (liblapack's `VerifyFortran.h`) reaching
    // a package that actually builds.
    //
    // A file the caller declared virtual contributes NO directives rather
    // than an error. That under-declares whatever the generated header
    // itself includes, and the static pass cannot do better - the bytes do
    // not exist yet to be read.
    //
    // WHAT COVERS THAT UNDER-DECLARATION, PRECISELY, because an earlier
    // version of this comment overclaimed it. `discover_c_includes` has
    // exactly two callers: `build_task_derivation` (this static pass) and
    // `discover_dynamic_dependencies` (inside the sandbox, where the
    // producing edge has run and the file is real). The second only happens
    // for an edge whose `deps = gcc` - `handle_derivation_result` builds
    // `built_inputs` under that test alone - so:
    //
    //   deps = gcc          the dynamic pass re-runs discovery and recovers
    //                       whatever this skipped. Covered.
    //   depfile, no deps    NO dynamic derivation is emitted, so nothing
    //                       re-runs. Not covered, and the failure is LOUD:
    //                       the task dies in the sandbox on the missing
    //                       include rather than shipping a wrong artifact.
    //
    // The loud half is acceptable and the silent half does not exist, which
    // is the property that matters. Saying "covered by design" of both was
    // wrong.
    //
    // GATED ON THE FILE BEING DECLARED VIRTUAL, never on the read failing.
    // A source that is simply missing is a real defect and must still fail
    // loudly; swallowing every NotFound here would turn every genuinely
    // absent header into a silently under-declared task, which is the
    // failure shape this whole module exists to prevent.
    let scan = match scan_directives(path) {
        Ok(scan) => scan,
        Err(e) => {
            if is_declared_virtual(path, virtual_paths) && !path.exists() {
                return Ok(Vec::new());
            }
            return Err(e);
        }
    };
    // Quoted includes resolve from where the file was REACHED (spelled),
    // which differs from the canonical parent when the file was reached
    // through a symlink; see the caller.
    let parent_dir = PathBuf::from(spelled.parent().unwrap_or_else(|| path.parent().unwrap()));
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
        // THE KEY IS A SPELLING AND THE MAP HOLDS GRAPH PATHS, so probing it
        // verbatim asks a question the map cannot answer. `#include
        // "../config.h"` from a compile edge in `coregrind` arrives here as
        // `<dir>/coregrind/../config.h` while the graph declares
        // `<dir>/config.h`; the lookup misses and the header is then scanned
        // off a disk the build has not written it to yet. `./x` against a
        // declared `x` misses the same way, which is what made the
        // generated-header example resolve nothing while still passing.
        //
        // Lexical rather than `canonicalize`: the header does not exist yet,
        // so the filesystem cannot normalize it, and this is a map key rather
        // than a path anything opens.
        //
        // Second probe, not first: the verbatim hit is the common case and
        // already keyed, so a normalized path is built only when it misses.
        let normalized = lexical_normalize(key);
        if normalized != key {
            if let Some(actual_path) = virtual_paths.get::<Path>(normalized.as_path()) {
                return Ok(Some(actual_path.clone()));
            }
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
    // A DIRECTORY IS NOT A HEADER, AND `exists()` SAYS YES TO ONE. gcc
    // skips a directory found on the include path and keeps searching;
    // this returned it as the resolved include, the BFS queued it, and
    // scan_directives' fs::read died EISDIR:
    //     Failed to read file /build/source/webrtc/rtc_base/memory: Is a
    //     directory (os error 21)
    // webrtc-audio-processing 1.3 and 2.1, every compile edge in the
    // graph: `#include <memory>` against an include dir holding a
    // `memory/` subdirectory (`modules/audio_processing/utility` the
    // same way). Same shape as the directory-output defect fixed in
    // patchelf.rs, and the same remedy - refusing here, at the single
    // resolution point, lets the search fall through to the next include
    // dir exactly as the compiler does.
    let result = match std::fs::metadata(path.as_ref()) {
        Ok(md) if !md.is_dir() => Some(canonicalize(&path)?),
        _ => None,
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
    fn function_like_computed_include_reports_the_scan_incomplete() {
        // cmake's kwsys, 2026-08-24: kwsysPrivate.h defines
        //   #define KWSYS_HEADER1(x) <x>
        //   #define KWSYS_HEADER(x)  KWSYS_HEADER1(KWSYS_NAMESPACE/x)
        // and every source writes `#include KWSYS_HEADER(Directory.hxx)`,
        // with KWSYS_NAMESPACE=cmsys arriving as -D on the command line.
        // No textual parser resolves that. What it must NOT do is stay
        // silent: the driver declared the task's inputs from a scan that
        // had quietly dropped the directive, and every kwsys TU died on a
        // missing cmsys/ header.
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let d = std::env::temp_dir().join(format!("nnkw{}", std::process::id()));
        // Named by pid alone, so a previous run of this same test can leave the
        // directory behind and `create_dir_all` then races its contents.
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("kwsysPrivate.h"),
            "#define KWSYS_HEADER1(x) <x>\n\
             #define KWSYS_HEADER(x) KWSYS_HEADER1(KWSYS_NAMESPACE/x)\n",
        )
        .unwrap();
        std::fs::write(
            d.join("Directory.cxx"),
            "#include \"kwsysPrivate.h\"\n#include KWSYS_HEADER(Directory.hxx)\n",
        )
        .unwrap();
        let (_got, incomplete) =
            bfs_parse_includes(vec![d.join("Directory.cxx")], &[], None).unwrap();
        assert!(
            incomplete,
            "a function-like computed include must report the scan incomplete, \
             or the driver declares inputs it has no reason to trust"
        );

        // THE NEGATIVE CONTROL, and it is the half that keeps this cheap.
        // The fallback runs a real preprocessor, so a detector that fired on
        // ordinary sources would pay that per object and hand back exactly
        // the time per-TU derivations exist to save. An everyday file with
        // plain quoted and angled includes must come back complete.
        std::fs::write(d.join("plain.h"), "\n").unwrap();
        std::fs::write(
            d.join("plain.c"),
            "#include <stdio.h>\n#include \"plain.h\"\n",
        )
        .unwrap();
        let (got2, incomplete2) = bfs_parse_includes(vec![d.join("plain.c")], &[], None).unwrap();
        assert!(
            !incomplete2,
            "an ordinary source must NOT trigger the preprocessor fallback: {got2:?}"
        );
    }

    #[test]
    fn computed_include_through_cross_file_define_is_declared() {
        // lzo, 2026-08-23: lzo1b_c.ch writes `#include
        // LZO_SEARCH_MATCH_INCLUDE_FILE` and the define lives in a config
        // header the same TU includes earlier. Same-file resolution cannot
        // see it; the walk-level table must.
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let d = std::env::temp_dir().join(format!("nnxf{}", std::process::id()));
        // Named by pid alone, so a previous run of this same test can leave the
        // directory behind and `create_dir_all` then races its contents.
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.h"), "#define SM_FILE \"sm_impl.h\"\n").unwrap();
        std::fs::write(d.join("body.h"), "#include SM_FILE\n").unwrap();
        std::fs::write(d.join("sm_impl.h"), "\n").unwrap();
        std::fs::write(
            d.join("main.c"),
            "#include \"config.h\"\n#include \"body.h\"\n",
        )
        .unwrap();
        let got = bfs_parse_includes(vec![d.join("main.c")], &[], None)
            .unwrap()
            .0;
        assert!(
            got.iter().any(|p| p.ends_with("sm_impl.h")),
            "cross-file computed include not declared: {:?}",
            got
        );
        // The negative control: a use whose macro is never defined stays
        // undeclared rather than inventing a path.
        std::fs::write(d.join("main2.c"), "#include NEVER_DEFINED\n").unwrap();
        let got2 = bfs_parse_includes(vec![d.join("main2.c")], &[], None)
            .unwrap()
            .0;
        assert_eq!(got2.len(), 1, "only the source itself: {:?}", got2);
    }

    #[test]
    fn guarded_default_declares_both_candidate_headers() {
        // lzo's third failure, 2026-08-23: lzo1_99.c defines
        // LZO_CODE_MATCH_INCLUDE_FILE "lzo1_cm.ch", then includes a .ch
        // that carries a guarded default for the same macro and the
        // `#include MACRO`. The same-file resolution alone declares only
        // the default; the preprocessor picks the root's value. Both must
        // be declared.
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let d = std::env::temp_dir().join(format!("nn-gd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("root.c"),
            "#define CM_FILE \"root_cm.ch\"\n#include \"body.ch\"\n",
        )
        .unwrap();
        std::fs::write(
            d.join("body.ch"),
            "#if !defined(CM_FILE)\n#  define CM_FILE \"body_cm.ch\"\n#endif\n#  include CM_FILE\n",
        )
        .unwrap();
        std::fs::write(d.join("root_cm.ch"), "\n").unwrap();
        std::fs::write(d.join("body_cm.ch"), "\n").unwrap();
        let got = bfs_parse_includes(vec![d.join("root.c")], &[], None)
            .unwrap()
            .0;
        let names: Vec<String> = got
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n.ends_with("root_cm.ch")),
            "root override missing: {names:?}"
        );
        assert!(
            names.iter().any(|n| n.ends_with("body_cm.ch")),
            "guarded default missing: {names:?}"
        );
    }

    #[test]
    fn computed_include_resolving_through_include_dir_keeps_that_head() {
        // libpng mips_init.c: the computed include's value resolves via
        // -I. (build root), not the includer's own dir; joining the
        // includer dir fabricated mips/contrib/... which exists nowhere.
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let d = std::env::temp_dir().join(format!("nn-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("contrib")).unwrap();
        std::fs::create_dir_all(d.join("mips")).unwrap();
        std::fs::write(d.join("contrib/x.c"), "\n").unwrap();
        std::fs::write(
            d.join("mips/init.c"),
            "#define F \"contrib/x.c\"\n#include F\n",
        )
        .unwrap();
        let got = bfs_parse_includes(vec![d.join("mips/init.c")], std::slice::from_ref(&d), None)
            .unwrap()
            .0;
        let names: Vec<String> = got
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n.ends_with("contrib/x.c")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("mips/contrib")),
            "fabricated spelling declared: {names:?}"
        );
    }

    #[test]
    fn nasm_percent_include_is_scanned() {
        // `scan_stats` is a pair of GLOBAL counters and the directive cache is
        // global too, so a test that parses without this lock is counted by
        // whichever test is reading those counters at the time. That is the
        // whole of the intermittent "parsed exactly once: left 2".
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("nn-nasm-{}", std::process::id()));
        // Named by pid alone, so a previous run of this same test can leave the
        // directory behind and `create_dir_all` then races its contents.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("cpuid.asm");
        std::fs::write(&src, b"; x86 helpers\n%include \"ext/x86/x86inc.asm\"\n%include\t\"config.asm\"\ncglobal cpu_cpuid\n").unwrap();
        let scan = scan_directives(&src).unwrap();
        let names: Vec<_> = scan.directives.iter().map(|d| d.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                PathBuf::from("ext/x86/x86inc.asm"),
                PathBuf::from("config.asm")
            ]
        );
        assert!(scan.directives.iter().all(|d| d.quoted));
    }

    #[test]
    fn non_utf8_line_does_not_end_the_scan() {
        // `scan_stats` is a pair of GLOBAL counters and the directive cache is
        // global too, so a test that parses without this lock is counted by
        // whichever test is reading those counters at the time. That is the
        // whole of the intermittent "parsed exactly once: left 2".
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        // groff's lbp.cpp: an ISO-8859 byte in a comment on line 2, every
        // include after it. The old lines()-based loop broke at the bad
        // byte and declared ZERO includes; the task then died on the first
        // missing header. The scan must survive the byte and keep every
        // directive that follows it.
        let dir = std::env::temp_dir().join(format!("nn-8859-{}", std::process::id()));
        // Named by pid alone, so a previous run of this same test can leave the
        // directory behind and `create_dir_all` then races its contents.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("lbp.cpp");
        let mut body = b"/*\n   Written by Francisco Andr\xe9s Verd\xfa\n*/\n".to_vec();
        body.extend_from_slice(b"#include <config.h>\n#include \"lbp.h\"\n");
        std::fs::write(&src, body).unwrap();
        let scan = scan_directives(&src).unwrap();
        let names: Vec<_> = scan.directives.iter().map(|d| d.name.clone()).collect();
        assert_eq!(
            names,
            vec![PathBuf::from("config.h"), PathBuf::from("lbp.h")]
        );
        assert!(!scan.directives[0].quoted);
        assert!(scan.directives[1].quoted);
    }

    #[test]
    fn directive_with_space_after_hash_still_scans() {
        // lzo again, 2026-08-23: it spells the pair `#  define` and
        // `#  include`. The first fix matched the glued literal only, so
        // lzo failed a second time on the identical symptom with the fix
        // for it already shipped. Same fixture, indented spelling.
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let f = std::env::temp_dir().join(format!("nn-sp-test-{}.c", std::process::id()));
        std::fs::write(&f, "#  define SM_INC \"sm2_impl.h\"\n#  include SM_INC\n").unwrap();
        let dirs = super::scan_directives(&f).unwrap();
        let names: Vec<String> = dirs
            .directives
            .iter()
            .map(|d| d.name.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"sm2_impl.h".to_string()), "{names:?}");
        // `#definexyz` must not parse as a define.
        assert!(super::directive_rest("#definexyz A \"b.h\"", "define").is_none());
    }

    #[test]
    fn symlinked_include_dir_declares_nested_quoted_include_at_both_spellings() {
        // nspr, 2026-08-23: dist/include/nspr/prtypes.h is a symlink to
        // pr/include/prtypes.h, which includes "obsolete/protypes.h";
        // dist/include/nspr/obsolete/protypes.h is also a symlink. The
        // compiler resolves the nested quoted include at the SPELLED
        // location, so the scan must declare it there too, not only at
        // the canonical source spelling.
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let d = std::env::temp_dir().join(format!("nn-nspr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["pr/include/obsolete", "dist/include/nspr/obsolete", "lib"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        std::fs::write(
            d.join("pr/include/prtypes.h"),
            "#include \"obsolete/protypes.h\"\n",
        )
        .unwrap();
        std::fs::write(d.join("pr/include/obsolete/protypes.h"), "\n").unwrap();
        std::os::unix::fs::symlink(
            "../../../pr/include/prtypes.h",
            d.join("dist/include/nspr/prtypes.h"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "../../../../pr/include/obsolete/protypes.h",
            d.join("dist/include/nspr/obsolete/protypes.h"),
        )
        .unwrap();
        std::fs::write(d.join("lib/strlen.c"), "#include \"prtypes.h\"\n").unwrap();
        // Relative paths, as the real cmdline spells them
        // (-I../../../dist/include/nspr from lib/libc/src): the spelled
        // declaration is gated on is_relative, so an absolute fixture
        // tests a different branch than the defect lives in.
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(d.join("lib")).unwrap();
        let got = bfs_parse_includes(
            vec![PathBuf::from("strlen.c")],
            &[PathBuf::from("../dist/include/nspr")],
            None,
        );
        std::env::set_current_dir(prev).unwrap();
        let got = got.unwrap().0;
        let names: Vec<String> = got
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.contains("dist/include/nspr/obsolete/protypes.h")),
            "nested quoted include missing its spelled location: {names:?}"
        );
    }

    #[test]
    fn computed_include_through_same_file_define_is_declared() {
        let _g = SCAN_TEST_LOCK.lock().unwrap();
        let f = std::env::temp_dir().join(format!("nn-ci-test-{}.c", std::process::id()));
        std::fs::write(&f, "#define SIMD_HEADER \"simd-support/simd-sse2.h\"\n#include SIMD_HEADER\n#include <math.h>\n#define FN(x) \"not-a-path\"\n#include UNKNOWN_MACRO\n").unwrap();
        let dirs = super::scan_directives(&f).unwrap();
        let names: Vec<String> = dirs
            .directives
            .iter()
            .map(|d| d.name.to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"simd-support/simd-sse2.h".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"math.h".to_string()));
        assert!(!names.iter().any(|n| n.contains("not-a-path")));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn lexical_normalize_keeps_symlinks_and_leading_parents() {
        use super::lexical_normalize;
        use std::path::PathBuf;
        assert_eq!(
            lexical_normalize(&PathBuf::from("../../include/./alsa/sound/x.h")),
            PathBuf::from("../../include/alsa/sound/x.h")
        );
        assert_eq!(
            lexical_normalize(&PathBuf::from("a/b/../c.h")),
            PathBuf::from("a/c.h")
        );
        assert_eq!(
            lexical_normalize(&PathBuf::from("../a/../../c.h")),
            PathBuf::from("../../c.h")
        );
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
        // Named by pid alone, so a previous run of this same test can leave the
        // directory behind and `create_dir_all` then races its contents.
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(
            edited.directives.len(),
            2,
            "edit was NOT seen - cache is pinning stale deps"
        );
        assert!(!edited.directives[1].quoted, "angle-bracket form preserved");
        assert_eq!(scan_stats().1 - after.1, 1, "the edit cost one re-parse");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// UPSTREAM FAILURE CLASSES 3 AND 4, reproduced from the shape that
    /// killed `example-nix` on 2026-08-30 rather than from the issue text.
    ///
    /// A header that a CUSTOM_COMMAND generates during the build is
    /// resolvable through `virtual_paths` before it exists, so the BFS
    /// queues it and the scan reads it. It must contribute no directives
    /// instead of failing the task.
    #[test]
    fn a_declared_virtual_header_that_does_not_exist_yet_scans_as_empty() {
        let d = std::env::temp_dir().join(format!("nnvirt{}", std::process::id()));
        // Named by pid alone, so a previous run of this same test can leave the
        // directory behind and `create_dir_all` then races its contents.
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let tu = d.join("nix-channel.cc");
        // Exactly nix's spelling: a quoted include of a .gen.hh that meson
        // writes during the build and declares order-only.
        std::fs::write(&tu, "#include \"unpack-channel.nix.gen.hh\"\n").unwrap();

        let generated = d.join("unpack-channel.nix.gen.hh");
        assert!(
            !generated.exists(),
            "the point of the test is that it is absent"
        );

        let mut virtual_paths = std::collections::HashMap::new();
        virtual_paths.insert(generated.clone(), generated.clone());

        // The includer resolves it and DECLARES it: that half must keep
        // working, or the generated header never reaches the sandbox.
        let includes =
            super::extract_includes(&tu, &tu, std::slice::from_ref(&d), Some(&virtual_paths))
                .unwrap();
        assert!(
            includes.contains(&generated),
            "the generated header must still be declared an input: {includes:?}"
        );

        // Scanning the absent file itself is the read that used to die.
        let nested = super::extract_includes(
            &generated,
            &generated,
            std::slice::from_ref(&d),
            Some(&virtual_paths),
        )
        .unwrap();
        assert!(nested.is_empty(), "a file with no bytes has no directives");

        std::fs::remove_dir_all(&d).ok();
    }

    /// THE NEGATIVE CONTROL, and it is the half that matters: the fix must
    /// not turn every genuinely missing header into a silent empty scan.
    /// A file NOT declared virtual still fails loudly.
    #[test]
    fn a_missing_header_that_was_never_declared_virtual_still_fails() {
        let d = std::env::temp_dir().join(format!("nnvirtneg{}", std::process::id()));
        // Named by pid alone, so a previous run of this same test can leave the
        // directory behind and `create_dir_all` then races its contents.
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let absent = d.join("not-generated-by-anything.h");
        assert!(!absent.exists());

        let empty = std::collections::HashMap::new();
        assert!(
            super::extract_includes(&absent, &absent, std::slice::from_ref(&d), Some(&empty))
                .is_err(),
            "an undeclared missing file must still be an error"
        );
        assert!(
            super::extract_includes(&absent, &absent, std::slice::from_ref(&d), None).is_err(),
            "and with no virtual map at all"
        );

        std::fs::remove_dir_all(&d).ok();
    }
}
