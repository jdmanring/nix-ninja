use crate::task;
use anyhow::bail;
use anyhow::{anyhow, Result};
use harmonia_store_path::StoreDir;
use n2::densemap::DenseMap;
use n2::graph::{Build, BuildId, FileId, Graph};
use n2::{canon, load, scanner};
use nix_builder_rpc_client::BuilderRpcClient;
use nix_ninja_task::derived_file::DerivedFile;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

pub struct BuildConfig {
    pub build_dir: PathBuf,
    pub store_dir: StoreDir,
    pub is_output_derivation: bool,
}

pub fn build(
    build_filename: &str,
    targets: Vec<String>,
    config: BuildConfig,
    rpc_client: &Arc<BuilderRpcClient>,
) -> Result<Vec<DerivedFile>> {
    // An empty target list is refused HERE rather than producing an empty
    // result: `nix-ninja` with no target used to reach `targets.first()`,
    // find None and report "unimplemented", which named the wrong thing.
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
        },
    )?;
    runner.read_build_dir(&mut loader.graph.files)?;

    let mut scheduler = Scheduler::new(&mut loader.graph, &mut runner);

    // `ninja a b c` is ordinary usage, and the TODO this replaces has been
    // open since the first commit.
    //
    // EVERY NAME IS RESOLVED BEFORE THE SCHEDULER RUNS, so an unknown
    // target names itself immediately instead of surfacing later as a
    // missing derived file for a FileId the reader cannot map back to a
    // name.
    //
    // The multi-target idea and the want_file fix below are
    // @RCoeurjoly's, from #43. This is that PR's CLI boundary rebased,
    // WITHOUT its phony mechanism, which is a separate design question
    // discussed in #5.
    let mut target_fids: Vec<(String, FileId)> = Vec::with_capacity(targets.len());
    for name in &targets {
        let fid = scheduler
            .lookup(name)
            .ok_or_else(|| anyhow!("unknown path requested: {}", name))?;
        // Was `let _ = scheduler.want_file(fid)`. want_file is what detects
        // a dependency CYCLE, so discarding its Result turned the one error
        // it exists to raise into a build that proceeds and fails later
        // somewhere else. @RCoeurjoly's fix, not ours.
        scheduler.want_file(fid)?;
        target_fids.push((name.clone(), fid));
    }
    scheduler.run()?;

    let mut outputs = Vec::with_capacity(target_fids.len());
    for (name, fid) in &target_fids {
        let derived_file = runner.derived_files.get(fid).ok_or(anyhow!(
            "Missing derived file {:?} for target {}",
            fid,
            name
        ))?;
        outputs.push(derived_file.clone());
    }

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
        while self.build_states.unfinished() {
            let mut made_progress = false;
            while let Some(bid) = self.build_states.pop_ready() {
                let build = &self.graph.builds[bid];
                self.build_states.set(bid, BuildState::Running);
                // println!("Writing derivation for {:?} at {:?}", &bid, &build.location);
                self.runner.start(&mut self.graph.files, bid, build)?;
                made_progress = true;
            }

            if made_progress {
                continue;
            }

            let bid = self.runner.wait(&mut self.graph.files)?;
            // println!("Derivation for build {:?} has been written", &bid);
            self.ready_dependents(bid);
        }

        Ok(())
    }
}
