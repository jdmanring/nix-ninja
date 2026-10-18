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

/// The driver's own state, which no walk or upload may take: this
/// directory, and the NAR stamp file (`resolve_cache`) at `NIX_BUILD_TOP`,
/// which changes between passes.
pub fn is_state_dir(name: &str) -> bool {
    name == STATE_DIR || name == crate::resolve_cache::NAR_FILE
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConfigureState {
    pub dirs: HashSet<String>,
    pub files: HashSet<String>,
    /// The directories that were empty.
    pub empty_dirs: HashSet<String>,
    /// Where each symlink landed when the walk saw it (`link_landing`), so a
    /// later pass follows the link configure left, not whatever a hook put
    /// at its path since.
    pub links: std::collections::HashMap<String, String>,
}

/// Where the symlink at `path` lands, as the record keeps it: `I:<rel>`
/// inside the build dir (real location `bd`), `O:<path>` outside it, `X`
/// for one that does not resolve.
pub fn link_landing(bd: &Path, path: &Path) -> String {
    match std::fs::canonicalize(path) {
        Ok(t) => match t.strip_prefix(bd) {
            Ok(r) => format!("I:{}", normal_rel(r).unwrap_or_default()),
            Err(_) => format!("O:{}", t.display()),
        },
        Err(_) => "X".to_string(),
    }
}

/// A later pass's record of what configure left, set by the walk
/// (`answer_listings_from`) and asked by every other listing of a build-tree
/// directory (`left_by_configure`). Unset on a first pass, so nothing a
/// first pass emits depends on it.
static LISTED: std::sync::Mutex<Option<ConfigureState>> = std::sync::Mutex::new(None);

/// The task derivations this process wrote, keyed by the outputs of the
/// edge each is for.
static EMITTED: std::sync::Mutex<std::collections::BTreeMap<String, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
/// The tree files each written task read (build paths, interned in
/// `INTERNED`), keyed as `EMITTED` is.
static EMITTED_INPUTS: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u32>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
type Interned = (Vec<String>, std::collections::HashMap<String, u32>);
static INTERNED: std::sync::Mutex<Option<Interned>> = std::sync::Mutex::new(None);
/// What each tree file holds now, hashed once per process, outside `PASS`.
static NOW: std::sync::Mutex<Option<std::collections::HashMap<String, Option<String>>>> =
    std::sync::Mutex::new(None);
/// This pass, decided at startup (`begin_pass`): the state dir, the build
/// dir, and on a later pass the task derivations earlier passes wrote.
static PASS: std::sync::Mutex<Option<Pass>> = std::sync::Mutex::new(None);
#[cfg(test)]
static PASS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone)]
struct Pass {
    state: PathBuf,
    build_dir: PathBuf,
    earlier: Option<std::collections::BTreeMap<String, String>>,
    reused: usize,
    fallback: Vec<String>,
    /// Edges whose recorded derivation was gone: this pass's replaces it.
    replaced: std::collections::HashSet<String>,
    /// For each recorded edge, the tree files it read, as indices into
    /// `files` (path, content hash when an earlier pass wrote the edge).
    edge_inputs: std::collections::HashMap<String, Vec<usize>>,
    files: Vec<(String, String)>,
    /// The edges this pass reused (key, derivation), compared at exit.
    reused_edges: Vec<(String, String)>,
    /// Reused edges that could not be resolved against the tree as it
    /// stands, by derivation.
    unresolved: std::collections::BTreeSet<String>,
}

/// The record's key for an edge: its outputs, joined on NUL, which no path
/// holds (a space would make `a b` one output and two).
fn task_key(outputs: &[PathBuf]) -> Option<String> {
    let key = outputs
        .iter()
        .map(|o| o.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\0");
    (!key.contains(['\n', '\t'])).then_some(key)
}

/// Decides, before anything is read, whether this is a later pass (a record
/// of configure exists) and loads the task derivations earlier passes
/// wrote. AN ERROR on a later pass whose task record is missing or
/// unreadable: deriving every edge again is what reuse exists to prevent.
///
/// ONLY WHERE EARLIER RUNS ARE NOT TRUSTED (`reuse_allowed`: inside a nix
/// build, where passes are the phases of one build of one tree). Outside
/// one, a later run follows an edit to a source, and reusing would place
/// the output from before it.
/// Never on the output-derivation route (configuration C, one pass, its
/// own dynamic arm): the caller does not begin a pass there.
/// `invocation` names what of this run shapes every derivation besides the
/// tree (`-v`, the builder knobs): a later pass run otherwise would reuse
/// derivations it would not have written, and stops instead.
pub fn begin_pass(build_dir: &Path, reuse_allowed: bool, invocation: &str) -> std::io::Result<()> {
    let state = state_dir(build_dir);
    STRICT.store(reuse_allowed, std::sync::atomic::Ordering::Relaxed);
    let later = reuse_allowed && read_lines(&state.join("configure.v1"), build_dir)?.is_some();
    let inv = state.join("invocation.v1");
    if reuse_allowed {
        match read_lines(&inv, build_dir)? {
            Some(first) if later && first != [invocation.to_string()] => {
                return Err(std::io::Error::other(format!(
                    "this pass runs as `{invocation}` and the first ran as `{}`: its \
                     derivations would differ from the ones it reuses",
                    first.join(" ")
                )))
            }
            Some(_) => {}
            None if later => {
                return Err(std::io::Error::other(format!(
                    "a later pass found no record of how the first ran in {}",
                    state.display()
                )))
            }
            None => write_lines(&inv, build_dir, &[invocation.to_string()], false)?,
        }
    }
    let earlier = if later {
        let lines = read_lines(&state.join("emitted.v1"), build_dir)?.ok_or_else(|| {
            std::io::Error::other(format!(
                "a later pass found configure's record but no record of the tasks \
                 earlier passes wrote in {}",
                state.display()
            ))
        })?;
        let mut m = std::collections::BTreeMap::new();
        for l in lines {
            let (k, d) = l.split_once('\t').ok_or_else(|| {
                std::io::Error::other(format!("an unreadable task record line: {l}"))
            })?;
            m.insert(k.to_string(), d.to_string());
        }
        Some(m)
    } else {
        None
    };
    let (files, edge_inputs) = if later {
        load_inputs(&state, build_dir)?
    } else {
        Default::default()
    };
    *PASS.lock().unwrap() = Some(Pass {
        state,
        build_dir: build_dir.to_path_buf(),
        earlier,
        reused: 0,
        fallback: Vec::new(),
        replaced: Default::default(),
        edge_inputs,
        files,
        reused_edges: Vec::new(),
        unresolved: Default::default(),
    });
    Ok(())
}

/// Whether configure's record exists for `build_dir` at all, whatever it
/// was written for: a pass that stops before it decides is a later pass.
pub fn record_present(build_dir: &Path) -> bool {
    std::fs::symlink_metadata(state_dir(build_dir).join("configure.v1")).is_ok()
}

/// Whether an earlier pass recorded a task derivation for the edge with
/// `outputs` (on a later pass).
pub fn recorded(outputs: &[PathBuf]) -> bool {
    let Some(key) = task_key(outputs) else {
        return false;
    };
    PASS.lock()
        .unwrap()
        .as_ref()
        .and_then(|p| p.earlier.as_ref())
        .is_some_and(|e| e.contains_key(&key))
}

/// On a later pass, the task derivation an earlier pass wrote for the edge
/// with `outputs`. `present` says whether it is still in the store; one
/// that is not is counted as a fallback, and the edge is derived again.
pub fn reuse(outputs: &[PathBuf], present: impl Fn(&str) -> bool) -> Option<String> {
    let mut pass = PASS.lock().unwrap();
    let pass = pass.as_mut()?;
    let earlier = pass.earlier.as_ref()?;
    // Outputs no record line can hold are derived on every pass: counted.
    let Some(key) = task_key(outputs) else {
        pass.fallback
            .push(format!("<unrecordable outputs: {outputs:?}>"));
        return None;
    };
    let drv = earlier.get(&key)?.clone();
    if present(&drv) {
        pass.reused += 1;
        pass.reused_edges.push((key, drv.clone()));
        Some(drv)
    } else {
        pass.fallback.push(drv);
        pass.replaced.insert(key);
        None
    }
}

/// Counts a reused edge that could not be resolved against the tree as it
/// stands: the tree changed under it since it was written.
pub fn note_unresolved(drv: &str) {
    if let Some(p) = PASS.lock().unwrap().as_mut() {
        p.unresolved.insert(drv.to_string());
    }
}

/// Notes the task derivation written for an edge with `outputs`, and the
/// tree files (build paths) it read.
pub fn note_task(outputs: &[PathBuf], drv: &str, inputs: Vec<String>) {
    if let Some(key) = task_key(outputs) {
        let ix: Vec<u32> = {
            let mut g = INTERNED.lock().unwrap();
            let (names, index) = g.get_or_insert_with(Default::default);
            inputs
                .into_iter()
                .map(|p| {
                    *index.entry(p.clone()).or_insert_with(|| {
                        names.push(p);
                        (names.len() - 1) as u32
                    })
                })
                .collect()
        };
        EMITTED_INPUTS.lock().unwrap().insert(key.clone(), ix);
        EMITTED.lock().unwrap().insert(key, drv.to_string());
    }
}

/// Placed files the rewind found changed since placement (a hook edited a
/// generated file between phases). Placement may write the reused output
/// over the edit before the pass ends, so the exit comparison cannot see
/// it; every reused edge that read one is counted stale.
static CHANGED_SINCE_PLACEMENT: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

fn note_changed_since_placement(rel: &str) {
    CHANGED_SINCE_PLACEMENT
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(rel.to_string());
}

/// What the tree file or directory at `rel` holds now, hashed once per
/// process.
fn now_hash(build_dir: &Path, rel: &str) -> Option<String> {
    if let Some(h) = NOW.lock().unwrap().as_ref().and_then(|m| m.get(rel)) {
        return h.clone();
    }
    let h = tree_hash(&build_dir.join(rel));
    NOW.lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(rel.to_string(), h.clone());
    h
}

/// A tree file's hash (content and executable bit, as an upload keeps
/// them), or a directory's (every entry below it in order: each file's path
/// and hash, each symlink's path and link text), or None for what is
/// neither or unreadable.
fn tree_hash(path: &Path) -> Option<String> {
    use std::hash::{Hash as _, Hasher as _};
    let file = |p: &Path, h: &mut std::collections::hash_map::DefaultHasher| -> Option<()> {
        std::fs::read(p).ok()?.hash(h);
        (std::fs::metadata(p).ok()?.mode() & 0o111 != 0).hash(h);
        Some(())
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if path.is_file() {
        file(path, &mut h)?;
    } else if path.is_dir() {
        let mut entries: Vec<_> = walkdir::WalkDir::new(path)
            .min_depth(1)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
            .collect();
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        for e in entries {
            e.path().strip_prefix(path).ok()?.hash(&mut h);
            if e.file_type().is_symlink() {
                std::fs::read_link(e.path()).ok()?.hash(&mut h);
            } else {
                file(e.path(), &mut h)?;
            }
        }
    } else {
        return None;
    }
    Some(format!("{:016x}", h.finish()))
}

type InputRecord = (
    Vec<(String, String)>,
    std::collections::HashMap<String, Vec<usize>>,
);

/// The tree files earlier passes' edges read (`files.v1`: path and content
/// hash, one per line, in the order indices name them) and which each edge
/// read (`inputs.v1`: key, then indices).
fn load_inputs(state: &Path, build_dir: &Path) -> std::io::Result<InputRecord> {
    let bad = |l: &str| std::io::Error::other(format!("an unreadable input record line: {l}"));
    let mut files = Vec::new();
    for l in read_lines(&state.join("files.v1"), build_dir)?.unwrap_or_default() {
        let (i, rest) = l.split_once('\t').ok_or_else(|| bad(&l))?;
        let (p, h) = rest.split_once('\t').ok_or_else(|| bad(&l))?;
        if i.parse::<usize>().ok() != Some(files.len()) {
            return Err(bad(&l));
        }
        files.push((p.to_string(), h.to_string()));
    }
    let mut edges = std::collections::HashMap::new();
    for l in read_lines(&state.join("inputs.v1"), build_dir)?.unwrap_or_default() {
        let (k, ix) = l.split_once('\t').ok_or_else(|| bad(&l))?;
        let ix: Vec<usize> = ix
            .split(',')
            .filter(|x| !x.is_empty())
            .map(|x| x.parse().ok().filter(|&i| i < files.len()))
            .collect::<Option<_>>()
            .ok_or_else(|| bad(&l))?;
        edges.insert(k.to_string(), ix);
    }
    Ok((files, edges))
}

/// At exit, success or not. Every pass adds the tasks it derived to the
/// record, an earlier pass's entry kept (first writer wins), under the
/// build dir's lock. A LATER PASS SAYS WHAT IT REUSED AND WHAT IT DERIVED,
/// zero included, so a round reads the line's absence as "not a later
/// pass", never as "nothing to report": `F` is pass-1 work done again (a
/// recorded task missing from the store, or one whose outputs had to be
/// built or substituted). None when this process decided no pass (the
/// compiler route, a subtool).
pub fn pass_report() -> std::io::Result<Option<String>> {
    let Some(pass) = PASS.lock().unwrap().clone() else {
        return Ok(None);
    };
    let emitted = EMITTED.lock().unwrap().clone();
    let path = pass.state.join("emitted.v1");
    let earlier = pass.earlier.clone().unwrap_or_default();
    let fresh: Vec<&String> = emitted
        .iter()
        .filter(|(k, _)| !earlier.contains_key(k.as_str()))
        .map(|(_, d)| d)
        .collect();
    {
        let _lock = lock_blocking(&pass.state).ok_or_else(|| {
            std::io::Error::other(format!("could not lock {}", pass.state.display()))
        })?;
        let mut all = std::collections::BTreeMap::new();
        for l in read_lines(&path, &pass.build_dir)?.unwrap_or_default() {
            let (k, d) = l.split_once('\t').ok_or_else(|| {
                std::io::Error::other(format!("an unreadable task record line: {l}"))
            })?;
            all.insert(k.to_string(), d.to_string());
        }
        for (k, d) in &emitted {
            if pass.replaced.contains(k) {
                all.insert(k.clone(), d.clone());
            } else {
                all.entry(k.clone()).or_insert_with(|| d.clone());
            }
        }
        let lines: Vec<String> = all.iter().map(|(k, d)| format!("{k}\t{d}")).collect();
        // The tree files this pass's new or replaced edges read, appended
        // to those earlier passes recorded (indices never move), each
        // hashed as it is now.
        let (mut files, mut edges) = load_inputs(&pass.state, &pass.build_dir)?;
        // One index per (path, content): an edge a later pass derived
        // after a hook changed a file read the new content.
        let mut index: std::collections::HashMap<(String, String), usize> = files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.clone(), i))
            .collect();
        let names = INTERNED.lock().unwrap().clone().unwrap_or_default().0;
        for (k, inputs) in EMITTED_INPUTS.lock().unwrap().iter() {
            if edges.contains_key(k) && !pass.replaced.contains(k) {
                continue;
            }
            let mut ix = Vec::new();
            for &n in inputs {
                let p = &names[n as usize];
                if p.contains(['\n', '\t']) {
                    continue;
                }
                // As the tree holds it at this pass's exit: hooks run between
                // passes, not during one, and this is the file as the task
                // was given it, whatever the driver rewrote in its upload.
                let Some(h) = now_hash(&pass.build_dir, p) else {
                    continue;
                };
                let next = files.len();
                let i = *index.entry((p.clone(), h.clone())).or_insert(next);
                if i == next {
                    files.push((p.clone(), h));
                }
                ix.push(i);
            }
            edges.insert(k.clone(), ix);
        }
        let file_lines: Vec<String> = files
            .iter()
            .enumerate()
            .map(|(i, (p, h))| format!("{i:010}\t{p}\t{h}"))
            .collect();
        let edge_lines: Vec<String> = edges
            .iter()
            .map(|(k, ix)| {
                let ix: Vec<String> = ix.iter().map(usize::to_string).collect();
                format!("{k}\t{}", ix.join(","))
            })
            .collect();
        write_lines(
            &pass.state.join("files.v1"),
            &pass.build_dir,
            &file_lines,
            false,
        )?;
        write_lines(
            &pass.state.join("inputs.v1"),
            &pass.build_dir,
            &edge_lines,
            false,
        )?;
        write_lines(&path, &pass.build_dir, &lines, false)?;
    }
    if pass.earlier.is_none() {
        return Ok(None);
    }
    let mut pass = pass;
    pass.fallback
        .extend(nix_builder_rpc_client::built_watched());
    // A REUSED EDGE A TREE FILE OF WHICH CHANGED SINCE IT WAS WRITTEN (a
    // hook edited or removed a source or a generated file between phases)
    // is not rebuilt, as reuse is the point; it is counted, so the round
    // sees it. Compared here, at exit, as the record was taken: placement
    // has put this pass's outputs back and the rewind has finished, so
    // both sides see the tree as a pass leaves it.
    let edited = CHANGED_SINCE_PLACEMENT
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    let mut stale: std::collections::BTreeSet<String> = pass
        .reused_edges
        .iter()
        .filter(|(k, _)| {
            pass.edge_inputs.get(k).into_iter().flatten().any(|&i| {
                let (p, then) = &pass.files[i];
                edited.contains(p) || now_hash(&pass.build_dir, p).as_ref() != Some(then)
            })
        })
        .map(|(_, d)| d.clone())
        .collect();
    stale.extend(pass.unresolved.iter().cloned());
    let first3 = |v: Vec<&str>| v.into_iter().take(3).collect::<Vec<_>>().join(" ");
    Ok(Some(format!(
        "nix-ninja: later-pass reused {} task(s), derived {} fresh, {} fallback, {} stale: \
         {}; stale: {}; fresh: {}",
        pass.reused,
        fresh.len(),
        pass.fallback.len(),
        stale.len(),
        first3(pass.fallback.iter().map(String::as_str).collect()),
        first3(stale.iter().map(String::as_str).collect()),
        first3(fresh.iter().map(|d| d.as_str()).collect()),
    )))
}

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
    // skips (`walk_skips`), and nothing outside the build dir, which the
    // listing reads.
    match landing(build_dir, path, r) {
        Landing::Inside(rel) => {
            rel.is_empty() || r.files.contains(&rel) || rel.split('/').any(walk_skips)
        }
        Landing::Outside => true,
        Landing::Absent => false,
    }
}

/// `rel`, a path placement writes, as the walk spells the file the write
/// reaches: its directory resolved as the kernel resolves it now (through a
/// link configure left, or one this pass placed), the name kept, so a
/// placed link is itself and a file written through one is the file it
/// reaches; as spelled where that leaves the tree. None for a spelling
/// that leaves it.
fn written_at(build_dir: &Path, rel: &Path) -> Option<String> {
    let rel = normal_rel(rel)?;
    let reached = || -> Option<String> {
        let bd = std::fs::canonicalize(build_dir).ok()?;
        let p = build_dir.join(&rel);
        let dir = std::fs::canonicalize(p.parent()?).ok()?;
        normal_rel(dir.join(p.file_name()?).strip_prefix(&bd).ok()?)
    };
    // A write that leaves the tree through a link is kept and journaled as
    // spelled: the rewind reaches it through the same link.
    Some(reached().unwrap_or(rel))
}

/// Where a spelling lands, as `landing` answers it.
#[derive(Debug, PartialEq, Eq)]
pub enum Landing {
    /// Under the build dir, as the walk spells it (`""` for the build dir).
    Inside(String),
    /// Outside it: the disk answers, as on the first pass.
    Outside,
    /// Nowhere on the first pass: a `..` after a name configure left no
    /// directory at, or more spelling after a link that did not resolve.
    Absent,
}

/// Where `spelled` (relative to `build_dir`, or absolute) lands on a later
/// pass, ONE ANSWER FOR EVERY PASS: inside the tree it asks only configure's
/// record `r`, never the disk, which hooks and placement change between
/// phases. It climbs as the kernel did when configure left the tree: from
/// the build dir's real location, through every symlink configure left
/// with more of the spelling to follow, to where that link landed then (a
/// `..` after one climbs from its target, and one leaving the tree is
/// followed back in if the spelling returns), and `..` after a name is a
/// climb only where configure left a directory there, since otherwise the
/// first pass found nothing. Outside the tree, and below a directory the
/// walk skips, nothing was recorded and the disk answers.
pub fn landing(build_dir: &Path, spelled: &Path, r: &ConfigureState) -> Landing {
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
        let unrecorded = |rel: &str| rel.split('/').any(walk_skips);
        let disk = match inside(&cur) {
            None => true,
            Some(rel) if rel.is_empty() => continue,
            Some(rel) if unrecorded(&rel) => true,
            Some(rel) => {
                let link = r.links.get(&rel).map(String::as_str);
                hops += usize::from(link.is_some());
                match link {
                    Some(_) if hops > 40 => return Landing::Outside,
                    Some(l) if l.starts_with("I:") => cur = bd.join(&l[2..]),
                    Some(l) if l.starts_with("O:") => cur = PathBuf::from(&l[2..]),
                    // A link that did not resolve when configure left it:
                    // nothing was there on the first pass.
                    Some(_) => return Landing::Absent,
                    None if up && !r.dirs.contains(&rel) => return Landing::Absent,
                    None => {}
                }
                false
            }
        };
        if disk {
            // Nothing recorded here: the disk answers what the kernel would.
            if cur.is_symlink() {
                hops += 1;
                match std::fs::canonicalize(&cur) {
                    Ok(t) if hops <= 40 => cur = t,
                    _ => return Landing::Outside,
                }
            } else if up && !cur.is_dir() {
                return Landing::Absent;
            }
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
    match load_in(&state, build_dir)? {
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
        // As the walk spells the file the write reaches, so `./m.json` and
        // a path through a link are kept as the journal keys them.
        let Some(rel) = written_at(build_dir, rel) else {
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

/// The manifest this process reads (`-f`), which keys every record.
static MANIFEST: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Keys this process's records on `manifest` instead of build.ninja.
pub fn use_manifest(manifest: &Path) {
    let _ = MANIFEST.set(manifest.to_path_buf());
}

/// What identifies one configure: a hash of the CONTENT of the manifest and
/// every file it includes or subninjas (CMake keeps its rules in
/// `CMakeFiles/rules.ninja`, and writes each file only when it changed), so
/// a touch is not a re-configure and a changed rule is. Hashed again only
/// when one of the files' length or mtime moves.
fn header(build_dir: &Path) -> Option<String> {
    use std::hash::{Hash as _, Hasher as _};
    type Stamped = Vec<(PathBuf, u64, i64, i64)>;
    static SEEN: std::sync::Mutex<Option<(Stamped, u64)>> = std::sync::Mutex::new(None);
    let manifest = MANIFEST
        .get()
        .map_or(Path::new("build.ninja"), PathBuf::as_path);
    let stamp_of = |p: &Path| {
        std::fs::metadata(p)
            .ok()
            .map(|m| (p.to_path_buf(), m.len(), m.mtime(), m.mtime_nsec()))
    };
    let mut seen = SEEN.lock().unwrap();
    if let Some((files, h)) = &*seen {
        if files.first().map(|f| &f.0) == Some(&build_dir.join(manifest))
            && files.iter().all(|f| stamp_of(&f.0).as_ref() == Some(f))
        {
            return Some(format!("nix-ninja-configure-state v3 {h:016x}"));
        }
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut files: Stamped = Vec::new();
    let mut queue = vec![build_dir.join(manifest)];
    while let Some(p) = queue.pop() {
        if files.iter().any(|f| f.0 == p) {
            continue;
        }
        let stamped = stamp_of(&p);
        let text = std::fs::read(&p).ok();
        // The top manifest must be readable; an include that is not is
        // hashed as absent.
        if files.is_empty() && (stamped.is_none() || text.is_none()) {
            return None;
        }
        p.hash(&mut hasher);
        text.hash(&mut hasher);
        files.push(stamped.unwrap_or((p.clone(), 0, 0, 0)));
        for line in String::from_utf8_lossy(&text.unwrap_or_default()).lines() {
            if let Some(rest) = line
                .strip_prefix("include ")
                .or(line.strip_prefix("subninja "))
            {
                let rest = rest.trim();
                if !rest.is_empty() && !rest.contains('$') {
                    queue.push(build_dir.join(rest));
                }
            }
        }
    }
    let h = hasher.finish();
    *seen = Some((files, h));
    Some(format!("nix-ninja-configure-state v3 {h:016x}"))
}

/// Inside a nix build (where earlier passes are not trusted, `begin_pass`),
/// a record written for another configure is an error, not a fresh start: a
/// pass that read it as absent would derive against the tree as it stands
/// and overwrite the record later passes reuse. Process-wide, so a read from
/// any thread answers alike; a test that sets it holds `PASS_TEST_LOCK`.
static STRICT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The lines of a record under this configure's header. Ok(None) when there
/// is none; an error when it cannot be read, and (`STRICT`) when it was
/// written for another configure.
fn read_lines(path: &Path, build_dir: &Path) -> std::io::Result<Option<Vec<String>>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(std::io::Error::other(format!("{}: {e}", path.display()))),
    };
    let mut lines = text.lines();
    if lines
        .next()
        .is_some_and(|h| Some(h.to_string()) == header(build_dir))
    {
        return Ok(Some(lines.map(str::to_owned).collect()));
    }
    if STRICT.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(std::io::Error::other(format!(
            "{} was written for another configure: the manifest changed between \
             passes (a re-configure)",
            path.display()
        )));
    }
    Ok(None)
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
        return Err(std::io::Error::other(format!(
            "no manifest in {} to key this configure's record on",
            build_dir.display()
        )));
    };
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut all: Vec<String> = if append {
        read_lines(path, build_dir)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    all.extend(lines.iter().cloned());
    all.sort();
    all.dedup();
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let res = (|| {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        writeln!(f, "{head}")?;
        for l in &all {
            writeln!(f, "{l}")?;
        }
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
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
pub fn load(build_dir: &Path) -> std::io::Result<Option<ConfigureState>> {
    load_in(&state_dir(build_dir), build_dir)
}

fn load_in(state: &Path, build_dir: &Path) -> std::io::Result<Option<ConfigureState>> {
    let mut s = ConfigureState::default();
    let mut malformed = 0usize;
    let Some(lines) = read_lines(&state.join("configure.v1"), build_dir)? else {
        return Ok(None);
    };
    for line in lines {
        match line.split_once(' ') {
            Some(("D", rel)) => s.dirs.insert(rel.to_string()),
            Some(("F", rel)) => s.files.insert(rel.to_string()),
            Some(("E", rel)) => s.empty_dirs.insert(rel.to_string()),
            Some(("L", rest)) => match rest.split_once('\t') {
                Some((l, t)) => s.links.insert(l.to_string(), t.to_string()).is_none(),
                None => {
                    malformed += 1;
                    false
                }
            },
            _ => {
                malformed += 1;
                false
            }
        };
    }
    if malformed > 0 {
        // A record missing lines would answer a later pass for a tree pass 1
        // never saw.
        return Err(std::io::Error::other(format!(
            "{malformed} unreadable line(s) in the configure record at {}",
            state.display()
        )));
    }
    Ok(Some(s))
}

/// Writes `s` as this configure's record. AN ERROR when it cannot: without
/// a record a later pass would read the directory as it stands, placed
/// files and all.
pub fn save(build_dir: &Path, s: &ConfigureState) -> std::io::Result<()> {
    save_in(&state_dir(build_dir), build_dir, s)
}

fn save_in(state: &Path, build_dir: &Path, s: &ConfigureState) -> std::io::Result<()> {
    // A name holding a newline cannot be one line; it is left out, so a
    // later pass treats it as something configure did not leave.
    let lines: Vec<String> = s
        .dirs
        .iter()
        .map(|d| format!("D {d}"))
        .chain(s.files.iter().map(|f| format!("F {f}")))
        .chain(s.empty_dirs.iter().map(|e| format!("E {e}")))
        .filter(|l| !l.contains('\n'))
        .chain(
            s.links
                .iter()
                .filter(|(l, t)| !l.contains(['\n', '\t']) && !t.contains('\n'))
                .map(|(l, t)| format!("L {l}\t{t}")),
        )
        .collect();
    write_lines(&state.join("configure.v1"), build_dir, &lines, false).map_err(|e| {
        std::io::Error::other(format!(
            "could not record the configure-time build directory ({e}); a later \
             pass would read the directory as it stands"
        ))
    })?;
    // A new record is a new configure: what an earlier one left at a path
    // placement replaced is not this configure's. Not while a pass holds
    // files or directories aside: its restore asks these copies what gives
    // way.
    let orig = originals(state);
    if ["aside", "removed-dirs"]
        .iter()
        .any(|n| std::fs::symlink_metadata(state.join(n)).is_ok())
    {
        return Ok(());
    }
    match std::fs::remove_dir_all(&orig) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(std::io::Error::other(format!(
            "could not clear {} ({e}); a later pass would read an earlier \
             configure's file where placement replaced one",
            orig.display()
        ))),
        _ => Ok(()),
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
        return Err(std::io::Error::other(format!(
            "could not lock {} to journal this run's placement",
            state.display()
        )));
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
    let Some(r) = load_in(state, build_dir)? else {
        return Ok(());
    };
    let path = state.join("placed.v1");
    // Keyed by kind and path, so a later pass's placement replaces the stamp
    // an earlier one recorded.
    let mut entries: std::collections::BTreeMap<(char, String), String> =
        read_lines(&path, build_dir)?
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
        let Some(key) = written_at(build_dir, rel) else {
            continue;
        };
        // A FILE CONFIGURE LEFT, WRITTEN WITHOUT ITS COPY KEPT: placement
        // reached it through a link this pass placed, which did not exist
        // when the copies were taken. A later pass could not put it back.
        if r.files.contains(&key)
            && std::fs::symlink_metadata(originals(state).join(&key)).is_err()
            && std::fs::symlink_metadata(build_dir.join(&key)).is_ok_and(|m| !m.is_dir())
        {
            return Err(std::io::Error::other(format!(
                "placement wrote over configure's {key} through {} without keeping \
                 configure's copy; a later pass could not read it",
                rel.display()
            )));
        }
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
        let lock = lock.map_err(|e| {
            std::io::Error::other(format!("no rewind state at {} ({e})", state.display()))
        })?;
        // ANOTHER DRIVER IN THIS BUILD DIR is waited for, never read past:
        // reading the directory as it stands would read its placements.
        if lock.try_lock().is_err() {
            eprintln!(
                "nix-ninja: another driver holds this build directory's rewind; waiting for it"
            );
            lock.lock()?;
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
        let Some(journal) = read_lines(&journal_path, build_dir)? else {
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
                        note_changed_since_placement(rel);
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
                    note_changed_since_placement(rel);
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
                if let Err(err) = std::fs::create_dir_all(self.build_dir.join(rel)) {
                    failed.get_or_insert((rel.to_path_buf(), err));
                    continue;
                }
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
            links: [("alias".to_string(), "I:CMakeFiles".to_string())].into(),
        };
        assert_eq!(
            load_in(&state, &d).unwrap(),
            None,
            "no record before the first pass"
        );
        save_in(&state, &d, &s).expect("saved");
        assert_eq!(load_in(&state, &d).unwrap(), Some(s.clone()));
        // Rewritten with the same content (a configure re-run, a touch): the
        // same configure.
        write(&d.join("build.ninja"), "default all\n");
        assert!(
            load_in(&state, &d).unwrap().is_some(),
            "a touch is not a re-configure"
        );
        // Every read of a record written for another configure below holds
        // the lock a test that sets STRICT also holds.
        let _g = super::PASS_TEST_LOCK.lock().unwrap();
        // An included file's content is the configure's too.
        write(&d.join("build.ninja"), "include rules.ninja\ndefault all\n");
        write(&d.join("rules.ninja"), "rule cc\n  command = cc\n");
        save_in(&state, &d, &s).expect("saved");
        write(&d.join("rules.ninja"), "rule cc\n  command = cc\n");
        assert!(load_in(&state, &d).unwrap().is_some(), "an include touched");
        write(&d.join("rules.ninja"), "rule cc\n  command = cc -O2\n");
        assert!(load_in(&state, &d).unwrap().is_none(), "an include changed");
        write(&d.join("build.ninja"), "default all app\n");
        assert_eq!(
            load_in(&state, &d).unwrap(),
            None,
            "outside a build, a changed build.ninja is a new configure"
        );
        // Inside one it stops the pass: the record must not be read as absent.
        super::STRICT.store(true, std::sync::atomic::Ordering::Relaxed);
        let strict = load_in(&state, &d);
        super::STRICT.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(strict.is_err(), "a re-configure between passes is an error");
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
                ..Default::default()
            },
        )
        .expect("saved");
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
        // A second driver in the same build dir waits for the rewind, never
        // reading the directory past it.
        let (tx, rx) = std::sync::mpsc::channel();
        let (s2, b2) = (state.clone(), bd.clone());
        let second = std::thread::spawn(move || {
            let r = Rewind::begin_in(&s2, &b2, true).map(|r| r.is_none());
            tx.send(()).unwrap();
            r
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "a second driver waits"
        );
        assert!(
            !bd.join("include/Foo/a.h").exists(),
            "and puts nothing back"
        );
        // The pass places one output again; the rest come back at the end.
        write(&bd.join("gen/sub/y.h"), "new");
        rewind.finish().expect("everything comes back");
        assert!(second.join().unwrap().unwrap(), "and goes on once it ends");
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
            ..Default::default()
        };
        save_in(&state, &bd, &record).expect("saved");
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
        let mut r = record_of(&files);
        r.links
            .insert("python".into(), "O:/elsewhere/python".into());
        assert!(super::left_by(Some(&r), bd, Path::new("python/pkg/a.py")));
        assert!(super::left_by(
            Some(&r),
            bd,
            Path::new("meson-private/x.dat")
        ));
        assert!(!super::left_by(Some(&r), bd, Path::new("pythonx/a.py")));
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
        let mut r = record_of(&files);
        r.links.insert("alias".into(), "I:real".into());
        assert!(super::left_by(Some(&r), &bd, Path::new("alias/a.h")));
        assert!(!super::left_by(Some(&r), &bd, Path::new("alias/w.tmp")));
        // A hook replaces the link with a directory holding the same name:
        // the record's link still answers, as pass 1 read it.
        std::fs::remove_file(bd.join("alias")).unwrap();
        write(&bd.join("alias/a.h"), "hook");
        assert!(super::left_by(Some(&r), &bd, Path::new("alias/a.h")));
        assert!(!super::left_by(Some(&r), &bd, Path::new("alias/w.tmp")));
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
        save_in(&state, &bd, &record).expect("saved");
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
        save_in(&state, &bd, &record).expect("saved");
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
        save_in(&state, &bd, &record).expect("saved");
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

    /// PLACEMENT THROUGH A LINK CONFIGURE LEFT writes the file the link
    /// reaches, so that file's copy is kept and its line journaled: through
    /// a chain of links and through a link with an absolute target alike,
    /// pass 2 reads configure's version where pass 1 did.
    #[test]
    fn placement_through_a_configure_link_keeps_the_file_it_reaches() {
        for (tag, links) in [
            (
                "chain",
                vec![
                    ("alias2", "real".to_string()),
                    ("alias", "alias2".to_string()),
                ],
            ),
            ("absolute", vec![("alias", String::new())]),
        ] {
            let d = scratch(&format!("through-{tag}"));
            let bd = d.join("build");
            let state = d.join("state");
            write(&bd.join("build.ninja"), "default all\n");
            write(&bd.join("real/x.h"), "configure");
            for (l, t) in &links {
                let t = if t.is_empty() {
                    bd.join("real").to_string_lossy().into_owned()
                } else {
                    t.clone()
                };
                std::os::unix::fs::symlink(t, bd.join(l)).unwrap();
            }
            let record = crate::task::plan_build_dir_walk(&bd, None, |_| false)
                .unwrap()
                .seen;
            save_in(&state, &bd, &record).expect("saved");
            let placed = [PathBuf::from("alias/x.h")];
            keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path))
                .expect("kept");
            write(&bd.join("alias/x.h"), "placed");
            journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
            let rewind = begin(&state, &bd);
            assert_eq!(
                std::fs::read_to_string(bd.join("real/x.h")).unwrap(),
                "configure",
                "{tag}: pass 2 reads configure's version"
            );
            rewind.finish().expect("everything comes back");
            assert_eq!(
                std::fs::read_to_string(bd.join("real/x.h")).unwrap(),
                "placed"
            );
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// NOTHING TO KEY A RECORD ON, OR A RECORD MISSING LINES, STOPS THE RUN:
    /// with no build.ninja every pass would be a first pass, and a record
    /// that dropped lines would answer for a tree pass 1 never saw. A state
    /// dir that cannot be opened stops a later pass the same way.
    #[test]
    fn a_record_that_cannot_answer_for_pass_one_stops_the_run() {
        let d = scratch("record-unkeyed");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("other.ninja"), "default all\n");
        assert!(save_in(&state, &bd, &ConfigureState::default()).is_err());
        write(&bd.join("build.ninja"), "default all\n");
        save_in(&state, &bd, &ConfigureState::default()).expect("saved");
        let rec = std::fs::read_to_string(state.join("configure.v1")).unwrap();
        std::fs::write(state.join("configure.v1"), format!("{rec}Q what\n")).unwrap();
        assert!(load_in(&state, &bd).is_err());
        write(&d.join("notadir"), "a file");
        assert!(Rewind::begin_in(&d.join("notadir/state"), &bd, false).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A LINK THIS PASS PLACED, written through: the journal keys the file
    /// the write reached, so pass 2 sets aside both the link and that file,
    /// not a path below a link it has already moved.
    #[test]
    fn a_file_placed_through_a_placed_link_is_set_aside_with_it() {
        let d = scratch("placed-link");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        std::fs::create_dir_all(bd.join("lib64")).unwrap();
        let record = crate::task::plan_build_dir_walk(&bd, None, |_| false)
            .unwrap()
            .seen;
        save_in(&state, &bd, &record).expect("saved");
        std::os::unix::fs::symlink("lib64", bd.join("lib")).unwrap();
        write(&bd.join("lib/foo.so"), "placed");
        let placed = [PathBuf::from("lib"), PathBuf::from("lib/foo.so")];
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        let rewind = begin(&state, &bd);
        assert!(!bd.join("lib64/foo.so").exists(), "the file is set aside");
        assert!(!bd.join("lib").is_symlink(), "and so is the link");
        rewind.finish().expect("everything comes back");
        assert_eq!(
            std::fs::read_to_string(bd.join("lib/foo.so")).unwrap(),
            "placed"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A LATER PASS REUSES WHAT EARLIER PASSES WROTE. Pass 1 records its
    /// tasks and prints nothing; pass 2 reuses them (a recorded task gone
    /// from the store is a fallback, derived again) and records the edge it
    /// derived fresh; pass 3 reuses that edge too. A later pass whose task
    /// record is missing stops rather than deriving everything again.
    #[test]
    fn later_passes_reuse_what_earlier_passes_wrote() {
        let _g = super::PASS_TEST_LOCK.lock().unwrap();
        let d = scratch("reuse");
        let bd = d.join("build");
        write(&bd.join("build.ninja"), "default all\n");
        let state = super::state_dir(&bd);
        let _ = std::fs::remove_dir_all(&state);
        let out = |n: &str| vec![PathBuf::from(n)];
        let reset = || {
            super::EMITTED.lock().unwrap().clear();
            super::EMITTED_INPUTS.lock().unwrap().clear();
            *super::NOW.lock().unwrap() = None;
        };
        write(&bd.join("src/a.c"), "int a;");
        assert_ne!(
            super::task_key(&out("a b")),
            super::task_key(&[PathBuf::from("a"), PathBuf::from("b")]),
            "one output with a space is not two outputs"
        );
        // Pass 1: no record of configure yet.
        reset();
        super::begin_pass(&bd, true, "v=false").unwrap();
        assert_eq!(
            super::reuse(&out("a.o"), |_| true),
            None,
            "nothing to reuse"
        );
        super::note_task(&out("a.o"), "/nix/store/aaa-a.drv", vec!["src/a.c".into()]);
        super::note_task(&out("b.o"), "/nix/store/bbb-b.drv", Vec::new());
        assert_eq!(super::pass_report().unwrap(), None, "pass 1 says nothing");
        save_in(&state, &bd, &ConfigureState::default()).expect("saved");
        // Pass 2, after a hook edited a source a reused edge read.
        write(&bd.join("src/a.c"), "int a = 1;");
        reset();
        super::begin_pass(&bd, true, "v=false").unwrap();
        assert_eq!(
            super::reuse(&out("a.o"), |_| true).as_deref(),
            Some("/nix/store/aaa-a.drv")
        );
        assert_eq!(
            super::reuse(&out("b.o"), |_| false),
            None,
            "gone: derived again"
        );
        super::note_task(&out("b.o"), "/nix/store/bb2-b.drv", Vec::new());
        super::note_task(&out("check"), "/nix/store/ccc-check.drv", Vec::new());
        // A second writer of an edge already recorded does not replace it.
        super::note_task(&out("a.o"), "/nix/store/zzz-a.drv", Vec::new());
        let line = super::pass_report().unwrap().expect("a later pass reports");
        assert!(
            line.starts_with(
                "nix-ninja: later-pass reused 1 task(s), derived 1 fresh, 1 fallback, 1 stale: "
            ),
            "{line}"
        );
        // Pass 3 reuses pass 2's fresh edge, and the first pass's record of
        // an edge stands (the first writer wins).
        reset();
        super::begin_pass(&bd, true, "v=false").unwrap();
        assert!(super::reuse(&out("check"), |_| true).is_some());
        assert_eq!(
            super::reuse(&out("b.o"), |_| true).as_deref(),
            Some("/nix/store/bb2-b.drv"),
            "a fallback's replacement is what later passes reuse"
        );
        assert_eq!(
            super::reuse(&out("a.o"), |_| true).as_deref(),
            Some("/nix/store/aaa-a.drv")
        );
        let line = super::pass_report().unwrap().unwrap();
        assert!(
            line.contains("reused 3 task(s), derived 0 fresh, 0 fallback, 1 stale"),
            "{line}"
        );
        // Untrusted earlier runs only: outside a build nothing is reused.
        reset();
        super::begin_pass(&bd, false, "v=false").unwrap();
        assert_eq!(super::reuse(&out("a.o"), |_| true), None);
        assert_eq!(super::pass_report().unwrap(), None);
        // A later pass run another way (`-v`) stops.
        assert!(super::begin_pass(&bd, true, "v=true").is_err());
        // A later pass with no task record stops.
        std::fs::remove_file(state.join("emitted.v1")).unwrap();
        assert!(super::begin_pass(&bd, true, "v=false").is_err());
        *super::PASS.lock().unwrap() = None;
        super::STRICT.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// STALE IS COUNTED AGAINST WHAT EACH EDGE READ, at exit, as the record
    /// was taken: a source a hook changed or removed counts, and so does a
    /// file inside a directory input; an edge a later pass derived after
    /// the change read the new content and does not count on the pass
    /// after; a placed output placed again does not count, but one the
    /// rewind found edited since placement does, though placement wrote the
    /// reused output back over the edit.
    #[test]
    fn stale_counts_what_each_edge_read() {
        let _g = super::PASS_TEST_LOCK.lock().unwrap();
        let d = scratch("stale");
        let bd = d.join("build");
        write(&bd.join("build.ninja"), "default all\n");
        let state = super::state_dir(&bd);
        let _ = std::fs::remove_dir_all(&state);
        let out = |n: &str| vec![PathBuf::from(n)];
        let reset = || {
            super::EMITTED.lock().unwrap().clear();
            super::EMITTED_INPUTS.lock().unwrap().clear();
            *super::NOW.lock().unwrap() = None;
            *super::CHANGED_SINCE_PLACEMENT.lock().unwrap() = None;
        };
        write(&bd.join("h.h"), "v1");
        write(&bd.join("data/x.txt"), "one");
        write(&bd.join("conf.h"), "placed over configure's");
        write(&bd.join("gone.h"), "source");
        write(&bd.join("gen.h"), "a pass-1 output");
        reset();
        super::begin_pass(&bd, true, "v").unwrap();
        let edges = [
            ("a.o", "aaa", "h.h"),
            ("d.o", "ddd", "data"),
            ("e.o", "eee", "conf.h"),
            ("f.o", "fff", "gone.h"),
            ("g.o", "ggg", "gen.h"),
        ];
        for (o, d, input) in edges {
            super::note_task(
                &out(o),
                &format!("/nix/store/{d}-x.drv"),
                vec![input.into()],
            );
        }
        super::pass_report().unwrap();
        save_in(&state, &bd, &ConfigureState::default()).expect("saved");
        // Between phases: a hook changes a source and a file in a directory
        // input, removes a source, and edits a generated file.
        write(&bd.join("h.h"), "v2");
        write(&bd.join("data/x.txt"), "two");
        std::fs::remove_file(bd.join("gone.h")).unwrap();
        // Pass 2: the rewind finds gen.h edited since placement; placement
        // writes the reused output back by exit. c.o is derived fresh.
        reset();
        super::begin_pass(&bd, true, "v").unwrap();
        super::note_changed_since_placement("gen.h");
        for (o, _, _) in edges {
            assert!(super::reuse(&out(o), |_| true).is_some());
        }
        super::note_task(&out("c.o"), "/nix/store/ccc-c.drv", vec!["h.h".into()]);
        let line = super::pass_report().unwrap().unwrap();
        assert!(line.contains(", 4 stale: "), "{line}");
        assert!(!line.contains("eee-x.drv"), "{line}");
        // Pass 3, nothing changed since.
        reset();
        super::begin_pass(&bd, true, "v").unwrap();
        for o in ["a.o", "c.o", "d.o", "e.o", "f.o", "g.o"] {
            assert!(super::reuse(&out(o), |_| true).is_some());
        }
        let line = super::pass_report().unwrap().unwrap();
        assert!(line.contains(", 3 stale: "), "{line}");
        assert!(!line.contains("ccc-c.drv"), "{line}");
        *super::PASS.lock().unwrap() = None;
        super::STRICT.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A TREE HASH SEES WHAT AN UPLOAD KEEPS: a script's executable bit,
    /// and a symlink's text inside a directory input.
    #[test]
    fn a_tree_hash_sees_the_mode_and_links() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("treehash");
        write(&d.join("s.sh"), "echo");
        let plain = super::tree_hash(&d.join("s.sh"));
        std::fs::set_permissions(d.join("s.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(super::tree_hash(&d.join("s.sh")), plain, "chmod +x");
        write(&d.join("dir/a"), "a");
        write(&d.join("dir/b"), "b");
        std::os::unix::fs::symlink("a", d.join("dir/l")).unwrap();
        let before = super::tree_hash(&d.join("dir"));
        std::fs::remove_file(d.join("dir/l")).unwrap();
        std::os::unix::fs::symlink("b", d.join("dir/l")).unwrap();
        assert_ne!(
            super::tree_hash(&d.join("dir")),
            before,
            "a link retargeted"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A RECORD THAT CANNOT BE WRITTEN STOPS THE FIRST PASS: without it a
    /// later pass would read the tree as it stands, placed files and all.
    #[test]
    fn a_record_that_cannot_be_written_stops_the_first_pass() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("record-stuck");
        let bd = d.join("build");
        let state = d.join("state");
        write(&bd.join("build.ninja"), "default all\n");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();
        let res = save_in(&state, &bd, &ConfigureState::default());
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(res.is_err());
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
        save_in(&state, &bd, &record).expect("saved");
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
        save_in(&state, &bd, &record).expect("saved");
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
        save_in(&state, &bd, &record).expect("saved");
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
        write(&bd.join("build.ninja"), "default all\n");
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
        save_in(&state, &bd, &ConfigureState::default()).expect("saved");
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
        save_in(&state, &bd, &record).expect("saved");
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
        save_in(&state, &bd, &record).expect("saved");
        let placed = [PathBuf::from("x")];
        keep_originals_in(&state, &bd, &record, placed.iter().map(PathBuf::as_path)).expect("kept");
        std::fs::remove_file(bd.join("x")).unwrap();
        std::fs::create_dir(bd.join("x")).unwrap();
        journal_in(&state, &bd, placed.iter().map(PathBuf::as_path)).expect("journaled");
        let rewind = begin(&state, &bd);
        assert_eq!(std::fs::read_to_string(bd.join("x")).unwrap(), "configure");
        // A record written now (a lost configure.v1) leaves the originals.
        write(&state.join("aside/.keep"), "");
        save_in(&state, &bd, &record).expect("saved");
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
