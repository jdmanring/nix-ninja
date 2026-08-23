use crate::task;
use anyhow::bail;
use anyhow::{anyhow, Result};
use harmonia_store_path::StoreDir;
use n2::densemap::DenseMap;
use n2::graph::{Build, BuildId, FileId, Graph};
use n2::{canon, load, scanner};
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::DerivedFile;
use harmonia_store_derivation::derived_path::SingleDerivedPath;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

pub struct BuildConfig {
    pub build_dir: PathBuf,
    pub store_dir: StoreDir,
    pub is_output_derivation: bool,
    pub jobs: usize,
    pub load_limit: f64,
}

pub fn build(
    build_filename: &str,
    targets: Vec<String>,
    config: BuildConfig,
    rpc_client: &Arc<BuilderRpcClient>,
) -> Result<Vec<DerivedFile>> {
    if targets.is_empty() {
        return Err(anyhow!("at least one target is required"));
    }

    let mut loader = load_file(build_filename)?;

    let tools = task::Tools::new(&config.store_dir)?;

    let mut runner = task::Runner::new(
        tools,
        rpc_client.clone(),
        task::RunnerConfig {
            system: "x86_64-linux".to_string(),
            build_dir: config.build_dir,
            store_dir: config.store_dir,
            is_output_derivation: config.is_output_derivation,
            jobs: config.jobs,
            // Ninja's own per-edge concurrency classes. The parser has always
            // produced these and the runner never received them.
            pools: loader.pools.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            load_limit: config.load_limit,
        },
    )?;
    runner.read_build_dir(&mut loader.graph.files)?;

    let mut scheduler = Scheduler::new(&mut loader.graph, &mut runner);

    // Multiple targets, adopted from upstream PR 43 onto this fork's phony
    // model rather than taking that PR's mechanism with it. The two are
    // complementary and this is the half worth having: `ninja a b c` is
    // ordinary usage, and the TODO this replaces had been open since the
    // first commit.
    let mut target_fids: Vec<(String, FileId)> = Vec::with_capacity(targets.len());
    for name in &targets {
        let fid = scheduler
            .lookup(name)
            .ok_or_else(|| anyhow!("unknown path requested: {}", name))?;
        // Was `let _ = scheduler.want_file(fid)`. want_file is what detects a
        // dependency CYCLE, so discarding its Result turned the one error it
        // exists to raise into a build that proceeds and fails later
        // somewhere else.
        scheduler.want_file(fid)?;
        target_fids.push((name.clone(), fid));
    }
    scheduler.run()?;

    // Deduplicated by build_path, because two targets legitimately share
    // outputs - ask for a phony and one of the files it aliases and the file
    // arrives twice - and a duplicate here becomes a duplicate symlink
    // operation at the CLI. Sorted so the output is a function of the target
    // SET rather than of the order it was typed in, which is what makes it
    // comparable between runs.
    let mut outputs: Vec<DerivedFile> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for (name, fid) in target_fids {
        let resolved = runner.resolve_target(fid);
        if resolved.is_empty() {
            return Err(anyhow!(
                "Missing derived file {:?} for target {}",
                fid,
                name
            ));
        }
        for df in resolved {
            if seen.insert(df.build_path.clone()) {
                outputs.push(df);
            }
        }
    }
    // EVERY TASK OUTPUT, not only the requested targets', when asked. The
    // drop-in's install step runs in the OUTER derivation: CMake's
    // cmake_install.cmake copies libraries, plugins, the syncqt include
    // tree and generated .pri files out of the build dir by absolute path,
    // and `ninja install` through this driver cannot do that - a task
    // sandbox has no $out and no view of the other tasks' outputs
    // (qtsvg: "file INSTALL cannot find .../lib/libQt6Svg.so.6.11.1",
    // 2026-08-23). So the build dir has to look built before the installer
    // runs, and `all` alone leaves every intermediate absent. Opt-in by
    // environment because it is what the ninja drop-in wants and not what
    // a plain `nix-ninja <target>` caller expects to find on disk.
    if std::env::var_os("NIX_NINJA_MATERIALIZE_ALL").is_some() {
        let mut extra: Vec<DerivedFile> = runner
            .derived_files
            .values()
            .filter(|df| matches!(df.derived_path, SingleDerivedPath::Built { .. }))
            .filter(|df| seen.insert(df.build_path.clone()))
            .cloned()
            .collect();
        extra.sort();
        eprintln!(
            "nix-ninja: NIX_NINJA_MATERIALIZE_ALL: {} further task output(s) will be linked",
            extra.len()
        );
        outputs.extend(extra);
    }
    outputs.sort();

    Ok(outputs)
}

fn load_file(build_filename: &str) -> Result<load::Loader> {
    let mut loader = load::Loader::new();

    let id = loader
        .graph
        .files
        .id_from_canonical(canon::to_owned_canon_path(build_filename));

    let path = loader.graph.file(id).path().to_path_buf();
    let bytes = match scanner::read_file_with_nul(&path) {
        Ok(b) => b,
        Err(e) => bail!("read {}: {}", path.display(), e),
    };

    loader.parse(path, &bytes)?;

    Ok(loader)
}

/// Build steps go through this sequence of states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BuildState {
    /// Default initial state, for Builds unneeded by the current build.
    Unneeded,
    /// Builds are in the topological sort of the desired targets.
    Want,
    /// Builds whose dependencies are all ready.
    Ready,
    /// Derivation for the build task is being written.
    Running,
    /// Derivation has been written to the Nix store.
    Done,
}

/// BuildStates is a state machine for build targets.
///
/// It tracks the progress of each build and lets the Scheduler know when a
/// build is ready to be started.
struct BuildStates {
    states: DenseMap<BuildId, BuildState>,

    /// Total number of builds that haven't had a derivation generated yet.
    total_pending: usize,

    /// Builds in the ready state, stored redundantly for quick access.
    ready: VecDeque<BuildId>,
}

impl BuildStates {
    fn new(size: BuildId) -> Self {
        BuildStates {
            states: DenseMap::new_sized(size, BuildState::Unneeded),
            total_pending: 0,
            ready: VecDeque::new(),
        }
    }

    fn get(&self, bid: BuildId) -> BuildState {
        self.states[bid]
    }

    fn set(&mut self, bid: BuildId, state: BuildState) {
        let prev = std::mem::replace(&mut self.states[bid], state);

        if prev == BuildState::Unneeded {
            self.total_pending += 1;
        }

        match state {
            BuildState::Ready => {
                self.ready.push_back(bid);
            }
            BuildState::Done => {
                self.total_pending -= 1;
            }
            _ => {}
        }
    }

    fn unfinished(&self) -> bool {
        self.total_pending > 0
    }

    fn want_file(&mut self, graph: &Graph, stack: &mut Vec<FileId>, fid: FileId) -> Result<bool> {
        let file = &graph.files.by_id[fid];

        // Check for a dependency cycle.
        if let Some(cycle) = stack.iter().position(|&sid| sid == fid) {
            let mut err = "dependency cycle: ".to_string();
            for &fid in stack[cycle..].iter() {
                err.push_str(&format!("{} -> ", graph.files.by_id[fid].name));
            }
            err.push_str(&file.name);
            anyhow::bail!(err);
        }

        let mut ready = true;
        if let Some(bid) = file.input {
            stack.push(fid);
            let state = self.want_build(graph, stack, bid)?;
            if state != BuildState::Done {
                ready = false;
            }
            stack.pop();
        }
        Ok(ready)
    }

    fn want_build(
        &mut self,
        graph: &Graph,
        stack: &mut Vec<FileId>,
        bid: BuildId,
    ) -> Result<BuildState> {
        let state = self.get(bid);
        if state != BuildState::Unneeded {
            return Ok(state); // Already visited.
        }

        let build = &graph.builds[bid];
        let mut state = BuildState::Want;

        // Any Build whose inputs are all ready is ready.
        let mut ready = true;
        for &fid in build.ordering_ins() {
            if !self.want_file(graph, stack, fid)? {
                ready = false;
            }
        }
        if ready {
            state = BuildState::Ready;
        }

        self.set(bid, state);

        for &fid in build.validation_ins() {
            let _ = self.want_file(graph, stack, fid)?;
        }

        Ok(state)
    }

    pub fn pop_ready(&mut self) -> Option<BuildId> {
        self.ready.pop_front()
    }
}

/// Topological scheduler of a Ninja build graph.
///
/// Calls out to Runner to start a build task when all its dependencies are
/// ready.
struct Scheduler<'a> {
    graph: &'a mut Graph,
    runner: &'a mut task::Runner,
    build_states: BuildStates,
}

impl<'a> Scheduler<'a> {
    fn new(graph: &'a mut Graph, runner: &'a mut task::Runner) -> Self {
        let build_count = graph.builds.next_id();

        Scheduler {
            graph,
            runner,
            build_states: BuildStates::new(build_count),
        }
    }

    pub fn lookup(&self, name: &str) -> Option<FileId> {
        self.graph.files.lookup(&canon::to_owned_canon_path(name))
    }

    pub fn want_file(&mut self, fid: FileId) -> Result<()> {
        let mut stack = Vec::new();
        self.build_states.want_file(self.graph, &mut stack, fid)?;
        Ok(())
    }

    // Check whether a given build is ready, after one of its inputs was
    // completed.
    fn recheck_ready(&self, build: &Build) -> bool {
        for fid in build.ordering_ins() {
            let file = &self.graph.files.by_id[*fid];
            match file.input {
                None => {
                    // Only generated inputs contribute to readiness.
                    continue;
                }
                Some(bid) => {
                    if self.build_states.get(bid) != BuildState::Done {
                        return false;
                    }
                }
            }
        }
        true
    }

    // Given a build that just finished generating its derivation, check
    // whether its dependent builds are now ready.
    fn ready_dependents(&mut self, bid: BuildId) {
        let build = &self.graph.builds[bid];
        self.build_states.set(bid, BuildState::Done);

        let mut dependents = HashSet::new();
        for &fid in build.outs() {
            for &bid in &self.graph.files.by_id[fid].dependents {
                if self.build_states.get(bid) != BuildState::Want {
                    continue;
                }
                dependents.insert(bid);
            }
        }

        for bid in dependents {
            let build = &self.graph.builds[bid];
            if !self.recheck_ready(build) {
                continue;
            }
            self.build_states.set(bid, BuildState::Ready);
        }
    }

    fn run(&mut self) -> Result<()> {
        // NIX_NINJA_KEEP_GOING=1 is ninja's -k: a failed task abandons its
        // dependents (they stay unready forever) while every independent
        // subtree drains, so ONE run surfaces every distinct failure -
        // built for the qtwebengine campaign, where each run otherwise
        // costs a round trip per undeclared-input convention. The run
        // still exits nonzero, with the failure count.
        let keep_going = std::env::var("NIX_NINJA_KEEP_GOING").as_deref() == Ok("1");
        let mut running = 0usize;
        let mut failed = 0usize;
        while self.build_states.unfinished() {
            let mut made_progress = false;
            while let Some(bid) = self.build_states.pop_ready() {
                let build = &self.graph.builds[bid];
                self.build_states.set(bid, BuildState::Running);
                self.runner.start(&mut self.graph.files, bid, build)?;
                running += 1;
                made_progress = true;
            }

            if made_progress {
                continue;
            }

            // Nothing ready and nothing running: every remaining Want is
            // downstream of a failure. Only reachable with failures
            // recorded, since success always readies dependents.
            if running == 0 {
                break;
            }

            let (bid, ok) = self.runner.wait(&mut self.graph.files)?;
            running -= 1;
            if ok {
                self.ready_dependents(bid);
            } else {
                failed += 1;
                if !keep_going {
                    anyhow::bail!("a task failed (set NIX_NINJA_KEEP_GOING=1 to collect all failures in one run)");
                }
            }
        }

        if failed > 0 {
            anyhow::bail!("{failed} task(s) failed under keep-going; each is reported above");
        }
        Ok(())
    }
}
