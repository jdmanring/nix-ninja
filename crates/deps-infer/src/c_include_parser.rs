use crate::gcc_include_parser;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
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
    let mut visited = HashSet::new();
    let mut result = Vec::new();
    let mut queue = VecDeque::new();

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
        }
    }

    Ok(result)
}

#[derive(Debug, PartialEq, PartialOrd)]
pub struct SourceWithIncludes {
    pub path: PathBuf,
    pub includes: Vec<PathBuf>,
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

            Ok(SourceWithIncludes { path, includes })
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        let res = handle.join().map_err(|_| anyhow!("Join error"))?;
        results.push(res?);
    }

    Ok(results)
}

static INCLUDE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r##"^\s*#\s*include\s*(["<])([^">]*)[">]"##).unwrap());

/// Given a C-like source, try to resolve includes.
///
/// Includes are generally of the form `#include <name>` or `#include "name"`
/// Is this a path the caller declared as a build-directory file that may not
/// exist yet?
///
/// `virtual_paths` is built as build_path -> build_path, so a queued include
/// can match either side. Both are checked rather than assuming the identity
/// mapping, which is the caller's choice and not this module's contract.
/// The scan over the values runs only on a read failure, which is rare.
fn is_declared_virtual(path: &Path, virtual_paths: Option<&HashMap<PathBuf, PathBuf>>) -> bool {
    let Some(vp) = virtual_paths else {
        return false;
    };
    vp.contains_key(path) || vp.values().any(|v| v == path)
}

pub fn extract_includes(
    path: &PathBuf,
    include_dirs: &[PathBuf],
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
) -> Result<Vec<PathBuf>> {
    // A generated header is resolvable before it exists, and scanning it is
    // what fails. `virtual_paths` resolves a declared-but-absent build
    // directory file so the include is declared; the resolver returns it
    // with no existence check, the search queues it like any other header,
    // and this read then dies with `No such file or directory`, failing the
    // whole task derivation.
    //
    // A file the caller declared virtual contributes no directives instead
    // of an error. That under-declares whatever the generated header itself
    // includes, and the dynamic task covers it by design rather than by
    // luck: discovery runs again inside the sandbox, where the producing
    // edge has run and the bytes exist. The static pass cannot do better,
    // because there is nothing yet to read.
    //
    // Gated on the file being declared virtual, never on the read failing.
    // A source that is simply missing is a real defect and must still fail
    // loudly; swallowing every read error here would turn every genuinely
    // absent header into a silently under-declared task, which is the
    // failure this module exists to prevent.
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            if is_declared_virtual(path, virtual_paths) && !path.exists() {
                return Ok(Vec::new());
            }
            return Err(anyhow!("Failed to open file {}: {}", path.display(), e));
        }
    };
    let reader = BufReader::new(f);
    let mut result = Vec::new();
    let parent_dir = PathBuf::from(path.parent().unwrap());

    let lines = reader.lines();

    for line in lines {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                // Usually this means the file isn't UTF-8 and we can skip.
                return Ok(result);
            }
        };

        if let Some(captures) = INCLUDE_REGEX.captures(&line) {
            let inc_type = captures.get(1).unwrap().as_str();
            let relative_path = PathBuf::from(captures.get(2).unwrap().as_str());

            if inc_type == "\"" {
                if let Some(p) = try_resolve(&parent_dir, &relative_path, virtual_paths) {
                    result.push(p);
                    continue;
                }
            }

            if let Some(p) = include_dirs
                .iter()
                .find_map(|i| try_resolve(i, &relative_path, virtual_paths))
            {
                result.push(p);
            }
        }
    }

    Ok(result)
}

fn try_resolve(
    head: &Path,
    tail: &Path,
    virtual_paths: Option<&HashMap<PathBuf, PathBuf>>,
) -> Option<PathBuf> {
    canonicalize_cached(head.join(tail), virtual_paths).ok()?
}

type PathCache = Arc<RwLock<HashMap<PathBuf, Option<PathBuf>>>>;
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
    // Check virtual paths first if provided
    if let Some(virtual_paths) = virtual_paths {
        for (build_path, actual_path) in virtual_paths {
            if build_path.as_path() == path.as_ref() {
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
mod virtual_path_tests {
    use super::*;

    fn absent() -> PathBuf {
        PathBuf::from("nix-ninja-test-no-such-dir/generated.h")
    }

    fn declaring(path: &Path) -> HashMap<PathBuf, PathBuf> {
        HashMap::from([(path.to_path_buf(), path.to_path_buf())])
    }

    /// The generated-header shape: declared virtual, not on disk. The scan
    /// contributes nothing rather than failing the task.
    #[test]
    fn a_declared_virtual_file_that_is_absent_contributes_nothing() {
        let path = absent();
        let vp = declaring(&path);
        let got = extract_includes(&path, &[], Some(&vp)).expect("must not fail");
        assert!(got.is_empty());
    }

    /// Negative control: absent, and nobody declared it. This must still be
    /// an error, or every missing header becomes a silently under-declared
    /// task.
    #[test]
    fn an_absent_file_nobody_declared_is_still_an_error() {
        let path = absent();
        assert!(extract_includes(&path, &[], None).is_err());
        let unrelated = declaring(Path::new("some/other/file.h"));
        assert!(extract_includes(&path, &[], Some(&unrelated)).is_err());
    }

    /// The map is build_path -> build_path, but the contract does not
    /// promise identity, so a match on either side counts.
    #[test]
    fn either_side_of_the_mapping_counts_as_declared() {
        let key = PathBuf::from("a/gen.h");
        let value = PathBuf::from("b/gen.h");
        let vp = HashMap::from([(key.clone(), value.clone())]);
        assert!(is_declared_virtual(&key, Some(&vp)));
        assert!(is_declared_virtual(&value, Some(&vp)));
        assert!(!is_declared_virtual(Path::new("c/gen.h"), Some(&vp)));
    }

    #[test]
    fn no_map_declares_nothing() {
        assert!(!is_declared_virtual(Path::new("a/gen.h"), None));
    }
}
