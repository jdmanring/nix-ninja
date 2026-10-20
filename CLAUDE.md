Guidance for Claude Code in this repository. Full records with every
measurement and retraction: `local/incidents/2026-09-04-claude-md-full-record.md`,
`local/incidents/2026-09-17-claude-md-full-record.md`,
`local/incidents/2026-10-16-claude-md-full-record.md` (the file this one
replaced); read them before changing any status here.

Reference, read on demand and never imported (it is 65 KB and the
instruction budget is 120 KB across all files): `local/claude-md-detail.md`
holds the error signatures (log line to status row), gate controls and
instruments, counters and variables, verification revisions and consumer
references. Open it when a round fails or a gate needs its control.

## What this is

`nix-ninja` emits one Nix derivation per translation unit: a killed build
costs one object, a binary cache substitutes objects, editing one `.cpp`
rebuilds one TU. This tree continues the dormant `pdtpartners/nix-ninja`
and **is the primary project**. The consumer is Saxum, a distribution built on one
workstation, checkout `~/Projects/saxum` (lowercase; the capitalized
spelling broke things), editions `.#saxum-kde`, `.#saxum-xfce`,
`.#saxum-server`.

Never open PRs or issues against `upstream`; `local/upstream/` is history
and drafting material, not a queue.

## Remotes and branch hygiene

    origin     git@github.com:jdmanring/nix-ninja.git     THE PROJECT; push freely
    upstream   https://github.com/pdtpartners/nix-ninja   FETCH ONLY, dormant

Default branch `main`, tracking `origin/main` (never `upstream/main`). Commits carry no AI attribution; James is sole author. Never
`git add -A` on a PR branch: `/local/` is only ignored on `main`.
`.git/info/exclude` backstops `local/`, `CLAUDE.md`, `.graphifyignore`,
`scripts/` on every branch. `local/` is its own git repository
(`rev-parse --show-toplevel` inside a gate script answers about it); its
`origin` is the private `jdmanring/nix-ninja-local` (Actions off): push
freely with `--no-verify` (James's decision 2026-10-11).
`CLAUDE.md` is TRACKED on `main` and published with it; the `/CLAUDE.md`
line in `.git/info/exclude` governs untracked files only and has no effect
on it. It is also mirrored to `local/instructions/CLAUDE.md`, which
`claude-md-citations.sh` compares byte for byte: refresh the mirror in the
same change as any edit here.

## THE ONE THING TO UNDERSTAND: what an edit costs

Saxum banks hundreds of thousands of per-TU outputs. Two independent tests
decide whether an edit spends them; passing the first is not enough.

1. **Fileset test.** `nix-ninja-task`'s `src` in `modules/flake/overlays.nix`
   is an allowlist: `Cargo.{toml,lock}`, `crates/nix-{libstore,ninja-task}`,
   `crates/deps-infer`, `vendor-n2`. An edit inside it (tests included) moves
   the task binary, builder of every PLAIN task derivation, re-keying every
   banked compile. The driver (`crates/nix-ninja`) is outside it, but
   `build_dynamic_task_derivation` makes it the builder of every DYNAMIC
   task derivation, embedding the plain one's JSON: a task-binary move
   re-keys both classes, a driver move the dynamic ones. A local drive (A)
   never keys on the driver.
2. **Emission test.** A driver-only edit is free only if it does not change
   what the driver EMITS: `drv.inputs`, the emitted `$PATH`, `passAsFile`,
   the forwarded environment, the discovered input set. Any of those
   re-keys every emitted derivation whatever the fileset says.

**Price by `drvPath`, from this directory, never by reasoning:**

    nix eval --impure --max-jobs 0 --raw \
      .#packages.x86_64-linux.nix-ninja-task.drvPath

`local/gates/emission-price.sh [BASE_REV]` asks both questions against a
revision (A): the task `drvPath` plus the emitted derivation for five shapes
(compile, `deps = gcc` compile, link, generator, app). Three control arms
(sensitivity, determinism, fileset-ctl) run before any price prints. Limits:
FREE on five shapes says nothing about shapes none carries (swig, msgfmt,
gir, nasm: there evidence is unit tests plus real-edge runs); drivers run
under `env -i`, so a newly forwarded variable needs
`local/gates/forwarded-env-price.sh`; a tool the host resolves outside the
store (`which pkg-config` = `/usr/bin`) reads FREE though inside a build it
re-keys every task; a sandbox-gated effect needs
`local/gates/emission-price-b.sh [BASE_REV]` (recursive-nix, both sides at
one build path); unknobbed pricing reads every task-binary move as GLOBAL
RE-KEY, so price under the knob with `emission-price-knob.sh [BASE_REV]`
(unknobbed control and an ABI change must move, then the subject decides).
Price against the CURRENT tree's base, never a remembered HEAD, never
through Saxum. `git add` new files first (untracked files are invisible to
the eval and break every `example-*` attribute). Never run a pricer or the
corpus beside other nix work. The implicit-input blanket is gated
`!is_gcc_task`, so a change confined to it never moves a compile. Never put
an unpriced cost clause in a commit body.

**A pin bump pays two bills; only one is large.** Package derivations
re-derive (`nnCcHook` embeds the driver path as CONTENT;
`ccWireAlways`/`withNixNinjaAttrs` add both binaries as inputs). The stable
mount `/nn-driver` would remove both carriers (driver allows dropping the
inputs since `779a742`), but landing it changes hook content and re-keys
every cc-route package including the banked server, and James's order is
that the server is NOT rebuilt (`local/pending/stable-cc-hook-proposal.md`).
Its toolchain half (`nnWireToolchain`) is WITHDRAWN by measurement (5 to
1594 refusals); do not re-propose it. Dynamic task derivations key on the
DRIVER; plain ones on the task binary and EMITTED BYTES, so an
emission-free driver bump costs a re-derivation whose expensive half
substitutes (0.11% dynamic, by BUILDER, not name). `nix derivation show` on
these needs `dynamic-derivations`, `ca-derivations` and `recursive-nix`
TOGETHER (missing one exits 1 printing only the refusal).

**Derivation JSON**: entries under `derivations`, keyed by drv path; deps are
`entry["inputs"]["drvs"]`; `entry["inputDrvs"]` is ABSENT and `.get(k, {})`
turns it into a confident "nothing depends on this". Where stdenv sets
`__structuredAttrs`, `env` holds ONLY output paths and build inputs
(`nativeBuildInputs`) live in `structuredAttrs`, so an `env` diff reads
identical while every input differs. A task drv has THREE input lists:
`inputs.drvs` (dynamic inputs), `inputs.srcs` (`Opaque` inserts: compiler
wrapper, pkg-config wrapper), `NIX_NINJA_INPUTS` (discovered); read the one
the code under discussion CONSUMES. `ninja-build` in a name is generic:
identity is the DRV HASH.

**The stable-builder knob.** Both binaries sit in every task derivation as
`builder` and in `drv.inputs` though outputs do not change when they move
(early cutoff works ABOVE an unchanged output; the cost is at the LEAVES).
`NIX_NINJA_TASK_BUILDER`/`NIX_NINJA_DRIVER_BUILDER` take builder paths from
the environment, drop the binaries from the inputs and key on
`NIX_NINJA_TASK_ABI`, mapped by daemon `sandbox-paths`
(`local/pending/stable-task-builder-proposal.md`). Rules:
- UNSET, every emitted derivation is byte for byte unchanged. The ABI
  reaches emission only with a stable builder (`emitted_abi` over
  `abi_in_key`); a non-absolute path is REFUSED; a path without the ABI is
  refused at `builder_path`. The driver prints `task keys name builder`
  once per process on stderr, nothing when unset. The ABI VALUE is the
  consumer hook's (`task_abi()` reads the environment).
- The entries live in `/etc/nix/nix.conf`, verified AGAINST THE DAEMON. The
  rpc client does not forward `--option extra-sandbox-paths` and a client
  option cannot substitute; without a daemon entry every task fails loudly.
- THE MAPPING TRACKS THE PIN, not HEAD: it moves in the SAME change as a
  pin bump and at no other time. A stale mapping fails SILENTLY while the
  old path is resident; BOTH halves move (driver every driver commit, task
  any fileset change). A change altering what a task WRITES moves the ABI
  and pays once; a fix to a previously FAILING task moves nothing.
- THE KNOB PROTECTS ONLY DERIVATIONS CREATED AFTER IT: one carrying
  `builder: /nn-task/bin/nix-ninja-task` and no task path in `srcs` is
  protected; an older one re-keys on a task-binary move. Read the failing
  drv's `builder` and `srcs` before calling a move free. Where `builder` is
  the store path and the ABI unset, the mapping is inert.
- The dynamic class is covered transitively (`example-dynamic-deps`). Local
  mode (A, B, the consumer's route) never builds a dynamic derivation
  (gated on `config.is_output_derivation`); it re-adds the PLAIN one via
  `update_derivation_with_discoveries`. Only `sandbox_adddrv_calls` says a
  dynamic derivation was created (`example-arm-census.sh --build`).

**Reading the mapping: FROM THE PIN, NEVER FROM THIS FILE.** The pin is
`.nodes["nix-ninja"].locked.rev` in the consumer's `flake.lock`. Read
binaries by revision, tracked files only:

    nix eval "git+file:///home/james/Projects/nix-ninja?rev=<full-rev>#packages.x86_64-linux.<attr>.outPath"

NEVER from `builtins.getFlake (toString ./.)` or any path-based eval
(including paths REPORTED to the consumer): the driver's `src` is a GLOB
(`Cargo.lock`, `**/Cargo.toml`, `**/*.rs`) that sweeps in untracked cargo
artifacts, giving a driver hash Saxum never builds; the task's named-dir
allowlist does not. `local/gates/stable-builder-mapping-read.sh` reads the
pin, evaluates its pair by revision and ends `CURRENT` (0), `STALE
task:`/`STALE driver:` (1) or `NO ANSWER` (2); its free half is safe during
a round but cannot see time: re-read before acting. `nix config show`
prints the FILES, not the daemon's loaded config. A daemon restart is proved by
the socket owner/mtime (`ls -l /nix/var/nix/daemon-socket/socket`), the
s6-supervise child (`ps -eo pid,ppid,lstart` on the supervisor's PPID, the
MASTER, whose start must follow the conf's mtime), or the `--daemon` hash
probe (a BUILD with a nonce, so in a gap); never by `ps` on a pid or
children existing. Mapped targets are read at daemon start: a collect of
them makes EVERY sandbox setup fail (`getting attributes of path ...: No
such file or directory`) until repointed AND restarted, so never collect
while the mapping names a path worth keeping. A stub line (any resident
pair) is safe to build under, not correct long-term (gate reads `STALE`);
replacing it is a conf edit plus restart between rounds. Dated readings go
in `local/pending/next-rekey-batch.md`.

**The pin floor.** `local/gates/pin-floor.sh` answers `SAFE` (0), `BAD PIN`
(1: in the recorded-bad window, an ancestor of the floor, or DIVERGENT) or
`NO ANSWER` (2); seconds, safe during a round. Run it before any pin
question and beside a boundary. `34038d2` and `e93c9da` MUST NOT BE PINNED
(junk-tree `-B` reader; ungated in `e93c9da`); the consumer takes `425778a`
or later. The consumer's `scripts/test_upstream_status.py`
`_REV_HISTORICAL` is the complementary hand list; both run. A new
MUST-NOT-PIN revision goes here, in that gate, and in the consumer's table
in the SAME change.

The single global re-key was taken at pin `b00dfaf` (2026-09-29); the task
binary has moved since, which under the knob moves only `/nn-task`. Never
assume the fileset axis is quiet: read the task `drvPath`.

**Batch every re-keying change and pay once**; landing them one at a time
has lost days. Do not garbage collect while a re-key is unproven. Never
start a build without stating its cost and that it has started; never build
without first establishing you are ready. THE WORK LANDS ON `main`; no
batch branch. Record every price, both before landing, in
`local/pending/next-rekey-batch.md` (append-only).

**Gate inputs.** A hard-coded store path in a gate is a NO ANSWER waiting
for the next GC: resolve by revision or attribute, or build the subject.
Sole exception: `round-intercept-sweep.sh`'s literal `-rustc-1.98.1.drv`, a
retained round's leaf log that is NOT DERIVABLE (by attribute you get the
wrapper drv); a non-derivable literal is acceptable ONLY with a loud `NO
ANSWER ... (collected?)`, exit 2. The corpus's latest result is in the
ledger.

## How Saxum consumes this

By revision from GitHub (`github:jdmanring/nix-ninja/<rev>`), never `path:`:
commit, push to `origin`, bump the rev in Saxum's flake. Unpushed work does
not exist to the consumer.

THE PIN PROTOCOL (2026-10-03; every driver bump re-keys the cc route via
`nnCcHook`): (a) the consumer bumps only at a CAMPAIGN BOUNDARY, never
mid-build, except for a fix to a failure inside the running build; (b)
before any hours-long build it asks for the complete in-flight list,
answered with `git log <pin>..origin/main`, `git status` of both trees and
the queue in `local/pending/next-rekey-batch.md`, and everything ready
rides one bump; (c) between boundaries this tree pushes freely; (d) a
mid-build fix arrives with the failing drv and its price (emission vs the
pin, task `drvPath`) before it lands; (e) a failure sent here carries the
failing task's drv and leaf log, never a round summary line; (f) the
mapping moves at the SAME boundary, and the daemon restarts only when it
serves nothing (`nix store ping` answers, no `nixbld` processes, no
orphaned `nix-daemon` with PPID 1): a mid-build restart orphans workers and
`nix-daemon-srv`'s run script then refuses every NEW client until that
build ends. Never argue for a pin bump against the no-global-rebuild order; report
facts. Brief peers proactively and flag state changes at once. (g) A fix for a
refused package goes back only after the consumer's
`scripts/preflight-sweep.sh <kept-tree> <driver-dir> <outdir>` drives every
edge class of its configured tree (outer graph and every nested ninja
graph) with zero refusals and zero `; skipped` lines, counts in the
message, plus a control aimed at each class that actually failed; one
passing edge is not readiness, and a class is called apparatus only after
a control run with the round's own environment (the outer edge's
`cmake -E env`) still fails, never by inspection. Its launch gate
(`refuse-unswept.py`) refuses a relaunch without a clean sweep for the
mapped driver.

Consumer files in `~/Projects/saxum/scripts`: `nn-cc-shim.sh` (cc drop-in,
one-edge build.ninja per TU, `NIX_NINJA_NO_BUILD_DIR_SCAN=1`, `deps = gcc`;
execs the real compiler for `-include`/`-imacros` TUs and an absolute `-o`)
and `nn-ninja-shim.sh` (real ninja for `-t` and `-n`;
`NIX_NINJA_MATERIALIZE_ALL=1` on its `test`, `install` and plain branches;
its `ctest`/`meson test` branches drop the hook's flag variables, an open
consumer-side class). Moving the shims here is James's call; the shim is
hook content, so a comment in it re-keys that route. `nn-cc: drop-in
active` is the HOOK's banner and proves no interception: read `nn-cc: N
intercepted, M passed through` or count `$NIX_BUILD_TOP/.nn-cc/*.ninja`.

`nnOptOut` (`site/base/default.nix`): an opt-out needs a named defect and a
route to removing it, never a fix. `ccPreferred` (reroutes): each entry
carries a `DEFER` naming the driver defect; count it from the file every
time; one `git merge-base --is-ancestor` per entry says whether its fix is
in the pin; a fired trigger still earns one build on B before removal.
Driver defects are fixed HERE: never propose a reroute as a fix (it gives
up resumability). `shared-mime-info` (store-path ITS datadir dropped by the
wiring) and `harfbuzz` (typelib abort, unmeasured) are not cleared by any
pin bump. A rerouted package is MASKED: a green round over it is evidence
about the reroute only.

`withNixNinjaAttrs` appends `[ nixNinja nixNinjaTask nixPkg ]` to every
routed package (cc branch: `pkgs-gaming.nix`), so a failing nix build kills
every routed package and surfaces under an unrelated name: read a
nix-shaped failure first. The nix boost-URL patch wiring and the consumer
resumability gate's reading traps are in the detail file (consumer-side
references).

## Three build configurations

    A  local mode outside any derivation            most historical drives
    B  local mode inside ordinary recursive-nix     THE CONSUMER'S ROUTE
       daemon Recursive: builds allowed, SubmitOutput refused
    C  DRV mode inside builder-rpc-v0               the legacyPackages examples
       daemon RecursiveSubmitted: SubmitOutput allowed, no builds

Per-derivation flag (`requiredSystemFeatures`), so B and C are exclusive.
ninja `dyndep` realises a file before scheduling, so Fortran and C++20
modules FAIL under C (`Operation 46 not allowed`) and WORK under B. A
verification is a claim about its configuration: say which letter or do not
write it down. When a package fails, build its shape on both. An A
reduction cannot reach a mirror-only class (A relativises paths), so an
A-only verification of a B-witnessed class is about the wrong
configuration. Driving a package on A: `local/second-run.sh`;
`generator-edge-execution-b.sh` with `NN_PROBE_FULL=1` builds any package
on B without a pin bump.

## Architecture

- `crates/nix-ninja`, the DRIVER: parses build.ninja via `vendor-n2`;
  `task.rs` turns every edge into a derivation (the emission test is about
  this file). `NIX_NINJA_DRV` selects submit-to-daemon or symlink into the
  build dir (`local.rs`). `dyndep.rs`; `resolve_cache.rs` (memo;
  `persistence_enabled` is `!in_nix_build`). Both binaries resolve off PATH
  only where their store path is keyed (`Tools::new`, `779a742`); with the
  builder knobs set nothing looks on PATH. Read any package shelling out to
  either by BARE NAME before dropping the inputs.
- `nix-ninja -t dynamic-task` (`subtool/dynamic_task.rs`): builder of
  dynamic task derivations; discovers (`discover_dynamic_dependencies`,
  `scan_seeds`) and resubmits. A `deps = gcc` edge in local mode discovers
  here too with `built_paths` (REALISED inputs), not `virtual_paths`.
  `discover_c_includes` has TWO callers (static and dynamic).
- `crates/nix-ninja-task`: builder of every task; materialises inputs
  (`derived_file.rs`, `create_symlinks`), runs, patches rpaths
  (`patchelf.rs`, `assemble_rpath`). `crates/deps-infer`: include scanner
  (`retrieve_c_includes_checked`, `canonicalize_cached`), in the fileset; a
  header reached through a link is declared canonical AND as spelled
  (`extract_includes`), store spellings excepted.
  `crates/nix-builder-rpc-client`: `builder-rpc-v0`.
- `modules/flake/pkgs/mkMesonPackage`, `mkCMakePackage` set
  `NINJA=nix-ninja`, `NIX_NINJA_DRV=true`, `builder-rpc-v0`; Saxum uses
  them, so editing them is emission-side. `packages` holds four examples,
  all `mkRecursivePackage` (B); `legacyPackages` thirteen (C). Spell them
  in full (`.#legacyPackages.x86_64-linux.example-dynamic-deps`); read the
  `attrNames` of both sets, never the directory listing.

Mechanisms, cited by symbol because line numbers rot:
- **Emitted PATH** (every entry gated, every entry an emission change):
  `make` for an LTO link (argv AND the cc's `cc-cflags-before`),
  `gcc-ar`/`gcc-nm`/`gcc-ranlib` from the wrapper's `orig-cc` when named,
  `SCRIPT_TOOLS` (`nm`, `readelf`, `ldd`, text filters; its `DEFER` trigger
  is a round failure naming a missing tool); `as` needs none (`-B`). A `.sh` script an edge
  runs gets the store tools it names in command position
  (`script_bare_programs`), that edge only. The
  compiler wrapper (27 closure paths vs 14) is added only where
  `task_needs_cc`: a toolchain named by basename, `deps = gcc`, all outputs
  object shaped, or a known generator execing cc unnamed
  (`generator_execs_cc_unnamed`). `cc_lookup_is_implied` decides only
  whether an ABSENT compiler is REFUSED (`require_cc`): a bare name or the
  object-shape arm refuses, an absolute toolchain path or non-object output
  does not. A "needs cc" fix belongs in `task_needs_cc`; making
  `names_only_absolute_toolchain` true when nothing is named drops `cc`
  from `dtrace`'s PATH (`the_refusal_gate_answers_by_why_the_compiler_is_needed`
  pins the combination). A refused edge emits NO derivation: read its
  `refused command:` line, never a successful sibling. `command_program`
  steps over assignments, `:`, `&&`, a `cd` pair and `env`; an `if` comes
  back as the program. A bare-name tool an input already ships is found by
  asking the inputs (`input_shipping(store_dir, inputs, rel)`, e.g.
  `bin/ninja`, `bin/pkg-config`), never by store-name matching (it takes
  this driver's own binaries) or widening `SCRIPT_TOOLS` (every task).
- **Implicit-input blanket**: `build_dir_inputs` (from `read_build_dir`)
  swept into every task gated `!is_gcc_task && len <=
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT`; the walk skips produced nodes, state
  files (`build_dir_disposition`) and declared depfiles, so the map is
  stable across passes.
- **Compile vs link**: `is_compile_task` = `deps == gcc` plus
  `object_shaped_outputs`; discovery gates on `-c` on the command line,
  never output name; a `-c` directly after a shell is rejected (`f381c92`).
  An allowlist is safe gating what a build carries, unsafe gating what
  correctness needs. A depfile is adopted on the command's generation flag,
  not `deps = gcc` (`a7a5f4a`).
- **Generated files** hit resolve (`virtual_paths`), scan and upload;
  fixing one leaves the next fatal.
- **Environment**: `forwarded_to_tasks` is an allowlist (`NIX_*`,
  `XML_CATALOG_FILES`, `PYTHONPATH`, `XDG_DATA_DIRS`,
  `NIX_GOBJECT_INTROSPECTION_DEFAULT_FALLBACK_LIBPATH`, `PKG_CONFIG_PATH`),
  plus `command_scoped` keys (`GETTEXTDATADIRS`, `NODEJS_EXECUTABLE`), which
  reach a task whose COMMAND names the consumer or whose INPUTS carry it by
  file name: Chromium's `node.py` is reached through an import closure,
  never named on the command line; values go through `outer_rewrite_map`, whose store paths become inputs;
  forwarding everything bakes `$out` into every key. A task runs no stdenv
  setup hooks. Outer output names come from `outputs` or the
  `__structuredAttrs` file (`outer_output_names`); the self-rpath pair in
  `NIX_LDFLAGS` is forwarded as a name-keyed placeholder. The map rewrites
  only a store path opening its token or following a separator.
- **Readers** for undeclared reads (a file the COMMAND reads that no edge
  declares). Decide driver vs package by whether real ninja succeeds: it
  reads the shared filesystem, the sandbox only DECLARED inputs, so success
  there puts the fix in the DRIVER whatever the CMake rule's `DEPENDS`
  says. A reader that reads a file must also declare it (`-P` script).
  They sit beside `cmake_script_referenced_paths` in the driver; the
  earliest members are `swig_invocation` and `msgfmt_invocation`.
  Enumerate members by shape, never by a count:
  `grep -nE 'let [a-z_]+ = [a-z_]+\(&args\)' crates/nix-ninja/src/task.rs`
  (plus `python_module_invocation` inside the argv loop of
  `build_task_derivation`). Each splits trigger from reader; dispatch is
  caught only by `reader-wiring-drv-repro.sh`. Readers rebase with a
  LEADING `cd` only (`undeclared_outputs` alone tracks a mid-command `cd`)
  and resolve against the directory the command RUNS in. A trigger parsing
  a flag must handle every real spelling: check whether a stripped flag can
  be empty (protoc's `-I .`). Parse to the delimiter, not an enumeration of
  known forms (`import option`), and count the real population before the
  fixture; fixtures use the argv shape production passes. A reader calling
  `upload_referenced_dir` inherits its fatal `read_dir`: filter
  `starts_with(store_dir)`, `!same_project_tree`, `!is_dir()` first.
- **Reader WIRING, three steps; step two is the trap.** (1) a RELATIVE
  candidate is rebased by `rebase_post_cd(&cd_dir, &cand)` (lexical; each
  `..` pops a `cd_dir` component); (2) an ABSOLUTE one is dropped by
  `p.starts_with(store_dir)` and `!same_project_tree(build_dir, p)` (first
  component only); (3) later readers adopt earlier pushes. EMIT RELATIVE
  SPELLINGS (absolute include dirs become relative climbs,
  `include_dirs_as_given`), or the input drops in silence and the fix
  changes nothing. Exception: nasm emits absolute in-tree paths so a `cd`
  prologue cannot rebase twice. A store-path datadir needs the `inputSrcs`
  route. Drop a climb to a STORE dir before it forms
  (`macro_arm_include_dirs`), or the task deepens out of its tree.
- **Macro arms** (`macro_arm_headers`): keep an arm only if it resolves to
  a REGULAR FILE; quoted and angle spellings; a single quoted arm skipped,
  a single angle arm carried; resolved through the command's own include
  dirs, with the unit's own body scanned for defines. `include_dirs_named`
  (drops dirs above the build dir) is for dirs a task CREATES, not RESOLVES.
- **Aliases**: co-outputs expand at resolution (`co_outputs`); a symlink
  output naming a sibling is stored and placed as LINK TEXT
  (`copy_outputs_to_placeholders`, `create_symlinks`); a dangling link in a
  store output is DESIGNED (`alias_link_target`). Soname links from a
  `CMAKE_SYMLINK_LIBRARY` edge ride only a library reached TRANSITIVELY
  (`soname_links`); a directly handed library is unchanged.
- **RPATH**: preserve every entry, empty and `$ORIGIN` included.
- **Uploads** name from the path asked about, never the canonical store
  path (`new_opaque_file`, `new_opaque_file_raw`); rewritten uploads are
  temp copies (`upload_temp_done`). Directory args: python packages under
  `DIR_UPLOAD_CAP`, data trees under `DATA_DIR_CAP`, else a named refusal.
  Python import closures (`upload_python_closure_uncached`): one
  `PYTHON_DEP_DIR_CAP` for every carried dir and `CLOSURE_DIR_CAP` on the
  walk, both REFUSE when exceeded, never skip; a `sys.path` root too large
  to carry resolves only the names imported through it. A reader passes
  over an oversized dir (`dir_exceeds_files`) only where the dir is not
  read (syncqt's `SYNCQT_OUTPUT_DIR_FLAGS`) and only when it would refuse,
  so an edge that resolves today emits what it did. A reader carrying a
  configured tree adds the store paths its files name
  (`store_paths_named_below`, meinproc's customization).
- **Output names** (`normalize_output`) are a LOSSY LABEL
  (`fold_long_output_name` digests over-cap names, others byte for byte);
  `Placeholder::standard_output` hashes the NAME alone; the collision guard
  is at `drv.outputs.insert` in `build_task_derivation`. The daemon's
  realisation refusal is not a name problem nor a driver defect (detail
  file); do not re-propose an injective name or a client in-flight guard.
- **Scope**: `evaluate_and_read_file` fallback chain; `include` leak-back
  is a known gap.
- **Build-dir walk** (`read_build_dir`, `build_dir_disposition`) asks the
  PATH, not the entry; feeds `NIX_NINJA_ALIASES` (configure-time soname
  links) and `NIX_NINJA_MAKE_DIRS` (a compile gets every existing include
  dir its command names via `include_dirs_named`; others the walk's empty
  dirs). `local.rs` places products as symlinks, then
  `refresh_placed_mtimes` makes fresh-mtime copies (aliases stay links);
  depfiles are COPIES; a placed link pulls in its target
  (`link_targets_to_place`). Encoded lists ride `passAsFile`, named in its
  value only where carried (an unconditional name re-keys every task).
- **Placeholders**: restore happens at PLACEMENT (`symlink_derived_files`)
  or the outer-stage copy, never in the task, so a task's store output
  legitimately carries placeholders; audit residue only where a path is
  FINAL. `placeholder_residue` scans 8-char windows (`RESIDUE_WINDOW`),
  inflating `SHF_COMPRESSED` sections (`compressed_sections`); placement
  refuses a file still carrying residue after the whole-path swap and
  nothing re-runs the build (a piece repair was withdrawn: compiler output
  showed a piece's edge matching its neighbour by chance). Scanning an
  artifact by hand, use the same window length on stand-in and control.
  The `out` stand-in is `hjbdq8s5j5dkn2a110sy8zlhfcs4lxsx`.
- **Outer-output staging**: `-I$out/...` and outputs inside the outer
  output are staged under `.nn-outer` by the driver (`outer_stage_outputs`)
  and copied out eagerly via `copy_restoring` (restores, refuses residue,
  keeps permission bits); `new_task`, `normalized_task_outputs` and
  `add_derived_file` must agree. Refused on the output-derivation path.
- **Edit-in-place**: an input also declared as an output is COPIED writable
  (directory: `copy_tree`; file: `mode() | 0o200`, never
  `set_readonly(false)`). ExternalProject trees ride their stamp edges
  (`external_project_extract_dirs`, `external_project_binary_dir`,
  `external_project_step`); a later step's tree SUPERSEDES the earlier one
  (the only non-first-writer-wins registration). The `-B` reader is gated
  at the CALL to step edges whose command names cmake.
- **Placement: link or COPY.** `copy_not_link` is the one list: `.py`,
  `.mjs`, `.js`, `.cjs`, `node_modules`, `header_like` suffixes, a
  `$ORIGIN`-relative build-tree executable (`origin_relative_executable`),
  a file a `cmake -P` `ARCHIVE_CREATE ... PATHS` archives. Aliases are
  asked BEFORE the list; hard links are EXDEV. Shared reason: a consumer
  resolving its own or a sibling's location (or needing a regular file)
  lands in the STORE. Add a member only on finding such a consumer; never
  answer with `LD_LIBRARY_PATH` or a wider PATH.
- **Build-dir deepening** (`main.rs`, `escapes`): the task deepens until
  every climb stays under `/build`. The measure is where a climb LANDS: an
  input that should never be in the tree (host path, store dir) is dropped
  on the INPUT side, or every real input resolves one name too deep.
- **Response files**: `@rsp` positions count from the argv END
  (`respell_rsp_args`), since `rewrite_cmdline` may drop a `cd` prologue.
- **`-v`** writes task output to stdout via an extra declared output
  (CMake's ABI detection parses it).
- **Ninja state** (`ninja_state`): after a local build of the default
  manifest, edges with ALL outputs placed are recorded (`.ninja_log` v7,
  `.ninja_deps` v4), stamped with the run START (`mark_start`); a depfile
  older than the start is not recorded; errors read DIRTY, never clean.
- **Later passes** (`configure_state`; B, where a build runs the driver
  once per phase): a pass after the first sets aside what earlier passes
  placed (`Rewind`, journal `placed.v1`), answers the walk from configure's
  record, and REUSES each edge's recorded task derivation (`emitted.v1`),
  deriving only edges none recorded. Every later-pass exit, error exits
  included, prints `nix-ninja: later-pass reused R task(s), derived M fresh,
  F fallback, S stale: ...` (never pass 1, the compiler route or a subtool);
  the consumer's tripwire stops on F>0 or S>0. S compares each reused edge's
  recorded inputs (`files.v1`, `inputs.v1`) at exit and counts every
  recorded reader of a hook's edit that placement wrote over. A missing,
  corrupt or other-configure record, or another invocation (`-v`, builder
  knobs), is a hard error. Gate `install-pass-reuse-repro.sh`; open gaps in
  the queue (2026-10-18 14:40).
- **Daemon exchanges** are bounded (`bounded`; ladder `300 << 2n` on
  `build_paths`); `BuilderRpcClient::pool` is a `BoundedPool` whose only
  acquire takes a deadline, so a bare acquire is a COMPILE error.

## Failure classes

The status table (every class, fix commits, gate, measurements) is
`local/claude-md-status-table.md`; read the row before reopening,
re-diagnosing or "fixing" a class (log line to row: the detail file's error
signatures). Every entry has a gate arm or a named reason it cannot.
"Verified" names a configuration; "free" means driver only, task `drvPath`
unmoved, five priced shapes identical; "B full" is
`generator-edge-execution-b.sh` full mode. A "fixed" row covers the half its
gate MODELS until a package says otherwise; ancestry says whether a fix is
in the pin, never whether the package builds.

Reading rules: a passing reduction is evidence about the reduction; built
means thousands of log lines, substituted a handful; a round's failure
count counts PACKAGE derivations (a task drv carries no package name); a
failing `try_compile` probe is its answer. Some classes are unreachable on
the compiler route (links are never narrowed; nss reaches both). Fixing
what a rerouted package hits on its NEW route says nothing about the defect
that rerouted it. A route with no working example has no gate. `Cannot
build '<pkg>'` over `N dependencies failed` is not a leaf: count leaves,
read the `Last N log lines` block, read each leaf's own drv before disposing
of it. `builder` in `nix derivation show` names the layer (`nix-ninja-task`
= task; bash/stdenv = the package's own phase).

## Gates

`local/gates/emission-price.sh` is a PRICER: run it before landing any
driver edit and before any pin bump. `local/gates/run-all.sh` is THE corpus
a pin bump must satisfy: builds both binaries once, runs A arms then B arms
inside recursive-nix, one tally; never run A alone and read it as the
tree's state. Arm zero `cargo test --workspace` (re-run single-threaded on
the poisoned-lock signature); arm one `local/gates/registered.sh` (fails an
exclusion naming an invoked script AND a registered pattern its gate no
longer prints: a gate's verdict text and its registration change together).
NO ANSWER is scored apart from FAIL; a runner must not re-decide a gate's
precondition. A discovery-mechanism gate must switch off every other route
(`NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0`). `run_gate` scores verdict TEXT, so
gates must also exit non-zero on failure; the B half FAILS a non-executable
arm. An arm whose control does not FAIL on the defect is excluded, not
registered (`mesa-macro-include-ccshim-repro.sh`); so is a RED-BY-DESIGN
gate.

In run-all: `stable-task-builder-repro.sh`, `logging-contract-repro.sh`,
`shim-intercept-count-repro.sh`, `round-intercept-sweep.sh --self-test`,
`shim-plain-materializes-repro.sh` (the last three read the consumer's
shims; missing shim = NO ANSWER). `example-arm-census.sh --build` (in no
runner) is the only arm asserting `drv_bytes` non-zero: run it beside a pin
bump. Owed: `gcc-reproducibility-probe.sh` (James's). Run
`second-pass-placement-repro.sh` after any change to placement, the walk or
readers. `generator-edge-execution-b.sh` takes `NN_PROBE_PKG=dtc|orc` and
demands `--run`. Indexed only here: `stdout-contract-sweep.sh` (static
stdout sweep), `ninja-flag-surface.sh` (every ninja option accepted or
listed refused), `relative-include-base-repro.sh` (scipy `_zeros.pyx`).
Controls are in the detail file. After editing this file run
`local/gates/claude-md-citations.sh` (cite symbols, not lines); after
editing `docs/ROADMAP.md`, `CHANGELOG.md` or the status table run
`local/gates/roadmap-numbers.sh`.

## Commands

    cargo check -p nix-ninja / -p nix-ninja-task        seconds, warm cache
    cargo test --workspace                              count it, never quote
    cargo test -p nix-ninja --test include_dir_shadow
    cargo clippy --all-targets -- --deny warnings       what the gate applies
    nix develop --command cargo fmt --all               the PINNED rustfmt
    nix flake check                                     clippy, fmt, taplo,
                                                        audit, deny, 10 VM tests
    nix build .#checks.x86_64-linux.nix-ninja-fmt
    local/mutate.sh FILE 'from' 'to' [cargo args]       one mutation, run,
                                                        revert; refuses a
                                                        dirty file
    ./bench/e2e.sh example-dotdot-source-include        stage its record after
    nix build --extra-experimental-features \
      'nix-command flakes dynamic-derivations ca-derivations recursive-nix' \
      .#example-dotdot-source-include

Never bypass `.cargo/config.toml`'s `target-dir`
(`~/.cache/cargo-target/nix-ninja`). Green tests and clippy say nothing
about formatting; a pushed commit is green only once its CI run is read.
Tests live in `crates/nix-ninja` reaching `pub` functions (a test under the
fileset spends every banked compile); fixtures build their own store
objects, never name resident paths; locks over process-global state
(`set_current_dir`, env) live at FILE SCOPE (`CHDIR_LOCK`, `OUT_ENV_LOCK`);
a control reads a different source than its subject. Pin the WIRING:
mutation-check that reverting the guard fails a test that runs it; a
flag-pair predicate's negative control carries the pair. Export a predicate
rather than writing it twice; prefer a compiler-enforced type over a
source-counting test. No compatibility wrappers. Commit before mutating.

## Daemon, logging, concurrency

The daemon (nix 2.36.0pre, `builder-rpc-v0`) needs `dynamic-derivations`,
`ca-derivations`, `recursive-nix` in ITS nix.conf as well as the client's
(the error blames the client). Three nix versions are independent: the
daemon pinned by literal store path in `/etc/s6/sv/nix-daemon-srv/run`
(read the version THERE, never from this file; GC-rooted; a daemon bump
is a third moving part at a boundary, root edit plus restart), this tree's client (gates),
the consumer's client (rounds). A gate proves one client/daemon pair; say
which. Compatibility is only established empirically (newest pair:
`bea853d2` client, `3aef07e` daemon, corpus green at `df55697`).

`/tmp` is tmpfs: export `TMPDIR` to disk before a large copy (scratch there
is James's to delete). Never write to `/mnt/storage` (backup drive). Daemon
stderr: `/var/log/nix-daemon/current` (a crash names the worker pid). A
transport error can be a build failure: serialize before blaming the
daemon. A recursive round is one long-lived worker with a thread per inner
connection; parent-log silence is not inactivity; stalls synchronize on
purpose. HTTP 429 from a substituter looks identical by state.
`--keep-failed` misses a recursive build's inner tree;
`keep-failed = true` (set here) opens a daemon segfault in `cleanupBuild`,
so a crashed worker looks like the failure it supervised; crashes also hit
an unpriced evaluation window, so read the crash delta around a run. A
daemon refusing connections while burning CPU at PPID 1 is WEDGED (cgroup
rmdir loop); recovery is root and James's
(`local/incidents/2026-10-14-nix-daemon-cgroup-rmdir-loop.md`).

Logging (`logging-contract-repro.sh`): stdout is data (`--version`, `-t
list`, `-t drv` JSON, a `-v` build's task output); all else is stderr
prefixed `nix-ninja:`/`nix-ninja-task:`/`nix-ninja-dynamic-task:`. `FAILED`
is per edge (C), `ABORTED` per invocation (A, B; names the derivation); no
count line exists at zero, so absence of `FAILED` is a count claim. Nothing
per file or task without a gate. Round logs are torn: read the raw line.
`-t drv` on an aborting edge exits 1 with empty stdout: branch on rc.

Concurrency: `concurrent compilers = packages in flight x max-jobs` per
worker per connection (driver `-j` is not a term; in a build the first term
is also `max-jobs`, so machine-wide it is the square); `cores` sets the
driver's `-j` (`resolved_jobs`) AND threads per build. Size through Saxum's
governor, never literals. Realise latency is the daemon's single SQLite
mutex: answer throughput by closure and realise counts per task, never
cores; a `-j` sweep on a busy machine measures the daemon. `build_paths`
realises DERIVED FILES only (a task's own outputs), never shared store
inputs, and its `realised` memo is per process: a cold per-TU driver sends
one first-time realise plus its drv and text uploads, none cacheable, so a
persisted realise memo is refuted (queue, 2026-10-16). Read a daemon
window as worker CPU DELTA (`daemon-gap-cpu-probe.sh`, also counts
crashes), never load average. `DaemonStalled` is NO ANSWER, not busyness:
crashed workers produce it in milliseconds
(`local/audits/2026-09-07-daemonstalled-is-a-worker-crash-loop.md`;
`forced-include-source-header-repro.sh` reproduces it). Linux 7.3's
`cgroup_mode` renumbers every figure here.

Stats `file adds N s/M calls (H s on cache hits)`: stamp-cache hits
(`nar_stamp_hit`, ~97%) vs real uploads (the only part a change reaches);
THREAD TIME under `new_opaque_files`; store-add calls must equal the NAR
total (`counter_disagreement`). The cache persists across local drives
(`nar_stamps_snapshot`, `seed_nar_stamps`), cold in a sandbox. Realise minutes are single-path BUILDS of
first-time tasks (memo ~96%); releasing handles or depsets are refuted
fixes.

## Counting and evidence rules that cost a day each

- A zero is the instrument until proven otherwise: capture status and
  output, anchor globs, `find | head` is not a sample, a package-prefixed
  grep cannot see a task's log, an optional prefix defeats a `^` anchor, a
  `tail` discards the phase you would count. Never glob `/nix/store` at
  all: into an exec it overruns ARG_MAX and reads ZERO, and the shell's
  expansion alone reached 77 GB RSS over 650k entries and stalled a live
  round (2026-10-17). Gates use `store_find` (a streamed `find -maxdepth
  1`, first hit); say so in every subagent brief.
- Any claim supported by the absence of matching lines is a count. A
  success path that prints nothing cannot be counted from a log; use a
  counter.
- A `grep -c` for the rejoined form does not verify a backtick repair; use
  `awk '{n=gsub(/`/,"`"); if (n%2) print NR}' FILE` and iterate to zero.
- Verify every fix on one small package on B before it reaches a closure:
  A GATE MODELS THE HALF ITS FIXTURE HAD; the package carries both.
- Read the failing task's own derivation and the LEAF derivation's log
  before naming a class (a `.drv` names its dependencies even when its log
  is `not available`); a class read from the error text has been wrong
  here repeatedly. Re-read a proposed fix against the log that motivated
  it. A `strings` hit proves text exists, not that a path runs. A
  subagent transcript names the requested model, not the resolved one.

## Where things are

| subject | file |
|---|---|
| design | `docs/design.md`, `docs/dynamic-deps.md`, `docs/dynamic-derivations.md`; dyndep/Fortran/modules in the header of `crates/nix-ninja/src/dyndep.rs` |
| history | the three full records above; `local/incidents/`; `local/daemon-wedge.md` |
| n2 patches, ledger | `local/upstream/`, `local/upstream-ledger.py` (run on `main`) |
| fixture-shape sweep; opt-out triage | `local/audits/2026-09-03-fixture-shape-sweep.md`; `local/opt-out-triage.md` |
| upstream todo (theirs); conventions; benchmarks | `docs/todo.md`; CONTRIBUTING's Conventions section; `bench/e2e.sh`, `crates/nix-ninja/benches/generate.rs` |
| Saxum's record | `~/Projects/saxum/docs/nix-ninja-recovery.md`, `~/Projects/saxum/docs/hand-run-ledger.md` |
