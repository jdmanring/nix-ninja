Guidance for Claude Code in this repository. Deliberately short: the full
record with every measurement and retraction is
`local/incidents/2026-09-04-claude-md-full-record.md` and
`local/incidents/2026-09-17-claude-md-full-record.md` (the verbatim file
this one replaced); read them before changing any status here.

@local/claude-md-detail.md

## What this is

`nix-ninja` emits one Nix derivation per translation unit: a killed build
costs one object, a binary cache substitutes objects, editing one `.cpp`
rebuilds one TU. This tree is the continuation of `pdtpartners/nix-ninja`,
which is dormant (last commit `9a07e67`, 2026-05-07). **This tree is now the
primary project**: `origin` is the project, and `upstream` is an archive kept
for history. The public documents were rewritten on 2026-10-12 to say so,
`README.md` and `docs/ROADMAP.md` among them, and this file said "developed FOR
UPSTREAM" for a day after that stopped being true. The consumer is
Saxum, a distribution built on one workstation, checkout `~/Projects/saxum`
(lowercase; the capitalized spelling broke things), editions
`.#saxum-kde`, `.#saxum-xfce`, `.#saxum-server`.

Do NOT open pull requests or issues against `upstream`; they would not be
reviewed. `local/upstream/` is history and drafting material, not a queue.

## Remotes

    origin     git@github.com:jdmanring/nix-ninja.git     THE PROJECT; push freely
    upstream   https://github.com/pdtpartners/nix-ninja   FETCH ONLY, dormant

`main` tracks `origin/main`. It tracked `upstream/main` until 2026-10-13, when
that was found and changed: a bare `git pull` on a primary project must not read
the archived one.

Default
branch `main`. Commits carry no AI attribution; James is sole author. Never
`git add -A` on a PR branch: `/local/` is only ignored on `main`. `.git/info/exclude` backstops `local/`,
`CLAUDE.md`, `.graphifyignore`, `scripts/` on every branch. `local/` is its
own git repository (`rev-parse --show-toplevel` inside a gate script answers
about it); `CLAUDE.md` is gitignored.

## THE ONE THING TO UNDERSTAND: what an edit costs

Saxum has hundreds of thousands of banked per-TU outputs. Two independent
tests decide whether an edit spends them; passing the first is not enough.

1. **Fileset test.** `nix-ninja-task`'s `src` in `modules/flake/overlays.nix`
   is an allowlist: `Cargo.{toml,lock}`, `crates/nix-{libstore,ninja-task}`,
   `crates/deps-infer`, `vendor-n2`. An edit inside it (tests included) moves
   the task binary, builder of every PLAIN task derivation, and re-keys every
   banked compile. `crates/nix-ninja` (the driver) is outside it, but
   `build_dynamic_task_derivation` makes the driver the builder of every
   DYNAMIC task derivation and embeds the plain derivation's JSON: a
   task-binary move re-keys both classes, a driver move the dynamic ones. A
   local drive (configuration A) never keys on the driver.
2. **Emission test.** A driver-only edit is free only if it does not change
   what the driver EMITS: `drv.inputs`, the emitted `$PATH`, `passAsFile`,
   the forwarded environment, the discovered input set. Any of those re-keys
   every emitted derivation whatever the fileset says.

**Price by `drvPath`, from this directory, never by reasoning:**

    nix eval --impure --max-jobs 0 --raw \
      .#packages.x86_64-linux.nix-ninja-task.drvPath

`local/gates/emission-price.sh [BASE_REV]` asks both questions for the
working tree against a revision (A): the task `drvPath`,
plus the emitted derivation for a plain compile, a `deps = gcc` compile, a
link and a generator (one shape cannot answer the emission test). Three
control arms run before any price prints, because a free verdict from a
dead instrument has shipped twice. Caveats: FREE on five shapes says no OTHER
shape re-keyed, not that a shape none carries is safe; drivers run under
`env -i`, so a newly forwarded variable is invisible
(`local/gates/forwarded-env-price.sh` reports it); a free verdict on A is a
claim about A where an effect needs a sandbox, and
`local/gates/emission-price-b.sh [BASE_REV]` takes the same comparison
inside recursive-nix, BOTH SIDES AT ONE BUILD PATH (the absolute build dir
is emitted). Price against the CURRENT tree's base, never a remembered
HEAD. Not through Saxum (it pins a rev). `git add` new files first: an
untracked file is invisible to the eval and breaks every `example-*`
attribute. The blanket (below) is gated `!is_gcc_task`, so an emission
change confined to it never moves a compile.

**A pin bump pays two bills and only one is large.** Package derivations
re-derive (`nnCcHook` embeds the driver path as CONTENT, and
`ccWireAlways`/`withNixNinjaAttrs` add both binaries as inputs; the
stable mount `/nn-driver` removes both carriers once the consumer exports
it and drops the inputs, which the driver allows since `779a742`; landing
that changes hook content, so it re-keys every cc-route package, the banked
server included; James's order is that the server is NOT rebuilt, and the
toolchain half of that proposal is WITHDRAWN by measurement (the flag takes
the gate from 5 refusals to 1594, because the class rule wires only packages
that emit ninja and the re-keyed plan is mostly autotools), so what remains
is the hook and the two input lists alone;
`local/pending/stable-cc-hook-proposal.md`). Dynamic task derivations key on the
DRIVER; plain ones on the task binary and the EMITTED BYTES, never on which
driver emitted them, so an emission-free driver bump costs each package a
re-derivation whose expensive half substitutes (0.11% dynamic, discriminated
by BUILDER, not name). `nix derivation show`
on these needs `dynamic-derivations`, `ca-derivations` and `recursive-nix`
TOGETHER; missing one exits 1 printing only the refusal.

**READ DEPENDENCIES FROM `inputs.drvs`, NEVER `inputDrvs`.** The two are
different fields and only one exists. Read, not described:

    top-level keys        derivations, version
    derivations           keyed by the drv path
    each entry's keys     args builder env inputs name outputs
                          structuredAttrs system version
    dependencies          entry["inputs"]["drvs"]
    the build attrs       entry["structuredAttrs"], NOT entry["env"]
    entry["inputDrvs"]    ABSENT

A reader that asks for `inputDrvs` gets a missing key, and `.get(k, {})` turns
that into an EMPTY MAP that prints as `0` or `none` - so the failure mode is a
confident "nothing depends on this", not an error. `scripts/dd-dryrun.py` in the
consumer reads both spellings for this exact reason: two unrelated derivations
compare equal and the tool reports "nothing changed". Two sessions lost a
finding each to it on 2026-09-29, one of them having already written the trap
into a research document nobody about to run the command would read.

`structuredAttrs` IS WHERE `nativeBuildInputs` LIVES, and this list omitted it
until 2026-10-04. Where stdenv sets `__structuredAttrs`, `env` holds the OUTPUT
PATHS AND NOTHING ELSE, so a comparison of two package derivations over `env`
reads byte identical apart from the four output paths while every build input
differs. Measured on linux-pam, the routed instance against the one in
`pkgs-gaming`: the `env` diff was four lines of output paths and the whole
nix-ninja wiring (`nix-ninja-hook`, `nn-audit-hook`, both binaries, nix-dev,
`requiredSystemFeatures`) sat in `structuredAttrs`, unread. The failure mode is
the same as `inputDrvs`: not an error, a confident "these two are the same
package".

**The builder is in every key and does not have to be.** Both binaries'
paths sit in every task derivation as `builder` and in `drv.inputs`, while
the outputs do not change when they move; content addressing does not
recover it (early cutoff works ABOVE an unchanged output; the cost is at
the LEAVES). `NIX_NINJA_TASK_BUILDER` and `NIX_NINJA_DRIVER_BUILDER` take
the builder paths from the environment, drop the binaries from the inputs
and key on `NIX_NINJA_TASK_ABI`, as nix maps `/bin/sh` through
`sandbox-paths`. Proposal and open items:
`local/pending/stable-task-builder-proposal.md`. Rules:
- UNSET, every emitted derivation is byte for byte what it was. The
  generation reaches the emission only with a stable builder configured
  (`emitted_abi`, over the pure `abi_in_key`); a non-absolute path is
  REFUSED; a path without the ABI is refused at `builder_path`. The driver
  prints `task keys name builder` once per process on stderr, nothing when
  unset (a run ignoring the knob is otherwise indistinguishable).
- The two entries are in `/etc/nix/nix.conf`, verified AGAINST THE DAEMON;
  a `sandbox-paths` entry mounts its closure (no static build needed). The
  driver's rpc client does not forward `--option extra-sandbox-paths`;
  without a daemon entry every task fails loudly naming the path.
- THE MAPPING TRACKS THE PIN THE CONSUMER BUILDS, not HEAD: it moves in the
  SAME change as a pin bump and at no other time (read it with the free
  half of `stable-builder-mapping-read.sh`). A stale mapping fails SILENTLY
  while the old path is in the store; the ABI says what a task WRITES, not
  which binary is mounted.
- A change altering what a task WRITES moves the generation and pays once;
  a fix to a task that previously FAILED moves nothing, and most fixes here
  are that kind. Turning it on is one last global re-key.
- THE KNOB PROTECTS ONLY WHAT WAS CREATED AFTER IT WAS IN PLACE. A frozen
  derivation protects itself by carrying the ALIAS as its `builder` with no
  task store path among its `srcs`; one made before the alias carries the
  store path directly and re-keys on a task-binary move however the knob is
  set. MEASURED 2026-10-14: three drvs of the consumer's current round read
  `builder: /nn-task/bin/nix-ninja-task` and name no task path, while an older
  campaign's drv names `fwks8vlr...-nix-ninja-task-0.1.0` and would re-key.
  So a cheap move is a claim about the DERIVATIONS, not about the knob: read
  the failing drv's `builder` and `srcs` before saying a task-binary move is
  free.
- The dynamic class is covered transitively, witnessed on
  `example-dynamic-deps`. WHICH ARM RUNS IS GATED ON
  `config.is_output_derivation`: local mode (A, B, the consumer's route)
  never builds a dynamic derivation; it re-adds the PLAIN one through
  `update_derivation_with_discoveries`. Only `sandbox_adddrv_calls` says a
  dynamic derivation was created; `local/gates/example-arm-census.sh
  --build` is the census. The consumer's own edge is
  theirs (`nativeBuildInputs`, `nnCcHook` content).

The bump from `37672a4` is priced in
`local/pending/pin-bump-37672a4-to-head.md`: nine net fileset commits
(re-counted 2026-09-19, `f241387`, `8a786a1`, `81243e2` among them), `drvPath` MOVED, link shape moved by
`5235867`, so no cheap subset exists and the knob goes on IN THE SAME
CHANGE. `f241387`, `d402c06`, `775dc12`, `8a786a1`, `81243e2` are also not ancestors of `37672a4`.

THE SINGLE REKEY WAS TAKEN AT THE PIN `b00dfaf` (consumer `ac4b052`,
2026-09-29; task binary `x92ay33p` -> `fwks8vlr`). The bumps through
`4ef51fd` moved the DRIVER ONLY (`d4e82ca` `66ba407`, `5f0fe07` `2049344`,
`c6040a0` `429ffd5`, `4ef51fd` `3122854`); THE TASK BINARY HAS MOVED SINCE,
at `a9d5dd5` (`miniz_oxide` in `Cargo.lock`), at `e1101af` (writable copy
of an edit-in-place tree), and at `239d6b1` (an output the command writes is
copied writable rather than linked read-only - the mesa-libclc abort; task
`3vnxcibcqqfin...` -> `86si75pg117f...`, the first MOVE OF THE 0.3.0 TASK BINARY
this list records). Under the stable-builder knob the consumer runs,
those moves leave banked compiles keyed as they were and move only the
`/nn-task` MOUNT; that is why the mapping is read from the pin, below. The
current pin is the consumer's lock, not this list. The rekey
lineage carries `5222a3e` - the merge of `nn-driver-compile-artifact` (the
phony and macro fixes), the space half, the two readers for the colord ITS and
libnvme swig classes, and the tests pinning their EMITTED SPELLING - plus the
libpq build-path collision fix (`6f490ad`, `acfd3d5`). THE COLLISION FIX RIDES
THE SAME REKEY because it is driver-only and priced FREE on all five shapes;
landing it separately would pay a second global re-key for one change. Read the
binaries, which the daemon mapping names, from the CONSUMER'S OWN FORM, a
by-revision fetch, tracked files only:

    nix eval "git+file:///home/james/Projects/nix-ninja?rev=<full-rev>#packages.x86_64-linux.<attr>.outPath"

DO NOT read them off `builtins.getFlake (toString ./.)` of the working tree: the
path-based eval yields a DIFFERENT store hash for the same source, one Saxum
never builds. At `5222a3e` the two forms differ (`pdr5gkj` dirty-tree vs
`p12v9mh` by revision), and re-measured on a CLEAN tree at `f855c5e` they still
do for the DRIVER (`id2d2a5a` path-based vs `1j9xav9f` by revision) while the
TASK is identical either way (`fwks8vlr`).

THE MECHANISM IS NOT THE `local/` TREE, which this file claimed until it was
measured. `local/` holds ZERO `.rs` files, and there are no untracked or
gitignored files matching the source globs at all. The cause is that the
DRIVER's `src` is a GLOB over the whole tree (`Cargo.lock`, `**/Cargo.toml`,
`**/*.rs` at `modules/flake/overlays.nix`'s top-level `src`) while the TASK's
is an allowlist of NAMED DIRECTORIES, so the driver's glob sweeps in
`target/debug/build/serde*/out/private.rs` - cargo build artifacts that exist
in the working directory and not in any git tree. A by-revision fetch exports
tracked files only and never sees them. THE RULE IS UNCHANGED AND THE REASON
WAS WRONG: a session that checked `local/` for `.rs` files, found none, and
concluded the warning was stale would then read the wrong driver. The stray
`target/` is itself leftover: `.cargo/config.toml` redirects `target-dir` to
`~/.cache/cargo-target/nix-ninja`, so nothing writes there now, and the one on
disk is dated 2026-09-01 at 313 MB. Removing it would make the two forms agree
for the driver, which is a reason to keep reading by revision rather than to
rely on its absence.

THE TASK BINARY WAS UNCHANGED FROM `b00dfaf` THROUGH `4ef51fd`; only the
driver moved (`b00dfaf` `lmnnnv8h`, `5222a3e` `p12v9mh`, `acfd3d5`
`7jnfx2ih`, `d4e82ca` `n1kgg95s`, `5f0fe07` `jgr70jsl`, `c6040a0` `s8i050y2`,
`4ef51fd` `3y1q34b0`). It has moved since (above), so never assume the
fileset axis is quiet: read the task `drvPath`, and price a move under the
knob with `emission-price-knob.sh` (`[BASE_REV]`), since unknobbed
`emission-price.sh` reads every task-binary move as GLOBAL RE-KEY whether or
not the consumer pays it. Its three arms (unknobbed control must move, an
ABI change must move, the knobbed subject then decides) all fired on
`526d293` -> `a10a5fc`: FREE UNDER THE KNOB.

READ THE MAPPING FROM THE PIN, NEVER FROM THIS FILE. This file no longer prints
the pair: it printed one three times (`5222a3e`, `b00dfaf`, `d4e82ca`) and each
went stale at the next bump, the first two while a session was about to act on
them. The pin is `.nodes["nix-ninja"].locked.rev` in the consumer's
`flake.lock`; a bump moves the pair in the SAME change, and every superseded
path stays resident, so a mapping left at any earlier revision MOUNTS THE OLD
BINARY AND SUCCEEDS SILENTLY. `local/gates/stable-builder-mapping-read.sh`
reads the pin from that lock, evaluates its pair by revision, and ends with a
verdict: `CURRENT` (exit 0), `STALE task:`/`STALE driver:` naming both paths
(exit 1), or `NO ANSWER` (exit 2). Its free half is safe during a round. BOTH
HALVES MOVE: the driver with every driver commit, the task with any change
inside the task fileset, so read the gate rather than assuming either half
is still current. Dated readings go in `local/pending/next-rekey-batch.md`.

THE PIN HAS A FLOOR AND `local/gates/pin-floor.sh` ENFORCES IT. This file states
that two driver commits MUST NOT BE PINNED and that the consumer takes `425778a`
or later. THIS TREE HAD NO INSTRUMENT FOR IT UNTIL 2026-10-12: the corpus runs at
HEAD, so a consumer pinned at `34038d2` passed every gate here, and the mapping
gate would read `CURRENT` for that pin's own pair, so nothing on THIS side could
fire. The consumer's side was NOT unguarded, and the first version of this row
claimed it was: `~/Projects/saxum/scripts/test_upstream_status.py` holds
`_REV_HISTORICAL`, a table of revisions that must not be the current pin, and
asserts over it against the lock, so it already refuses both `34038d2` and
`e93c9da` (VERIFIED 2026-10-12 by reading the assertion at the source, not by
taking the claim: it fails any entry whose hash the lock resolves to). The two
instruments are complements and both should run: THAT one fires from a
hand-maintained list, so it catches a pin someone already classified as bad;
`pin-floor.sh` asks git ANCESTRY, so it catches a revision nobody has classified
yet. The gate reads the lock and answers `SAFE` (0), `BAD PIN` (1, naming WHICH
defect: inside the recorded-bad window, an ancestor of the floor, or DIVERGENT
from this line entirely), or `NO ANSWER` (2). Seconds, no build, safe during a
round. Run it before answering any pin question and beside a boundary. If you add
another MUST-NOT-PIN revision, add it here, in that gate, and in the consumer's
table in the SAME change; adding it to only one leaves the rule half-read.

THE MAPPED TARGETS ARE READ AT DAEMON START, SO A COLLECT TURNS THIS LINE INTO
A HARD STOP. The 2026-10-01 collect took both targets of the then-configured
line, and every sandbox setup in the daemon failed with

    getting attributes of path "/nix/store/<old>-nix-ninja-0.1.0":
    No such file or directory

including the builds of the pin's own two binaries, so nothing could be built
until `/etc/nix/nix.conf`'s `extra-sandbox-paths` was repointed AND the daemon
restarted. A resident-but-superseded target is the silent case this block is
about; a collected one is loud and still blocks everything. Do not run a
collect while the mapping names a path worth keeping.

A STUB LINE CAN STAND IN THE FILE. After the 2026-10-01 collect the line was
pointed at two arbitrary RESIDENT outputs (`0inx1h2l`, `3d2nnypa`), because the
pin's pair could not be built while the line named collected paths. A stub is
safe to build under (any resident nix-ninja satisfies a mount) and NOT correct
long-term: a task mounts a binary that is not the pin's, the silent-staleness
case reached from the other direction. The gate's verdict reads `STALE` on both
halves while it stands; replacing it is a `/etc/nix/nix.conf` edit plus a
daemon restart, between rounds. `nix config show` prints what the FILES say as
this client reads them, not what the running daemon loaded at start; only the
gate's `--daemon` half reads the mount.

THE FILE HALF CANNOT SEE TIME, AND A RESTART THAT DID NOT HAPPEN LOOKS LIKE ONE
THAT DID. Measured 2026-10-12, both shapes in one repoint. (1) The gate's free
half returned `STALE task`/`STALE driver`, correctly, and then returned `CURRENT`
eleven minutes later on the SAME FILE after James ran the sed; nothing in its
output says which moment it describes, so a verdict carried past the write is
the failure, and the pin had landed while the file was only about to. Re-read the
free half before acting on it, or take the reading that runs against the live
service. (2) The repoint landed at 15:21:40 and per-connection daemon children
appeared at 15:23, which reads as a successful restart; the master had not
re-execed (`reaped child process N, status = succeeded` is a CHILD of the old
master), so the daemon still held the pre-repoint pair. `ps` on a pid proves
nothing in either direction, and neither does the presence of children, because
they churn either way. THE THREE READINGS THAT DISCRIMINATE: the socket owner
(`ls -l /nix/var/nix/daemon-socket/socket`, owner and mtime unchanged when no
restart happened), the s6-supervise child (`ps -eo pid,ppid,lstart` filtered to
PPID of the supervisor, which is the MASTER), and the `--daemon` hash probe,
which mounts the mapped paths and compares them to the configured ones. The
third is the strongest and is how the daemon was shown to be mounting the OLD
pair by CONTENT (`d48663ef`/`439c2895` mounted against `17faa1d0`/`d713b878`
configured) rather than by an mtime argument; it is a BUILD, so it belongs in a
gap, and it carries a nonce or it replays a stored output.

THE CORPUS'S LATEST RESULT IS IN THE LEDGER, NOT HERE (a count in this file goes
stale at the next run). Every NO ANSWER in the 2026-10-03 run at `4ef51fd` but the
mapping arm was a gate reading a LITERAL STORE PATH a collect had taken; gate
inputs are now resolved by revision or attribute. A gate that hard-codes a store
path is a NO ANSWER waiting for the next GC: do not write one.

THE RULE HAS ONE LEGITIMATE EXCEPTION, and it is in the corpus, so know it before
"fixing" it: `round-intercept-sweep.sh` holds a literal
`/nix/store/...-rustc-1.98.1.drv` for its live arm. IT IS NOT DERIVABLE, which is
the test that decides the case: that path is a RETAINED ROUND'S leaf log, not a
tree artifact, and resolving the same subject by attribute returns a DIFFERENT
derivation (`p.rustc.drvPath` answers `naqdjc49...-rustc-wrapper-1.98.1.drv`, the
wrapper, not `2r5g9f28...-rustc-1.98.1.drv`). So repointing it by attribute would
silently read the WRONG derivation, which is worse than the literal. What the
exception requires instead is that the absence FAIL LOUD: the gate prints `NO
ANSWER: leaf log for $drv is no longer available (collected?)` and exits 2. Read
the two cases apart: a literal that CAN be derived is a defect to fix; a literal
that cannot is acceptable ONLY with a loud NO ANSWER.

THE READERS ARE DRIVER-ONLY, which the task `drvPath` proves: it is unmoved
from `2ab5e0a` to `5222a3e`, so they cost nothing on the fileset axis. The task
binary DID move across the bump itself (`x92ay33p` at the pin to `fwks8vlr`
here), and the task binary is the builder of every PLAIN task derivation, so
EVERY banked compile is re-keyed by this bump whatever the readers do: their
added inputs land inside a re-key already being paid. `emission-price.sh
b00dfaf` is FREE on all five shapes with three control arms passing, which says
the driver changed no byte it was not aimed at; those five shapes are a compile,
a `deps = gcc` compile, a link, a generator and an app, so the verdict is
SILENT about swig and msgfmt edges. There the evidence is the unit tests below
plus the real-edge runs, both reductions.

**Batch every re-keying change and pay once.** The bill is the same for one
change or ten; landing them one at a time has lost days. Do not garbage
collect while a re-key is unproven. Never start a build without stating its
cost and that it has started; never build without first establishing you
are ready. THE WORK LANDS ON `main`; no batch branch exists. Record every
price in the re-key record.

## How Saxum consumes this

By revision from GitHub (`github:jdmanring/nix-ninja/<rev>`), never `path:`.
Loop: commit, push to `origin`, bump the rev in Saxum's flake. Unpushed work
does not exist to the consumer.

THE PIN PROTOCOL, agreed with the consumer session 2026-10-03 after one day
cost four pins and three gcc starts (every driver bump re-keys the cc-route
world through `nnCcHook`):
(a) the consumer bumps only at a CAMPAIGN BOUNDARY, never mid-build, except
for a fix to a failure inside the running build; (b) before any build costing
hours it asks this tree for the complete in-flight list, answered with
`git log <pin>..origin/main`, `git status` of both trees and the queue in
`local/pending/next-rekey-batch.md`, and everything ready rides one bump;
(c) between boundaries this tree pushes freely; (d) a mid-build fix arrives
with evidence, the failing drv and its price (emission vs the pin, task
`drvPath`) before it lands; (e) a failure sent here carries the failing
task's drv and leaf log, never a round log's summary line; (f) the
stable-builder mapping moves at the SAME boundary, and the daemon restarts
only when it serves nothing (`nix store ping` answers, no `nixbld`
processes, no orphaned `nix-daemon` with PPID 1). A restart mid-build kills
the master and orphans the per-connection workers, and `nix-daemon-srv`'s
run script cannot return while a worker's build cgroup is populated, so
every NEW client is refused until that build ends (measured 2026-10-03).

nix-ninja code in `~/Projects/saxum/scripts`: `nn-cc-shim.sh` (cc drop-in,
one-edge build.ninja per TU, `NIX_NINJA_NO_BUILD_DIR_SCAN=1`, `deps = gcc`,
execs the real compiler for `-include`/`-imacros` TUs), `nn-ninja-shim.sh`
(execs real ninja for any `-t`), `.nix` wiring, tests. Moving the shims
here is James's call; the shim is hook content, so a comment in it re-keys
that route. `nnOptOut` (`site/base/default.nix`) is the opt-out list: an
opt-out needs a named defect and a route to removing it, never a fix.
`ccPreferred` is the reroute list, every entry carrying a `DEFER` naming
the driver defect: count it from the file every time; whether a fix is in
the pin is one `git merge-base --is-ancestor` per entry; a fired trigger
still earns one build on B before the entry comes out. TWO OF ITS ENTRIES
ARE NOT DRIVER DEFECTS and a pin bump will not clear them: `shared-mime-info`
(`cannot locate ITS rules`; the reader drops the store-path datadir, above)
and `harfbuzz` (the gir failure IS closed; a typelib abort remains, mechanism
unmeasured). A reroute whose entry the driver cannot fix stays live however
far the pin moves, so `git merge-base --is-ancestor` is necessary and not
sufficient for those two.

`withNixNinjaAttrs` (`site/base/default.nix`) appends `[ nixNinja nixNinjaTask
nixPkg ]` to EVERY routed package, and the cc branch appends `pkgs-gaming.nix`.
NIX'S HEALTH IS THEREFORE LOAD-BEARING FOR THE WHOLE DISTRO: when a nix build
fails, its dev output never exists, every package naming that output dies, and
the failure surfaces as an unrelated package (measured 2026-09-29: one boost
1.91 assertion in nix's own test suite stopped all three editions, reported as
`libimagequant-src-lock`). Read a nix-shaped failure before believing the
package it names.

THE FIX FOR THAT ONE IS A PATCH ON NIX, NOT A BOOST PIN. `boost::urls` 1.91.0's
`host_address()` returns the ENCODED length for an IPv6 literal with a zone id,
NUL-padding the tail, so two bytes become `%00%00` in every stringified scoped
IPv6 URL; five `nix-util-tests` and one `nix-store-tests` assertion compare
against that. It needs boost 1.91 AND the checks together. Upstream is NixOS/nix
PR 16482, carried in the consumer at `patches/nix-16482-zoneid-url-fix.patch`
and applied by `nixUrlFixOverlay` in its `flake.nix`. TWO TRAPS, both measured:
patching the `nix` package alone never reaches the object under test, because
the failing binary comes from `nix-util`, its own derivation - the seam is
`nix.lib.makeComponents { pkgs = prev; }` then `appendPatches`; and the overlay
must be in `gamingOverlayNames` as well as the main list, because routed
packages take `pkgs-gaming.nix`.

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
configuration. Driving a package on A: `local/second-run.sh` is the recipe;
`generator-edge-execution-b.sh` with `NN_PROBE_FULL=1` builds any package
on B without a pin bump.

## Architecture

- `crates/nix-ninja`: the DRIVER. Parses build.ninja via `vendor-n2`;
  `task.rs` turns every edge into a derivation (the emission test is about
  this file). `NIX_NINJA_DRV` selects submit-to-daemon or symlink into the
  build dir (`local.rs`). `dyndep.rs`; `resolve_cache.rs` (memo;
  `persistence_enabled` is `!in_nix_build`). BOTH BINARIES RESOLVE OFF PATH
  ONLY WHERE THEIR STORE PATH IS KEYED (`Tools::new`, `779a742`): with
  `NIX_NINJA_TASK_BUILDER`/`NIX_NINJA_DRIVER_BUILDER` set the sandbox mount
  supplies the binary and nothing looks on PATH, so a consumer may drop both
  from `nativeBuildInputs`; unset, the lookup is what it always was. A
  package that shells out to either binary by BARE NAME is the case this
  does not cover and the case to read before dropping the inputs.
- `nix-ninja -t dynamic-task` (`subtool/dynamic_task.rs`): builder of
  dynamic task derivations; re-reads its drv, discovers
  (`discover_dynamic_dependencies`, `scan_seeds`), resubmits. A `deps = gcc`
  edge in local mode discovers here too, with `built_paths` (REALISED
  inputs) as its virtual map, not `build_task_derivation`'s `virtual_paths`.
- `crates/nix-ninja-task`: builder of every task derivation. Materialises
  inputs (`derived_file.rs`, `create_symlinks`), runs the command, patches
  rpaths (`patchelf.rs`, `assemble_rpath`). The sandbox PATH is EMITTED, so
  every entry is gated: `make` when the link is LTO (argv AND the cc's
  `cc-cflags-before`), `gcc-ar`/`gcc-nm`/`gcc-ranlib` from the wrapper's
  `orig-cc` when named, `nm`/`readelf`, a text-filter set unconditionally;
  `as` needs none (`-B`). The compiler wrapper (27 closure paths against 14
  for all else) is an input and PATH entry only where the command names a
  toolchain program by basename, the edge is `deps = gcc`, or every
  declared output is object shaped (`command_names_toolchain`,
  `task_needs_cc`, `require_cc`); a script execing cc unnamed fails loudly.
  `command_program` steps over leading assignments, `:`, `&&`, a `cd` pair
  and `env`; an `if` still comes back as the program.
- `crates/deps-infer`: include scanner (`retrieve_c_includes_checked`,
  `canonicalize_cached`), inside the fileset. A header reached through a
  link is declared canonical AND as spelled (`extract_includes`), a
  store-path spelling excepted.
- `crates/nix-builder-rpc-client`: `builder-rpc-v0`.
- `modules/flake/pkgs/mkMesonPackage`, `mkCMakePackage`: set
  `NINJA=nix-ninja`, `NIX_NINJA_DRV=true`, `builder-rpc-v0`; Saxum uses
  them, so editing them is emission-side. THE ATTRIBUTE SET DECIDES AN
  EXAMPLE'S CONFIGURATION: `packages` holds four, all `mkRecursivePackage`
  (B); `legacyPackages` thirteen (C). Spell them in full
  (`.#legacyPackages.x86_64-linux.example-dynamic-deps`); read the
  `attrNames` of both sets, never the directory listing.

Mechanisms in `task.rs`, cited by symbol because line numbers rot:
- **Implicit-input blanket**: `build_dir_inputs` (from `read_build_dir`, one
  caller per process) swept into every task gated `!is_gcc_task && len <=
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT`, for configure files no edge declares.
  The walk skips produced nodes, the driver's state files
  (`build_dir_disposition`) and declared depfiles, so the map is the same
  every pass (a growing map once switched the blanket off).
- **Compile vs link**: `is_compile_task` = `deps == gcc` plus
  `object_shaped_outputs`; discovery gates on a `-c` on the command line,
  never on output name (suffixes are an open set); a `-c` directly after a
  shell is rejected (`f381c92`, else every `mkCMakePackage` target breaks).
  Polarity: an allowlist is safe gating what a build merely carries, unsafe
  gating what correctness needs.
- **Generated files** are reachable at three points: resolve
  (`virtual_paths`), scan (no bytes yet), upload (nothing to upload). Fixing
  one leaves the next fatal.
- **Environment**: `forwarded_to_tasks` is an allowlist (`NIX_*`,
  `XML_CATALOG_FILES`, `PYTHONPATH`, `XDG_DATA_DIRS`,
  `NIX_GOBJECT_INTROSPECTION_DEFAULT_FALLBACK_LIBPATH`); values go through
  `outer_rewrite_map` and store paths in them become inputs; forwarding
  everything bakes `$out` into every key. Outer output NAMES come from
  `outputs` or the `__structuredAttrs` file (`outer_output_names`); the
  self-rpath pair in `NIX_LDFLAGS` is forwarded as a placeholder, keyed on
  the output name alone (no deriver). The map rewrites only a store path
  opening its token or following a separator.
- **Readers** for statically reachable undeclared inputs sit beside
  `cmake_script_referenced_paths` in the driver (one in `deps-infer` spends
  the task binary): cmake `-P` `set()` and `include(<Name>)` after
  `rebase_post_cd`, XInclude, TableGen, `rst_include_closure`, the
  `glib-compile-resources` manifest, a `-include`/`-imacros` seed (cwd then
  the `-I` chain, ahead of scan and depfile). THE `-P` READER ALSO DECLARES
  THE SCRIPT ITSELF (`fe4e173`): it read the script's CONTENTS for targets
  and never pushed the script, so a configure-written
  `<name>-mkdirs.cmake` that no edge declares was carried only where an
  edge DEPENDS on it (llvm's case) and the task ran with an EMPTY input
  list, dying `Not a file` (curl-impersonate). A reader that reads a file
  must declare the file. Readers rebase with a LEADING
  `cd` only; only `undeclared_outputs` tracks a mid-command `cd`. A rebased
  path is guarded by a read, so a wrong spelling DROPS an input silently.
  THE DISPATCH CARRIES NINE TRIGGERS and this list named four of them for
  long enough that the named-nowhere ones were where the live defects sat:
  bound at the top of the loop are `tablegen_invocation`,
  `xinclude_invocation`, `rst_invocation`, `gresource_invocation`,
  `msgfmt_invocation` (`--datadirs`, the `.its` a locating rule names),
  `swig_invocation` (a `.i`'s `%include`), `gir_filelist_invocation` (the
  `g-ir-scanner` `--filelist`) and `protoc_invocation` (a `.proto`'s
  `import`), with `python_module_invocation` bound inside the loop. The
  four in the second group are the same family - a file the COMMAND reads
  and no edge declares - and each splits trigger from reader so the trigger
  is testable without a filesystem. Read the dispatch from `task.rs` before
  trusting any enumeration here; this one was miscounted twice.
  A TRIGGER THAT PARSES A FLAG MUST HANDLE EVERY SPELLING THE REAL EDGES
  USE, and the join/separate pair is where this went wrong: `protoc`'s
  reader first checked `strip_prefix("-I")` before the bare `-I`, so the
  separated `-I .` - the form CMake emits and upb's failing edges carry -
  matched the strip with an EMPTY value and the reader resolved no imports
  at all while its unit test passed (`protoc_invocation` is the fix). THE
  TRAP IS THE STRIP, NOT THE PAIR, and the sweep says so: `-I` has no
  separator so a bare flag strips to empty, while `msgfmt_invocation`'s
  `--datadirs=` keeps its `=` and cannot match a bare flag, so its ordering
  is safe. Whenever a trigger grows a flag, check whether its stripped form
  can be empty. THE SAME SHAPE HIT THE READER BODY: `proto_imports` listed
  the qualifiers it knew (` public`, ` weak`) and skipped the third in use,
  `import option` (fixed `526d293`). A parser that enumerates the forms
  between a keyword and its target drops the next form silently; parse to
  the delimiter instead, and count the real population before writing the
  fixture.
- **A reader's WIRING is three steps, and step two is the trap.** A reader
  pushes candidates into `referenced` beside the three dispatchers above;
  what happens next decides whether the reader does anything. (1) a
  RELATIVE candidate is rebased through `rebase_post_cd(&cd_dir, &cand)`,
  a LEXICAL join where each `..` pops a `cd_dir` component, so with an
  empty `cd_dir` the spelling passes through unchanged. (2) an ABSOLUTE
  candidate is dropped by TWO filters, `p.starts_with(store_dir)` and
  `!same_project_tree(build_dir, p)`, and `same_project_tree` compares
  only the FIRST component of each path. (3) later readers adopt what
  earlier ones pushed. SO A READER MUST EMIT RELATIVE SPELLINGS: one
  emitting absolute paths is dropped in silence and reads as a fix that
  changed nothing, which is a control whose arms cannot differ. This is
  the same shape as the pkg-config wrapper that was aimed at a function
  which did not build its subject, and it is the trap to check before
  believing either new reader works.
- **Aliases**: co-outputs expand at resolution (`co_outputs`); a symlink
  output pointing at a sibling is stored and placed as LINK TEXT
  (`copy_outputs_to_placeholders`, `create_symlinks`). A dangling link
  inside a store output is the DESIGNED state (`alias_link_target`);
  copying through it failed nixpkgs' forbidden-reference audit on brotli.
- **RPATH**: preserve every entry, empty ones and `$ORIGIN` included; CMake
  pads with empty entries and rewrites IN PLACE.
- **Uploads** name from the path asked about, never the canonical store
  path (`new_opaque_file`, `new_opaque_file_raw`; canonical naming nests
  hashes and each re-add takes a bind mount). A rewritten upload is a temp
  copy removed after the add (`upload_temp_done`). Directory arguments:
  python packages under `DIR_UPLOAD_CAP`, a data tree under `DATA_DIR_CAP`,
  else a named refusal.
- **Output names** (`normalize_output`) are a LOSSY LABEL, not identity:
  every character outside the store's set maps to one `-`, a leading `.`
  gains one, and an over-cap name is folded with a digest over the ORIGINAL
  path (`fold_long_output_name`; names at or under the cap return byte for
  byte, which is why that fold is not a global re-key). Two outputs of one
  edge can therefore arrive at one name - `.libs/version.o` and
  `.libs-version.o` both give `-.libs-version.o` - and `Placeholder::standard_output`
  hashes THE NAME ALONE, so both also render ONE placeholder. THE GUARD IS AT
  THE INSERT: `drv.outputs.insert` in `build_task_derivation` refuses and names both
  paths (added `3c45c1f`, in the pin). The set it walks is EXTENDED past
  `dedup_paths(&task.outputs)` with `undeclared_outputs`, the depfile and the
  verbose log, each deduped by PATH equality only, so the name check is what
  catches a collision the path check cannot. The mapping is sound BY DESIGN
  (identity is the content hash, so two names colliding still yield distinct
  store paths unless the content is also identical). The refusal is therefore
  NOT a name problem: the daemon keys a realisation on
  `DrvOutput{drvPath,outputName}` and refuses when `outPath != other.outPath`
  (`isCompatibleWith`, `realisation.hh`), which for a CA output means one
  `(drv, output)` was registered against two different CONTENTS. Measured and
  proven 2026-10-03: a collect left a stale `BuildTraceV3` row naming the
  collected object (the schema has no cascade and nothing deletes traces), the
  next drive rebuilt that drv, and the daemon refused against the dead trace.
  NO DRIVER CHANGE IS INDICATED. Two fixes were designed and both falsified
  before landing - an injective output name, and a client in-flight realise
  guard (the daemon serializes a floating CA build per `(drv, output)` with an
  on-disk lock held across workers, so a second requester reuses rather than
  registers). Proof, and every falsified reading, in
  `local/pending/next-rekey-batch.md` 2026-10-03.
- **Scope**: enclosing-file bindings as a fallback chain
  (`evaluate_and_read_file`); `include` leak-back is a known gap.
- **Build-dir walk** (`read_build_dir`, `build_dir_disposition`) asks the
  PATH, not the entry. Feeds `NIX_NINJA_ALIASES` (configure-time
  soname links only) and `NIX_NINJA_MAKE_DIRS`: a compile gets every
  include directory its command names that EXISTS (`include_dirs_named`; an
  absolute spelling ABOVE the build dir is dropped first, unwitnessed),
  other tasks the walk's empty directories. `local.rs` places products as
  symlinks, then `refresh_placed_mtimes` replaces each with a fresh-mtime
  copy (an alias stays a link); depfiles are COPIES (`make` stats through a
  link); a placed link pulls in the output it names
  (`link_targets_to_place`).
- **Outer-output staging**: `-I$out/...` and outputs declared inside the
  outer output are staged under `.nn-outer` FROM THE DRIVER
  (`outer_stage_outputs`) and copied out EAGERLY after the task runs;
  `new_task`, `normalized_task_outputs` and `add_derived_file` must agree.
  Refused on the output-derivation path; `-t drv` subtools only emit. The
  copy out goes through `copy_restoring`, because the thing it copies FROM
  legitimately holds a placeholder and the place it copies TO must not.
- **A TASK'S STORE OUTPUT IS SUPPOSED TO CARRY THE PLACEHOLDER, so never
  audit one for placeholder residue.** The task is handed placeholders and
  writes them; the restore happens at PLACEMENT
  (`symlink_derived_files`) or at the outer-stage copy
  (`copy_restoring`), never in the task. So a blanket check over task
  outputs in the store would flag every one of them and fail BY
  CONSTRUCTION, and the per-TU derivations correctly carry no audit line in
  the consumer's round for exactly this reason (their finding, 2026-10-03,
  alongside the measurement that their `fixupPhase` audit does fire: 544
  lines over 251 packages, so a round silent on residue is a reading). The
  audit belongs where a path is FINAL: a package output, or the bytes landing
  in `$out`, which is what the write gate's fourth arm scores. One coverage
  edge there, theirs: `nix-support/*` is written AFTER the fixup hooks, so a
  `fixupOutputHook` structurally cannot see it.
- **Edit-in-place trees**: a DIRECTORY a task both receives and declares as
  an output is COPIED writable before the command runs, not linked
  (`nix-ninja-task` main, ahead of `create_symlinks`), because the command
  edits it in place: ExternalProject's patch step runs `patch` inside
  `SOURCE_DIR`, which refuses a link into the store. The driver side:
  `external_project_extract_dirs` declares the tree on the step that writes
  it, `external_project_step` makes step edges stamp edges, and the tree
  rides its stamp as a co-output; a later step's registration of the tree
  SUPERSEDES the earlier one, the one place registration is not
  first-writer-wins.
- **Placement: a symlink into the store, or a COPY**. `create_symlinks`
  symlinks an input by default and copies it when `copy_not_link` holds, which
  is the ONE list `nix-ninja-task` owns: `.py`, `.mjs`, `.js`, `.cjs`,
  `node_modules`, every `header_like` suffix (one predicate, exported), and a
  build-tree EXECUTABLE whose loader search path is `$ORIGIN`-relative
  (`origin_relative_executable`, `f8fb5e8`). An alias is excluded and asked
  BEFORE the list, because a store object that is itself a symlink is dangling
  by design and `fs::copy` on it is ENOENT. A hard link is EXDEV in a sandbox.
  THE ARMS SHARE ONE REASON: a consumer that resolves its own location, or the
  location of a file beside it, lands in the STORE where its siblings are not.
  A script realpaths `__file__`, node realpaths a required module, a header is
  reached as a quoted sibling of an already-relocated header, and an ELF
  resolves `$ORIGIN`. Adding a member means finding the consumer that resolves
  a path, never a file that merely looks similar.
- **Build-dir deepening** (`main.rs`, the `escapes` closure) is the task's
  self-defense against a `..`-heavy input escaping the writable tree: it
  deepens its own build dir until every climb stays under `/build`, because
  `create_symlinks` does `create_dir_all` on an input's parent and a path
  outside the tree is an EACCES. THE MEASURE IS WHERE A CLIMB LANDS, NOT HOW
  FAR IT CLIMBS, and the guard is not the only thing that reads it: refusing
  to deepen for an input whose escape means "never in this tree" is what
  `5f0fe07` closed on the INPUT side, leaving this guard untouched. A host
  path a package's own header names (`/etc/hosts`) lands outside `/build`
  legitimately, so deepening for it moves the cwd for inputs that were fine -
  the whole tree resolves a name too deep.
- **Verbose build** (`-v`) writes task output to stdout via an extra
  declared output (CMake's ABI detection parses it).

## Failure classes: status table

Every entry has a gate arm or a named reason it cannot. "Verified" names a
configuration; "free" means driver only, task `drvPath` unmoved, five priced
shapes byte identical; "B full" is `generator-edge-execution-b.sh` full mode.
A "fixed" row is a claim about the half its gate MODELS until a package says
otherwise; ancestry says whether a fix is in the pin, never whether the
package builds. Mechanisms and retractions are in the archive.

| # | package, class | status | fix | gate |
|---|---|---|---|---|
| 1 | bison, installcheck recompiles through shim | closed in consumer | Saxum | `test-nn-cc-passthrough.sh` there |
| 2 | hicolor, eager `cc` resolution | verified B | `7fb756e` | |
| 3,4 | libvmaf, generated headers on compile edges | verified B via `example-nix` | `2f7b8f2` `dc296f3` `c45cc88` `cf75635` | `example-generated-header` does NOT prove it |
| 4 | liblapack, Fortran via CMake | verified B | version string, dyndep path rewrite, `-v` | |
| 5,15 | c-ares/abseil/openexr/libjpeg-turbo, RPATH record | closed; verified B | `2746512` `101d038` | `cmake-rpath-padding`, B full |
| | c-ares, an include that names a HOST file drags the task's cwd and loses the whole tree | FIXED, PUSHED and PRICED `5f0fe07`. Priced FREE on all five shapes with the three control arms (sensitivity, determinism, fileset-ctl) passing BEFORE any price printed, and the task `drvPath` unmoved (`r5q3ich6`), so landing it costs no rebuild. WHY free, which is the part that makes it believable: the change drops an input that should never have been declared, so any shape without a host path in a macro body emits byte-identical output. THE PRICE IS CONFIGURATION A AND THE CONSUMER'S ROUTE IS B, so a sandbox-gated effect would read as free and would not be; the B witness is the consumer's server round. IN THE PIN since consumer `2049344` (2026-10-03 02:35) and WITNESSED ON B: the 04:19 editions round built `c-ares-1.34.8` (`vkchm36d`) on the drop-in through `fixupPhase`, and `deepened build dir` appears ZERO times in that round's log. The scanner reads string-macro BODIES, so `ares_private.h`'s `PATH_HOSTS "/etc/hosts"` and `PATH_RESOLV_CONF "/etc/resolv.conf"` arrived as absolute includes; declared, they spell as `../../../etc/hosts` - a climb past the root - and `nix-ninja-task`'s deepening guard read that as a cwd error, moved the working dir to `build/nnd0`, and ALL 35 real inputs then resolved one name too deep. Measured 37 inputs -> 35, `/etc` 2 -> 0, max climb 3 -> 2. The fix is `host_path_not_this_trees_to_carry` = `include.is_absolute() && !same_project_tree(...)` in the DISCOVERY path, reusing a predicate that already had a test and no caller. ONE HALF UNMEASURED: `discover_c_includes` has TWO callers (graphify+Serena found this) and only the static route was measured; the dynamic route (`discover_dynamic_dependencies`) reaches the guard with sound ordering but is unverified. WITNESS IS SERVER-ONLY: the c-ares drv (`c693mz3y` at `d4e82ca`, `vkchm36d` at `5f0fe07`; the hash moves with every bump) was in the `.#saxum-server` plan and in NEITHER desktop's, measured across all three, so a desktop round could never witness this class | `5f0fe07` | unit test `a_host_path_include_is_not_carried_and_cannot_outrun_the_build_dir`, mutation-checked (disabling the predicate fails it on `/etc/hosts must not be this tree's to carry`). `host-path-include-drv-repro.sh` (A, in run-all): drives `-t drv` on a LOCAL CMAKE CONFIGURE of the real c-ares source (from the store tarball by hash, no build, seconds) and reads `NIX_NINJA_INPUTS` on both drivers - head `etc=0 total=35 source=1`, pin `etc=2 total=37 source=1`, so the arms DIFFER. The control arm is MANDATORY (`NN_PIN_REV`, set by run-all) because the head arm alone cannot tell a fixed driver from a fixture that never reproduced the class, and this gate's first version proved exactly that: a SYNTHETIC fixture emitted no host input on EITHER driver, because the scanner declares the path only when it resolves to a file that exists, which the real tree's include chain supplies and a bare fixture does not. Mutation-checked: running the unfixed driver as head prints `etc=2` and FAILS. THE EARLIER REASON FOR "NO GATE" WAS WRONG and is kept here because it was stated twice: this row claimed the sibling gate "asserts `NIX_NINJA_INPUTS` while this changes `drv.inputs`". Measured, the host path lives in `NIX_NINJA_INPUTS` and `drv.inputs.drvs` is 0 - the sibling reads the RIGHT field, and the justification was manufactured |
| 6 | zxing-cpp, PCH read-back | verified on package | `79f634c` (key on `-include`, never `-Winvalid-pch`) | `second-run.sh` |
| 7 | openh264/glib, bare python script parent | mechanism verified; package unverified | `e9eb4c3` `58f31ac` `53d7d5a` `7331250` `bf25bda` | `openh264-bare-python` |
| 8 | openblas, sibling-dir include prefix | closed B | `a525636` (task binary) | |
| 9 | liblapack, configure-time sub-project dir | closed | `4e81766` `fabfbb9` | |
| 10 | svt-av1, scan upload canonicalize | UNWITNESSED in any round; retire; read the pending file before reopening | | none |
| 11 | krb5, `-o x.so` compile as link | closed | `f381c92` | `krb5-dotdot`, `krb5-objsuffix`, mutation-checked as a pair |
| 12 | p11-kit, `XML_CATALOG_FILES` | closed | `ee27295` | `p11kit-xml-catalog` |
| 13 | libssh/onetbb, failure naming no derivation | both build on B | `04b2857` | B full |
| 14 | brotli, alias copied as content | verified B; `bca4fb3` alone regressed 15 packages, keep the pair | `bca4fb3` `36d7158` `6015510` | `tests/alias_outputs.rs`, B full |
| 16 | onetbb, path composed in `-P` script | closed; second defect is class 19 | `7d6acc9` | `onetbb-alias-consumer` |
| | llvm/clang, `-P` script includes a sibling module by bare name | fixed by the PAIR; `992fd66` alone does not fix the package | `992fd66` `c72124c` | `llvm-vcs-include-b.sh` (B), `cmake-p-include-module-repro.sh` (A) |
| | curl-impersonate 2.1.1, a `cmake -P` script that no edge declares (ExternalProject's configure-written `<name>-mkdirs.cmake`) | fixed, free | `fe4e173` | `cmake-p-include-module-repro.sh` (A), second edge; control `992fd66` fails it |
| | shaderc, custom target cd's to the source dir above `<src>/build` | fixed (driver) | `19d92b9` | `cd-parent-script-repro.sh` (A), `cd-parent-script-b.sh` |
| | shaderc, `build-version.inc` written by a custom TARGET with no BYPRODUCTS | PACKAGE defect; the consumer's patch is the declaration | | none; reduction in the pending file |
| | compiler-rt, sanitizer `.syms` written as a SIDE EFFECT of an edge declaring only its stamp | PACKAGE defect, third BYPRODUCT member; boundary is the producing edge, not `install(FILES)`. `BYPRODUCTS $<TARGET_FILE>` fails at configure (not ordering); a composed plain path configures; install unmeasured; buys the ninja route only; a toolchain build, James's | | `cmake-byproducts-target-expression.sh` (no runner), `compiler-rt-syms-byproduct-configure.sh` (control `NN_CRT_UNPATCHED=1`); `local/pending/compiler-rt-syms-byproduct.md` |
| | glib, compile `-I` names a directory absent from the sandbox, `-Werror=missing-include-dirs` | fixed (driver); emptiness was never the mechanism, existence is the whole guard; closed on the package on B | `7aee41e` `132b372` `498dac3` | `unstaged-include-dir-source-tree-repro.sh`, `empty-include-dir-source-tree-repro.sh`, `empty-include-dir-compile-repro.sh` (all A) |
| | glib 2.88.3, a GENERATED header includes a GENERATED sibling undeclared to the CONSUMER target (`ABORTED` names the LINK) | PACKAGE defect, shaderc family; real ninja fails on the object alone. `a39c1f1` REVERTED at `b40abd0` (re-keyed every compile, closed nothing); an accommodation in `discover_dynamic_dependencies` is not proposed | | `generated-header-includes-generated-sibling-repro.sh` (A, RED BY DESIGN, excluded); `local/pending/generated-sibling-include.md` |
| | glib 2.88.3, a generator EXECS the compiler unnamed (`dtrace -G`) | fixed (driver): output-shape arm of `task_needs_cc` | `3ec8201` | `object-generator-execs-cc-repro.sh` (A); probe the command-line arm with a LINK |
| | liblapack 3.12.1, a staged shared library's `DT_NEEDED` is not an input | fixed, free; fires only where library and alias exist before the build tool runs; unreachable on the compiler route | `5235867` | `dt-needed-alias-repro.sh` (A, red at `37672a4`) |
| | glib 2.88.3, inputs listed inside a `.gresource.xml` | fixed (driver); gettext `.po` lists and Qt `.qrc` are the same unwritten shape | `143415f` | `gresource-manifest-repro.sh` (A), `gresource-manifest-b.sh` |
| | libepoxy 1.5.10, a build path with a SPACE | TWO driver defects under one shim defect (fixed there): space-joined encoded lists (`non-absolute store path`), then `$out` expanded unquoted where ninja shell-quotes it (gcc `cannot specify '-o' with '-c'`); both fixed (TASK BINARY, GLOBAL RE-KEY); the second was found by the gate's first run after the first was closed | `f241387` `81243e2` | `build-path-with-space-repro.sh` (A, in run-all; control `48bd866` names the shell split, an older control the entry split; real ninja is its fixture control) |
| | wayland-protocols 1.45, a generated object's path over the store name cap | fixed (driver only): names under budget are byte identical, over it folded with a digest over the ORIGINAL path | `d402c06` `59a3054` | unit tests, one against the real validator |
| | nss 3.112.5, compile against headers staged into its own output; edges also WRITE there | both halves fixed free, closed on B by fixture; unreachable on the compiler route. Next: a B run dies on gyp's `mtime_preserving_solink` (`command_program` returns `if`; six tool names needed on PATH; every gyp shared-library link), unwritten: `local/pending/nss-outer-output-write.md` | `435c1b1` `48a72e4` `eaafd31` `3e01bcf` `66d015b` `db17dca` `1dc5550` | `outer-output-header-b.sh` (B), `emission-price-outer-write.sh` |
| | clang 22.1.8, outer output matched as a SUBSTRING of a build-tree path | fixed, free (no priced shape carries it); by fixture, clang not re-driven | `ad78ec6` | unit tests |
| | llvm 22.1.8, an outer-output placeholder the COMPILER SPLIT survives the restore (`llvm-config --bindir` printed the placeholder; rustc's x.py then failed on a FileCheck path that does not exist) | fixed, driver only, priced FREE vs `5f0fe07` (task `drvPath` unmoved, five shapes identical). gcc 16 at `-march=znver4` emits a constant-length copy into a stack buffer as overlapping immediates, so the whole hash is absent and the byte swap misses it. Placement scans for any 8-char window of a placeholder hash (`placeholder_residue`), leaves the file unplaced, and the driver re-execs once with `NIX_NINJA_RAW_OUTER_PATHS`, the LTO arm for every task (on the cc route that is one TU). Measured windows surviving: 22/25 vector stores, 3/25 8-byte `movabs`. NOT COVERED (silent-failure review 2026-10-03): the scan sees only regular files placed by `symlink_derived_files`, so directory and alias outputs, depfiles and `-v` transcripts (`copy_derived_files`) are unscanned; `place_outer_stage_outputs` copied an output declared inside `$out` straight into it with NEITHER restore nor scan, the worst pre-existing gap: FIXED at `2973567` (`copy_restoring`), which restores through the same `rewrite_bytes` and refuses on the same `placeholder_residue` the placement path uses, and keeps the source's permission bits because a mode change inside `$out` moves the outer derivation's own output (the OPPOSITE choice from `local::write_restored`, which ORs `0o200` for a build tree). Priced FREE on all five shapes against `b886895` with three control arms passing and the task `drvPath` unmoved; B-WITNESSED WITH A FAILING CONTROL, which is what makes it more than a unit test: `outer-output-write-b.sh` grew a fourth arm staging an output whose content NAMES the outer output, scored on the bytes that landed in `$out` and never on the build succeeding. Head PASSES (`outer_stage_restored=1`, the real path in `$out`); control `NN_PROBE_REV=b886895` BUILT THE OUTPUT AND FAILED THE ARM, shipping `prefix=/nix/store/hjbdq8s5j5dkn2a110sy8...-nn-outer-output-write-0/lib` where the real output path belonged. THE BUILD SUCCEEDED ON BOTH SIDES, so an arm scored on exit status would have seen nothing, and that is exactly how this shipped for as long as it did. Its first three arms write content naming no outer output, so they ran the staging copy with nothing to swap and passed whether or not the restore worked; `outer_stage_restored=0` on them is what exposed that. THREE LIMITS, each measured rather than assumed: the re-exec in `cli::run` reads the error `symlink_derived_files` returns and this runs INSIDE the build from `handle_derivation_result`, so residue here ABORTS instead of recovering, and routing it to the re-exec waits for a package that produces one; a placeholder split into pieces shorter than `RESIDUE_WINDOW` slips the scan on this path exactly as on the other; and NO PACKAGE WITNESSES IT. That last one is a claim about the instrument, not about the package set: `OUTER_STAGE_DIR` is never printed on a success path, so grepping a round log for `.nn-outer` returns zero whether or not an edge staged an output, and it returned zero for both 2026-10-03 rounds. The sixth-shape pricer cannot price this class either, and says so: `emission-price-outer-write.sh` returns NO ANSWER because the subject edge aborts under `-t drv` and an empty document scores identical against anything, with its ordinary-output control identical at 1413 bytes; on the raw arm a header first found by the LOCAL discovery arm (`handle_derivation_result`), an outer-stage header, or an A-only memoised upload still reaches the task placeholdered, which the second run refuses naming those carriers; a 4-byte-immediate split. In the pin since consumer `429ffd5` (`c6040a0`), now `3122854` (`4ef51fd`). THE WHOLE-PATH HALF IS NOW WITNESSED ON A REAL PACKAGE, valgrind 3.27.1 at this pin, measured by the consumer 2026-10-03 and counted rather than eyeballed: the `out` stand-in appears 49 times in the round's commands and the other eleven stand-ins zero times, which is what makes those eleven zeros real rather than a dead pattern. All 49 are one define, `-DVG_LIBDIR="<out>/libexec/valgrind"`, against 365 invocations carrying the real path, so the arm fired on 11.8% of 414. A `-D` string lands in an OBJECT, objects are placed by `symlink_derived_files`, and the shipped `bin/valgrind` carries the REAL path at offset 0x4518 with exactly 1 occurrence and the stand-in form at 0, over five outputs and 388 files at rc 0. So the restore works end to end on a package, with its own built-in control. THE SPLIT HALF IS PAID, 2026-10-03 at pin `34f75f5`, on llvm 22.1.8 in the consumer's round and on the exact TU the fix was written for. Verbatim, right after `[3766/4961] Building CXX object tools/llvm-config/CMakeFiles/llvm-config.dir/llvm-config.cpp.o`: `nix-ninja: outer-output placeholder survived the restore in tools/llvm-config/CMakeFiles/llvm-config.dir/llvm-config.cpp.o (/nix/store/hjbdq8s5j5dkn2a110sy8zlhfcs4lxsx-llvm-22.1.8); rebuilding with real outer paths`. Counts over the whole round: healthy form WITH the `rebuilding` clause 1, `survived the restore` WITHOUT the clause 0, `placeholder reached the task` 0, `re-exec for real outer paths` 0. So the detector fired, the re-exec recovered, and nothing hit either defect form. ONE LINE IS THE CORRECT COUNT rather than a suspiciously low one: on the cc route one TU is one driver invocation and only llvm-config's TU carries an outer-output path, so exactly one re-exec is expected, and the price is that object's reuse ending at the next outer re-derivation. THE STAND-IN IS `hjbdq8s5j5dkn2a110sy8zlhfcs4lxsx`, byte for byte the `FAKE` constant in `placeholder_residue_tests`, and it is the placeholder for the output named `out` keyed on the NAME ALONE, which is why the same string appears in valgrind's 49 commands and in the write gate's failing control. THE LOAD-BEARING HALF IS ALSO PAID, 2026-10-03, and `llvm-config` IS IN THE `dev` OUTPUT, NOT `out` - both sessions armed watchers on `<out>/bin/llvm-config`, which holds `bugpoint`, `clang-offload-packager` and `count` and would never have produced it. Resolved through drv `rqvi62rc`, the binary is `xblg6js0sak03sf407gxr914xwfa9zz5-llvm-22.1.8-dev/bin/llvm-config`; the placeholder it PRINTS is `out`, the binary itself ships in `dev`. Scanned with SYMMETRIC windows: the `out` stand-in `hjbdq8s5` is 0 at 8, 9 and 12 characters, the real `out` hash is PRESENT as the placed control, `--bindir` prints `/nix/store/0dlrbmkw...-llvm-22.1.8/bin` and `--libdir` the real `-lib` path, receipt sha256 `a3fd624cf478d4a5`, 660,336 bytes. THE SPLIT IS VISIBLE IN THE ARTIFACT AND CONFIRMS THE MECHANISM: the real hash is contiguous only to 20 characters, breaks at 22, and the full store path never appears as one string, yet the binary prints it correctly because the pieces assemble into a buffer at runtime. That is exactly the gcc overlapping-immediates shape the byte-swap restore cannot reach, which is why the re-exec exists. THE FIRST ATTEMPT AT THIS SCAN WAS A BROKEN CONTROL and nearly reported FAIL on a passing artifact: an 8-character needle for the stand-in against the FULL 32-character hash for the real path, so the control read 0 because the real path is split too. Use the same window length on both arms. The retired form of this item read for that hash at zero occurrences with llvm's real `out` hash found as the placed control. The log says the mechanism worked; only the shipped binary says the artifact is right, and a stand-in in a COMMAND or an ENVIRONMENT is correct by construction (it rides every forwarded `PYTHONPATH`), so no log grep can ever pay this half: valgrind demonstrates a whole path being swapped, which is the case that always worked, and `llvm-config --bindir` is the case where gcc's overlapping immediates defeat the swap. Do not read the valgrind witness as closing the row. The scan's map uses std's SipHash: 83 MB/s on a 210 MB `libLLVM.so` (2.5 s per placement of a file that size), 588 MB/s with `FxHasher` (already a dependency); fixed on main at `c538753` (FxHash), with the response-file fix; both in the pin since `3122854` (`4ef51fd`) | `c6040a0` `c538753` | `split-placeholder-restore-repro.sh` (A, real one-task build, in run-all): control `5f0fe07` places 22 windows at rc 0, head falls back and places 0; a RESPONSE-FILE arm (control `c6040a0`, which kept the `.rsp` placeholdered on the raw arm and ABORTED, rc 1) places 0 on head; its first run was a FAKE PASS on an empty count, now NO ANSWER. Unit tests on the shipped byte shape, mutation-checked |
| | pkg-config 0.29.2, an outer-output placeholder inside a COMPRESSED debug section survives the restore and the scan | FIXED, PUSHED and PRICED `a9d5dd5`; found 2026-10-09 in the desktop round at pin `a7a5f4a`. A `-DLIBDIR` placeholder reached `.debug_info` of one glib object; every object's debug sections were zlib-compressed (`SHF_COMPRESSED`, data `78 9c`), so `rewrite_bytes` and `placeholder_residue`, both byte scans, saw nothing: no residue, no re-exec, the link shipped it and nn-audit caught it. Sibling of the slim-LTO carve-out (compressed IR) and of the split-immediate row above; the scan's NOT COVERED list did not name compressed sections. Where the compression is switched on is UNMEASURED (no `-gz` in the log). Fix: `compressed_sections` hand-parses the ELF64 header and each `Elf64_Chdr` (24 bytes) and inflates every `SHF_COMPRESSED` zlib section, so the existing `NIX_NINJA_RAW_OUTER_PATHS` re-exec recovers the TU. THE PRICE IS A GLOBAL RE-KEY AND IT IS THE ONE THE BOUNDARY ALREADY ACCEPTS: `miniz_oxide` is a direct dep of the driver, `Cargo.lock` is in the TASK fileset, so the task binary moved (`kjvk0q78` -> `kdnpllq7`); emission identical on all five shapes with three control arms passing, and the fix rides the SAME bump as the stable-cc-hook consumer side rather than paying a second. VERIFIED ON THE REAL FAILING OBJECT from the kept tree (`libcharset_la-localcharset.o`): raw scan `None`, full scan reports the placeholder. The fixture is a `gcc -g3 -O2 -gz=zlib` build whose macro lives only in `.debug_macro`, embedded as zlib+base64 in a `.rs` file because the driver's flake `src` globs `.rs` alone (an `.o` beside it is invisible to the nix build and `include_bytes!` fails there). Mutation-checked twice: removing the compressed scan, and a wrong `Elf64_Chdr` offset, both fail the test | `a9d5dd5` | `crates/nix-ninja/src/tests/zlib_debug_placeholder.rs`, mutation-checked as above; `local/pending/next-rekey-batch.md` 2026-10-09 |
| | compiler-rt 22.1.8, a `-Wl` linker script written by another edge | fixed, verified B by fixture; `e4cb09b` alone did not close it | `e4cb09b` `21767b0` `6a35958` | `version-script-generated-repro.sh` (A), `version-script-generated-b.sh` (B); the only instrument, a mutant hoisting the test survives |
| | meson shared library, `nm`/`readelf` absent from the emitted PATH | fixed (driver, EMISSION, re-keys every task); no-op on A | `9c2db59` | `nixos-test-nix-build-shared-lib` |
| | dav1d / dtc, a generated version header claimed by a `Built` output and an `Opaque` upload of the same bytes | fixed, free: dropped on the VARIANT; dav1d rerouted at the consumer | `34914f5` | `built-shadows-upload-repro.sh` (A; asserts WHICH claimant survived) |
| | libxkbcommon/pipewire/virglrenderer/gtk+3, one build path claims a REWRITTEN and a RAW upload of `config.h` | fixed, free; verified B by fixture; the `9f79161` pass NEVER RAN, the discovery merge is the site | `775dc12` | `outer-rewrite-two-claimants-repro.sh` (A, cannot reach the merge), unit test on the merge, `discovery-merge-two-claimants-b.sh` (B, the only instrument reaching the merge) |
| | gobject-introspection 1.86.0, a DIRECTORY argument with no python package | fixed (refusal, then carry whole under `DATA_DIR_CAP`) | `5e807ba` `17c03af` | `dir-arg-no-package-repro.sh` (A; asserts the generator's count) |
| 17 | libssh, quoted linker script path | verified B | `5485679` | `libssh-quoted-wl`, B full |
| 18 | double-named uploads, mount exhaustion | cause closed, unconfirmed | `2378d88` `adadddd` | `openblas-doublename` does NOT reproduce |
| 19 | openfec/dtc, co-outputs not placed | openfec verified B; onetbb is the UNDECLARED family, masked at the consumer | `93346c2` `b21cec5` `d27af3a` `f2dba47` | `openfec-co-output`, `second-pass-placement-repro.sh`, B full |
| 20 | xkeyboard-config, `python3 -m` | closed | `66b39a0` `a528e0a` `24978af` (`python_module_invocation`) | `python-dash-m-module` |
| 21 | llvm/rdma-core, directory seed in scanner | fixed (task binary); rdma-core builds on B | `7b60645` | `tests/directory_seed_scan.rs`, B full |
| 22 | nss, subninja scope | landed | `3d6dd36` | `argv0-scope` |
| 23 | mesa, `PYTHONPATH` forward | landed, B | `36eebaa` | |
| | gi stack, g-ir-scanner's include path and library fallback from the outer shell | fixed (driver, EMISSION, re-keys every task): `XDG_DATA_DIRS` and the gi fallback libpath (the silent half) forwarded together; `GI_TYPELIB_PATH` not | `6609727` | `forwarded-env-price.sh` (A) |
| | harfbuzz 13.2.1, the gir failure is CLOSED and a typelib abort remains | MEASURED ON B 2026-09-29 with the `acfd3d5` driver: `Gio-2.0` appears ZERO times, so `Couldn't find include 'Gio-2.0.gir'` (the `37672a4` failure) is gone as `6609727` predicted. WHAT STOPS IT is `ABORTED: build of ...^src-Harfbuzz-0.0.typelib failed`; the mechanism is UNMEASURED (the wrapper's leaf log was `not available`, so naming a cause from the abort text is a known trap). `hb-gobject-enums.h` is NOT the fatal line - that string is the scan's own fallback notice, printed while a candidate resolves - and harfbuzz is NOT a glib-sibling instance: its compile edge carries that header as an ORDER-ONLY dep (`|| src/hb-gobject-enums.h`), where glib 2.88.3 declares it nowhere. harfbuzz stays in the consumer's `ccPreferred` through a pin bump | | none; `local/pending/next-rekey-batch.md` 2026-09-29 |
| | tinysparql, a header FORCED with `-include` | fixed, free, TWO commits: `1806716` seeded the scan and the upload loop then skipped the seed as declared, so the task got the header's includes and not the header; `2833e54` keeps forced seeds out of the skip set (`declared_skip_set`, called by site and test). Compiler-route population ZERO (shim passthrough). The gate's five DaemonStalled runs were the daemon crash, not this class | `1806716` `2833e54` | `forced-include-source-header-repro.sh` (A, in run-all; PASS `2833e54`, control `28b453f`), `forced_include_wiring_tests` |
| | qemu 10.x, a configure-time header named ONLY by a `-D` macro value, reached through `-iquote` | fixed (TASK BINARY, GLOBAL RE-KEY): the scan seeds path-shaped defines from the command line and the preprocessor fallback forwards `-iquote`; the two defects met on one TU. Removed at the consumer (`hostCpuTargets = [ ]`), one qemu build on B owed before it comes back | `8a786a1` | `macro-named-generated-header-repro.sh` (A, control `48bd866`), three mutants caught |
| | which 2.25, a macro arm that resolves to a DIRECTORY or the root | FIXED, in the pin `d4e82ca` (2026-09-30): `macro_arm_headers` kept every string-valued macro arm that EXISTS, and which 2.25's `sys.h` defines `SLASHES`/`DEFAULT_HOMEDIR` once per platform whose non-Windows arm is `"/"` - absolute, existing, the root. The arm came back as a header, the include upload added `/` as a tree, and under recursive-nix the walk refused the socket at `/build/.nix-socket`, so every compile of the package failed with `add_to_store of "source" (nar of /)`. An arm is kept only if it resolves to a REGULAR FILE (`resolved.is_file()`); the root and directory arms drop out, the generated headers the reader exists for still come back. On the COMPILER route, so autotools never lists ninja and `ccPreferred` cannot move it. Test fails without the change, returning `/` and a sibling directory as headers | `d4e82ca` | `an_arm_naming_a_directory_is_not_returned` |
| | mesa-libclc 22.1.8.3, a macro arm spelled `<name.inc>` instead of `"name.inc"` | FIXED `8622e7a` (driver only, FREE): `macro_arm_headers` matched a double-quoted body only, so an ANGLE-bracket arm was dropped in silence. libclc defines `__CLC_BODY` as `<clc/shared/binary_decl_with_int_second_arg.inc>` in `clc_pown.h` and in each translation unit; the header was declared nowhere and every OpenCL compile died `clc/include/clc/math/gentype.inc:89:10: fatal error: 'clc/shared/binary_decl_with_int_second_arg.inc' file not found` on the `<scratch space>` expansion of `#include __CLC_BODY`. Both spellings now yield the body minus its delimiters; an empty body is still skipped by the `len() < 3` test. MEASURED ON THE FAILING TASK'S OWN 17 DECLARED INPUTS rather than a fixture: `__CLC_BODY` has exactly 2 arms from 2 define sites (`clc_pown.h` and `pwn.cl`), both angle-bracket, both resolving to files on disk and clang opening the first, so the existing two-arm gate was already satisfied and the quote test was the only blocker. Sibling of the `which` 2.25 directory arm and the qemu `-D` arm above, and the same family as `protoc`'s `-I` strip: a parser enumerating the forms it knows and dropping the next one in silence | `8622e7a` | `an_angle_bracket_macro_arm_is_returned` (fails against the unfixed reader with `got []`, passes after) and `an_angle_arm_is_resolved_and_an_absent_one_is_not_invented` (the negative control, which exercises the same resolution path so the first cannot be satisfied by accepting any bracketed token). A fixture-only arm could not have found this: the reader acts on a macro with TWO arms, so a one-define-site fixture never reaches the code under test |
| | mesa-libclc 22.1.8.3, the compiler refusal firing for a task that never invokes it | FIXED `240050a` (driver only, FREE), and the refusal now NAMES the command it refused (`4b3bd7d`, FREE). 514 tasks aborted with `` `cc` is not on PATH, and this task names a compiler or binutils program`` on commands that exec `$LIBCLC_CUSTOM_LLVM_TOOLS_BINARY_DIR/clang` by ABSOLUTE path and never invoke cc; `command_names_toolchain` strips the directory before matching, so the absolute path matched `clang` and chose the compiler closure (which is right, and stays), and it was the REFUSAL that had to stop firing, because nothing on such a line needs a PATH lookup. A bare name still refuses. THE SECOND COMMIT IS A DIAGNOSTIC AND IT EXISTS BECAUSE THE FIRST COST A ROUND: a refused edge emits NO DERIVATION, so `nix derivation show` finds no drv and `nix path-info` finds no output, and the only tasks a reader CAN inspect are ones that SUCCEEDED - which is the wrong reading, and both sides investigating a 32-task rustc/LLVM abort on 2026-10-13 read a healthy `llvm-min-tblgen` command from a different task because of it. `refused command:` now rides the `caused by:` chain. TWO CAUSES ARE NOW DISTINGUISHABLE BY THAT LINE and were not before: a command naming a known toolchain token is this row, a command naming none is `task_needs_cc`'s third arm (`object_shaped_outputs`) firing on the OUTPUT SHAPE with no regard for the command | `240050a` `4b3bd7d` | `an_absolute_toolchain_path_does_not_demand_a_path_resolved_cc` (asserts the OLD behaviour is wrong and failed before the fix existed) plus a negative control keeping a BARE toolchain name refusing; an earlier, broader version filtering every absolute token out of the name test broke `a_compiler_by_any_spelling_is_named` and was discarded, which is the existing suite doing its job. Both priced FREE with the three control arms firing before the verdict, task drvPath unmoved |
| | colord on B, a `g-ir-scanner --filelist` whose ENTRIES are undeclared sources | WRITTEN, in the pin `d4e82ca` (introduced `6f490ad`, an ancestor of the pin): the reader (`gir_filelist_invocation`, `gir_filelist_sources`, `gir_filelist_referenced_paths`) reads the SOURCE paths listed INSIDE the filelist the edge names. Was: the task had no `cd-client.c` and died `Invalid filelist entry-no such file or directory`. Same family as the ITS and swig readers (a file a command reads, declared nowhere) and a DIFFERENT member: the names are in a FILE the scanner opens, not in its argv. NOT yet witnessed on a real package round from this pin | `6f490ad` | `reader-wiring-drv-repro.sh` |
| | upb 33.6.0/36.2.0, a `protoc` edge whose `.proto` IMPORT siblings no edge declares | FIXED `16a3e01` (driver only, priced FREE vs `348e2cf`: task `drvPath` unmoved at `kdnpllq7`, emission byte-identical on all five shapes, three control arms passing). `protoc` resolves `import "..."` through its `-I` roots at runtime and no ninja edge declares the siblings, so the argv proto arrived and its imports did not: 119 `upbdefs` failures on KDE (protobuf 33.6.0, `protoc-gen-upbdefs`) plus 15 `upb_minitable` on XFCE (36.2.0) and 11 downstream upb-test compiles, at pin `348e2cf`. Verified from the kept build dirs: the import siblings are ABSENT from the sandboxed source view while the 89 protos some edge declared are present, so this is the SANDBOX hiding undeclared imports and NOT a `scan_seeds` gap - the consumer's first diagnosis named the wrong layer. Same undeclared-read family as the swig `%include` and the g-ir-scanner `--filelist` rows; a DIFFERENT member: the names are in a FILE the compiler reads, reached through the include roots rather than the argv. The reader (`protoc_invocation`, `proto_imports`, `protoc_referenced_paths`) resolves each import in protoc's own first-root-wins order, recurses the closure, carries only targets that exist, and emits relative spellings. A TRIGGER BUG WAS CAUGHT LIVE by the wiring gate before any package: the separated `-I .` (the form upb's edges carry) matched `strip_prefix("-I")` first with an empty value, so the reader resolved nothing while its unit test passed | `16a3e01` | `reader-wiring-drv-repro.sh` ARM 3 (A, in run-all): drives a protoc edge shaped like the failing derivation and asserts EVERY import form reaches `NIX_NINJA_INPUTS` (plain, `public`, `option`), pin control carrying none; verdict text moved with it, so the run-all pattern changed in the same commit. Plus three tests in `its_and_swig_reader_tests`. THE DISPATCH MUTATION SURVIVES THE UNIT TESTS, as it does for every reader here: the gate is the layer that catches it. B witness owed from the next desktop round. SECOND DEFECT, FIXED `526d293` (FREE vs `16a3e01`): `proto_imports` stripped only ` public` and ` weak`, so the `edition = "2024"` spelling `import option "..."` was skipped in silence and 162 KDE tasks failed at pin `16a3e01` (drv `cf8igx44`). Population in protobuf 36.2's own tree: `import "` 247, `import option "` 33, `import public "` 27, so the first fixture exercised two of three. The parser now skips any qualifier up to the opening quote; the gate arm carries an `import option` import | `local/pending/next-rekey-batch.md` 2026-10-10 |
| | curl-impersonate 2.1.1, an ExternalProject step writes `SOURCE_DIR` as an UNDECLARED side effect | FIXED `e1101af` (series from `b0dc02a`), B-witnessed by fixture with a failing control. NOT steps out of order: the download stamp was in the store; the extracted tree was discarded because a task stores declared outputs only. FIVE DEFECTS, each the next wall: (1) the download step now declares the tree, read from the `extract-<name>.cmake` CMake writes (`external_project_extract_dirs`); (2) that script is resolved against the step's leading `cd`, not the build dir; (3) the patch step runs INSIDE `SOURCE_DIR`, so it declares the tree too and the TASK BINARY copies a directory it both receives and declares, writable, rather than linking it (`refusing to patch`); (4) step edges are stamp edges (`external_project_step`) and the tree rides its stamp as a co-output via `BuildResult::undeclared_dirs`; (5) a later step's tree supersedes the earlier registration, which had handed the build step the UNPATCHED tree. PRICE: the task binary MOVED (by revision, the consumer's form: task `vf13fb88`, driver `wqwc69hz` at `e1101af`; a path-based eval of the working tree reads `9325i62i` and is NOT the pair to mount), but the consumer runs the knob (`/nn-task`, ABI 1) and a compile emitted under it is IDENTICAL at both revisions while the unknobbed control moves, so banked compiles keep their keys; the ABI is not bumped because only a task that receives and declares one directory behaves differently, and no banked task did. THE MAPPING MUST MOVE AT THE BUMP or `/nn-task` silently mounts the old binary. A reroute was offered for this row on 2026-10-10 and WITHDRAWN: it takes the package off resumability. Owed: curl-impersonate building in an XFCE round | `b0dc02a` `0922958` `362d801` `e657c80` `ba8ee4e` `e1101af` | `external-project-source-dir-b.sh` (B, in run-all): a real ExternalProject, store-path archive, patch, build step scored on the bytes it saw; head `marker=patched`, control `NN_PROBE_REV=526d293` FAIL. MUTATION-CHECKED against it: disabling the supersede gives `marker=original`, disabling the task binary's writable copy aborts the patch step; both FAIL. Unit tests `the_extract_scripts_source_dir_is_an_output`, `external_project_steps_are_stamps` | `local/pending/next-rekey-batch.md` 2026-10-11 |
| | curl-impersonate 2.1.1, an ExternalProject sub-build with no ninja on the task PATH and no `BINARY_DIR` declared | FIXED `34038d2` IN TWO COMMITS: the arms are `34038d2` and the `-B` guard is `e93c9da` (both driver-only, priced FREE) - **`34038d2` MUST NOT BE PINNED**, it carries the `-B` reader that declares a junk tree; `e93c9da` MUST NOT BE PINNED EITHER, its reader is UNGATED and fires on CMake's REGENERATE edge, so an ordinary cmake task declares the binary dir as its own output tree; pin `425778a` or later. THE `-B` GUARD, found by auditing the shipped commit rather than by review: the reader matched `-S` with `-B` as a CMake configure, but `-S` is ALSO gcc compile-to-assembly and `-B` is ALSO a gcc/linker search prefix, so `gcc -S -Bstatic a.c -o a.s` declared `static` as an output tree. It survived the unit test because that test's negative control gave a `-B` with NO `-S`, the combination that cannot reach the guard - a control whose input the subject cannot fail on is not a control. `e93c9da` requires the command to NAME CMAKE, a discriminator on the COMMAND (the make and gcc-ar precedent) rather than on the flag. The SIXTH and SEVENTH walls behind the SOURCE_DIR row above. The nghttp2 sub-build runs `cmake -GNinja`, and the emitted PATH carried no ninja: `which_store_path_opt` resolves through the OUTER PATH, where `ninja` answers `/usr/bin/ninja` outside the store, so it returns None, while the store ninja was already among the step's inputs (the `XDG_DATA_DIRS` value carries its `share/`). `ninja_input` asks each input whether it ships `bin/ninja` rather than matching a NAME, and the name is the trap: `contains("ninja-")` takes this driver's own `nix-ninja-0.1.0` and `nix-ninja-task-0.1.0` so the DRIVER would be handed over as the make program, and `starts_with("ninja-")` misses `python3.14-ninja-1.13.2`, the second ninja resident here; the file separates both real ninjas from the driver binaries and from the `-dist` metadata variant. The configure step also WRITES `BINARY_DIR` (`cmake -B deps/build/nghttp2`) and declared only its stamp, so the build step's `cmake --build` had no tree and `NIX_NINJA_MAKE_DIRS` listed the dir as create-empty: `external_project_binary_dir` reads the `-B` destination off the configure cmdline and rides the step chain as a co-output, the way `external_project_extract_dirs` already did for SOURCE_DIR (the module comment above `external_project_step` had named BOTH trees all along; only the source half was built). ONLY THE PATH ARM IS GATED ON `external_project_step`; the ORIGINAL text here claimed both were, and THAT CLAIM WAS FALSE - the binary-dir reader runs from `undeclared_outputs`, which every task with a cmdline reaches, and it fired on CMake's REGENERATE edge under an ordinary cmake package. GATED AT THE CALL by `425778a`. VERIFIED by driving `-t drv` on an EP fixture (the emitted drv carries `deps/build/nghttp2` in `NIX_NINJA_OUTPUTS` and the store ninja on PATH) with a NEGATIVE CONTROL: the same drive on a plain compile whose env carries the same `XDG_DATA_DIRS` puts NO ninja on PATH. The sub-build runs REAL ninja, so its objects are not banked per-TU - accepted bounded opacity, because the outer task stays banked whole and per-TU banking inside one vendored library would need a task class carrying the recursive-nix grant, the daemon socket and the driver binary; the upgrade trigger is that sub-build becoming repeated cost. B-WITNESSED 2026-10-12 with a FAILING CONTROL: `external-project-configure-b.sh` runs a real `cmake -GNinja -S <SOURCE_DIR> -B <BINARY_DIR>` configure inside an ExternalProject on configuration B; HEAD PASSES (configure declares BINARY_DIR=1, ninja-on-PATH=1, build-step carries BINARY_DIR=1) and the PIN control `6367b52` FAILS at BOTH walls (0/0/0, the configure leaf log carrying `CMAKE_MAKE_PROGRAM is not set`, the string the real drv died on). Owed: curl-impersonate building on B in a real round | `34038d2` | unit tests `the_configure_steps_binary_dir_is_an_output` (mutation-checked: disabling the reader fails it with an empty list) and `external_project_steps_are_stamps`; `external-project-configure-b.sh` (B, minutes, self-controlling, excluded from the corpus with its reason in `registered.sh`); `reader-wiring-drv-repro.sh` ARM 4 (A, in run-all: both arms fire at `-t drv` in seconds, pin as control) and ARM 5 (the guard for the class that shipped broken TWICE: an edge that is NOT a step, carrying CMake's own regenerate pair `cmake --regenerate-during-build -S<src> -B<build>`, must declare NO tree; head PASSES with `['build.ninja']` and the ungated `e93c9da` reads `['build.ninja','sub']` and would FAIL, both directions DRIVEN against real binaries rather than inferred; `-B` points UNDER the build dir deliberately, because a destination outside it is dropped by `relative_from` and a fixture with `-B` elsewhere cannot see the class); `local/pending/next-rekey-batch.md` 2026-10-12. THE HOSTILE AUDIT OF `34038d2` (rule 10) RETURNED SIX FINDINGS; F3 WAS THE FALSE GATE CLAIM, FIXED BY `425778a`, AND FOUR OTHERS REMAIN OPEN AND UNFIXED. The count reads FOUR, not five: this sentence said FIVE and enumerated four from the day it was written, an off-by-one nothing checked. `F1` was the loose `-S`/`-B` matching itself, CONFIRMED and closed by `e93c9da`; the false commit-message claim is `F3`, a DIFFERENT finding, and an earlier version of this sentence called F1 the commit-message claim by INFERENCE rather than by reading the audit. Listed so a reader does not take the arms as clean: `F2`: `names_cmake` is satisfied by any cmake-looking token, not by cmake being the PROGRAM, so `cmake -E env CC=gcc gcc -S -Bstatic a.c -o a.s` and `gcc -DCMAKE_COMMAND=<cmake> -S -Bstatic a.c -o a.s` both still declare `static`. REACHABILITY MEASURED: real, and unreachable in the packages on hand (the commands that would carry it do not occur). `F4`: `ninja_input` returns the FIRST input shipping `bin/ninja` and follows symlinks, so it can return the nix-ninja DROP-IN SHIM when both it and real ninja are inputs. `F5`: the `has_source`/`names_cmake` early return is a SILENT skip with no diagnostic, so an unguarded spelling and an unreadable command look identical, and because of F3 that silence is LOAD-BEARING - it is what suppresses the RERUN_CMAKE false positive. `F6`: the OTHER silent drops and fallbacks, THREE of them: a separated `-B` takes the next token blindly (`cmake -S x -B -GNinja` declares `-GNinja`), the `continue` arms inside the `-B` loop drop a destination without a word, and the doc comment and commit message assert gating the `-S`-with-`-B` pair does not have. An earlier version of this row SWAPPED F5 and F6 and described F6 by one of its three parts. Each needs a reproduction from a real command before the guard is tightened - a fix without a failing case is how the first version shipped |
| | libdatachannel/libvpl/editorconfig-core-c, an alias output read at its STORE path | OPEN, UNREDUCED; the reported mechanism is the DESIGNED alias state, so an outputs-into-one-path fix aims at correct code; what aborts is the include scanner's read (`retrieve_c_includes_checked`); which task is open | | none; kept build directory requested |
| | gtk+3, an encoded entry that is not an absolute store path | OPEN; two mechanisms print it: the libepoxy split (`f241387`, not in pin `37672a4`) or a build-dir-relative entry; the derivation name cannot discriminate, the RAW value or `build.ninja` line can | | none until the entry is read |
| 24 | meson-python, `-n` | FIXED (driver only, priced FREE vs `c538753`, task `drvPath` unmoved), on main at `4ef51fd`; A SECOND AND INDEPENDENT CAUSE OF THE SAME SYMPTOM WAS FIXED 2026-10-06 (`a7a5f4a`, priced FREE vs `df55697` on all five shapes with the task `drvPath` unmoved): the depfile gate required `deps = gcc`, so a rule naming a depfile with no `deps` line never had its depfile carried, and meson's cython rule is exactly that (`depfile = $out.dep`, command `cython -M ... -o $out` writes it). Real ninja reads a PLAIN depfile from disk on every check, so those edges read dirty FOREVER - `ninja explain: depfile '...pyx.c.dep' is missing` on all three of meson-python's `complex` fixture, `recorded 8/11 edge(s)`. The gate is the command's generation flag now. THIS ROW'S OWN GATE COULD NOT SEE IT: `ninja-state-dry-run-repro.sh` had only C compiles, and `c_COMPILER` DOES carry `deps = gcc`, so every depfile its fixture could produce was carried for another reason. It now contains a cython target and FAILS on `df55697`. A bounded re-key of cython-shaped tasks, not global, because such a task declares one more OUTPUT | IN THE PIN since consumer `3122854` (2026-10-03 12:33). **THE PACKAGE WITNESS CAME BACK SPLIT 2026-10-03 AND THIS ROW IS NOT CLOSED.** The WRITER half is witnessed on a real round: 37 `recorded N/M edge(s)` lines, every one N == M, largest 97/97, across 20-plus packages including meson-python's own 6. The CONSUMER half is witnessed FAILING at `34f75f5`: meson-python's three verbose `test_editable` variants fail the same `assert stdout.getvalue() == ''` as before (`3 failed, 135 passed, 12 skipped`, drv `qx8kk0l9`), printing meson-python's own `meson-python: building ...` line, so `_work_to_do` found work. DIAGNOSED, AND NOT A RECORDS DEFECT: `_editable.py`'s `_work_to_do` needs the exact bytes `ninja: no work to do.` on STDOUT; the shim correctly routes `-n` to REAL ninja; the driver wrote records; but `build.rs` collects only edges "whose outputs are ALL in the placed set" and its own comment says "`all` alone leaves every intermediate absent", and the flag's log line appears 28 times in that round and ZERO times for meson-python. So the intermediates are GENUINELY ABSENT and real ninja is right; no record can make an incomplete tree clean, which is `ninja_state` failing toward "work to do" as designed. THE REMAINING FIX IS THE SHIM'S PLAIN BRANCH AND IT IS APPLIED AND PROVEN, 2026-10-03: the consumer put `NIX_NINJA_MATERIALIZE_ALL=1` on both plain-branch execs of `nn-ninja-shim.sh` (the `test` and `install` branches already had it) committed at consumer `62bf17c` (it sat uncommitted in their working tree first; dated reading in `local/pending/next-rekey-batch.md` 2026-10-04), and `shim-plain-materializes-repro.sh` drives the REAL SHIM and scores it, flag ON reads `ninja: no work to do.`, flag OFF reads dirty, so the line is both the cause and the cure. That gate CONSTRUCTS BOTH ARMS rather than assuming the file's current state, because built the other way round it silently compared two identical copies the moment the fix landed. STILL OWED: the package itself, meson-python's three verbose `test_editable` variants passing on a relaunch; the mechanism is closed, the package is not. The cost the consumer accepted is in their comment and is not meson-python-only: every plain build of a routed package now places all intermediates. A consumer change whose price is theirs (hook content re-keys the cc route, and materializing every intermediate on every plain build is a real per-package placement cost). A driver default is argued against by `build.rs`'s own comment. meson-python is in `saxum-server`'s plan, so the edition ends nonzero while the drop-in leaves that tree incomplete. THE PACKAGE WITNESS IS OWED FROM THE NEXT ROUND THAT COMPLETES AND CONTAINS THE PACKAGE. The 2026-10-08 12:55 server round COMPLETED and cannot pay it: `meson-python` appears ZERO times in that build log and zero times in `.#saxum-server`, because the round was a 55-derivation toolchain rebuild plus a python/sphinx tail. So the round that completes is not by itself the round that witnesses; the owed witness is now the first round that both completes and contains meson-python, and no such round has run. The 2026-10-03 round was INTERRUPTED (`error: interrupted by the user`, static at 1,606,713 lines, llvm complete and clang at `[253/2308]`) and the 18:48 relaunch was REFUSED by the consumer's own resumability gate before building anything, for reasons unrelated to this row. Dated readings are in `local/pending/next-rekey-batch.md`; this line states the mechanism and deliberately names no round as pending. Cause: nix-ninja wrote no `.ninja_log` or `.ninja_deps`, and the consumer's shim routes `-n` to real ninja (`nn-ninja-shim.sh`, correct), which decides "no work to do" from those files and not from outputs existing; measured on ninja 1.13.2, a missing log dirties every edge and a missing deps log every `deps = gcc` edge (meson's compile rule). So meson-python's `_editable.py` `_work_to_do` read every driver-built tree as dirty and the three verbose `test_editable` variants failed (2026-10-03 rounds). Fix (`ninja_state`): after a local build places its outputs (the shim sets `NIX_NINJA_MATERIALIZE_ALL` ON ITS `test` AND `install` BRANCHES ONLY, NOT on the plain build branch an editable rebuild takes, so every intermediate is placed THERE AND NOT HERE - this clause read as unconditional and that is why this row was reported closed while the package failed), every non-phony edge whose outputs were ALL placed is recorded: log v7 with the rapidhash of `Edge::EvaluateCommand(true)` (command plus `;rspfile=` content), deps v4 from the placed depfile, every placed output stamped with ONE mtime because placement order is not dependency order. Each record restates only what ninja re-checks itself (`RecomputeOutputDirty`, `LoadDepsFromLog`), so an error reads DIRTY, never clean, PROVIDED TWO INPUTS ARE THIS RUN'S, which `4ef51fd` did not ensure and `34f75f5` does (both found by a silent-failure review): the stamp is the run's START read from the filesystem clock before any input is read (`mark_start`, as ninja records `command_start_time_`), not its end, so a source edited mid-run reads newer; and an edge whose depfile is older than that start is not recorded, since the depfile copy is best effort and leaves the previous run's file. Neither reaches a sandboxed build (no tree is reused or edited across runs). Only the default manifest is recorded (the cc drop-in's one-edge file is not); the build-dir walk skips both files. LIMITS: files are rewritten whole each run, so an edge an earlier run placed and this one did not reads dirty; n2 may not shell-escape `rspfile_content` where ninja does (a mismatch reads dirty). Earlier texts of this row said "SERVED by the consumer's PASSTHROUGH ... no driver change is owed" (never measured on a driver-built tree) and, before that, "the drop-in must serve it" (which produced a driver `-n` that printed no sentinel) | `4ef51fd` `34f75f5` `a7a5f4a` | `ninja-state-dry-run-repro.sh` (A, in run-all): a real meson project INCLUDING A CYTHON TARGET (the rule whose depfile the gate above refused, and without which this arm is structurally blind to it) built the shim's way, then real ninja: fresh clean, touched header dirty, head rebuild clean, control `c538753` dirty and `df55697` dirty with `depfile '...pyx.c.dep' is missing` against head's `recorded 7/7`; mutating the recorded command makes it FAIL (`command line changed`). `ninja-state-dry-run-b.sh` (B, in run-all): same inside recursive-nix, control `c538753` dirty with `deps ... are missing`. `rapidhash_matches_ninja`: seven hashes real ninja wrote, every length branch. `records_are_stamped_with_the_run_start_not_its_end` and `a_depfile_older_than_the_run_leaves_the_edge_unrecorded`, each mutation-checked |
| | libnvme, swig `%include` resolved against the CWD | WRITTEN `0a35a39`+`2b7bc55`; priced FREE against `2ab5e0a` on all five shapes. Emits `../libnvme/../src/nvme/types.h` where the edge carried only the `.i`. `nvme.i:1256` is `%include "../src/nvme/types.h"` (the file's LAST line; a capped `grep \| head` hides it). swig resolves against the CWD, not the `.i`'s directory - three arms: header at `<cwd>/src/nvme/` passes, at `<cwd>/libnvme/src/nvme/` fails, absent fails; and it COMPOSES with a task's climb, verified from a deeper cwd with `../libnvme/nvme.i`. The task declares `../libnvme/nvme.i` and no `types.h` in any spelling. Fix shape: read the `.i` for `%include`, declare each target against the cwd. Emission-side | | `reader-wiring-drv-repro.sh` (A, in run-all): drives `-t drv` and asserts the target IS in `NIX_NINJA_INPUTS`, with the PIN as control (carries `nvme.i`, not `types.h`); mutation-checked by removing the reader's dispatch, which drops it to 0. Plus `its_and_swig_reader_tests` for the trigger arms and the emitted spelling | `local/pending/next-rekey-batch.md` 2026-09-28 |
| | colord, msgfmt ITS lookup through the datadirs variable | WRITTEN `0a35a39`+`2b7bc55`; priced FREE against `2ab5e0a`. Emits `colord.loc` and `colord.its`, neither of which the edge carried. `i18n.merge_file(type: 'xml', data_dirs: ...)` is the only place the lookup directory is named - no positional argument and no `.its` string in any meson file. `msgfmthelper` only SETS `GETTEXTDATADIRS`; msgfmt does the lookup, and it needs `<datadirs>/its/*.loc` PLUS the `.its` the locating rule names (`colord.loc`: `pattern="*.iccprofile.xml"` -> `target="colord.its"`). Both checked in, neither declared. Three arms: real datadirs passes, a dir without `its/` fails, unset fails. A FOURTH ARM FAILS AND IT IS shared-mime-info: the reader emits `Path::new(datadir).join("its")`, which is RELATIVE for colord's build-dir `--datadirs` and survives the wiring filter, but ABSOLUTE for a STORE-PATH datadir and is then dropped by the `starts_with(store_dir)` test in silence. Both fixtures (the unit test's `/tmp`, the gate's build dir) model colord's spelling, so NO arm can differ on the store-path case. shared-mime-info is in the consumer's `ccPreferred` for `cannot locate ITS rules` and stays broken through any pin bump; a store-path input needs no upload, so the candidate fix is the `inputSrcs` route rather than a spelling one, UNTESTED: a reader emitting the store-path datadir that a shared-mime-info drive then builds would settle it | | `reader-wiring-drv-repro.sh` (A, in run-all): asserts BOTH `colord.loc` and `colord.its` are in `NIX_NINJA_INPUTS`, with the pin as control (carries the `.loc`, not the `.its`). Plus `its_and_swig_reader_tests` | `local/pending/next-rekey-batch.md` 2026-09-28 |
| 25 | glslang, a declared header materialised at its store path | FIXED (TASK BINARY, GLOBAL RE-KEY): headers placed as COPIES. A conjunction of FOUR: symlink placement; system-header status; includer reached as a QUOTED SIBLING; a build path LONG ENOUGH (likely why synthetic reproductions fail here). Open: the copy rule keys on SUFFIX, the mechanism on being an includer | `fab689a` `ea1abcc` | `placed-header-sibling-include-repro.sh` (A, SAFE geometries), `glslang-sibling-local-repro.sh` (A), `placed-header-sibling-include-b.sh` (B, the only one reaching the class) |
| | glslang 16.2.0, a declared output made by `create_symlink` after a mid-command `cd` in a LINK | fixed, free. Gap: no reader carries a BARE SCRIPT after a mid-command cd and `ensure_cd_target` (task binary) makes only a leading cd's target; batch candidate, no witness | `0232ed8` `4de5b57` | `post-build-symlink-output-repro.sh` (A, scores on the DECLARED SPELLING) |
| | gdbm/zstd/nasm/p11-kit/python3-minimal, a daemon refusal to register a realisation it already holds | **ROOT CAUSE PROVEN 2026-10-03, AND IT IS NOT IN THIS TREE.** The store holds a stale `BuildTraceV3` row naming a COLLECTED object; the next drive rebuilt that drv and the daemon refused the new registration against the dead row. THE EXPOSURE IS STRUCTURAL, NOT A COLLECT ONE-OFF: measured 2026-10-03, 2,044,902 trace rows against 1,942,333 whose outputPath is NOT in `ValidPaths` (95%) and 125,775 valid paths - about FIFTEEN stale rows per live object, because nothing in libstore deletes trace rows. VALIDATED ACROSS THE WHOLE ROUND, NOT FITTED TO ONE ABORT: all FIVE distinct (drv, output) pairs the round named have a drv still VALID and a trace outputPath NOT in `ValidPaths` - 5/5, no counterexample, so the mechanism predicts the round rather than explaining one case after the fact. The first query returns exactly the abort's `Local:` path `69cmkc1s`, and `nix path-info` says that path is NOT VALID with `ls` giving ENOENT. COLUMN TRAP: `BuildTraceV3.drvPath` is a BARE name while `outputPath` and `ValidPaths.path` carry `/nix/store/`; joining drvPath to `ValidPaths.path` matches NOTHING and reports every drv dead, which reads as the mechanism being wrong when the query is. The chain, each step read from nix: the output was collected while the drv stayed valid; `BuildTraceV3` HAS NO FOREIGN KEY TO `ValidPaths` AND NO CASCADE and `deleteBuildTraces` has ZERO callers in libstore, so the trace outlives the object; `derivation-goal.cc` then finds the trace, sees `!isValidPath(outputPath)` (PathStatus::Absent), REBUILDS, and if the bytes differ `registerDrvOutputUnchecked` (local-store.cc:692-699) throws with `Local:` naming the collected path. The guard is `isCompatibleWith`, whole body `outPath == other.outPath` (realisation.hh:88-91), keyed on `DrvOutput{drvPath,outputName}`. IT IS INTERMITTENT BECAUSE THE ABORT NEEDS FOUR THINGS TOGETHER: a trace row, that row's output path dead, the drv still buildable, and the REBUILT BYTES DIFFERING from the row's. The first two are 95% of rows, so cheap; the fourth is what makes it rare, and it means a REPRODUCIBLE task output NEVER ABORTS however stale the trace, because the new outPath equals the old and the daemon merges signatures in silence. REMEDIATION, MEASURED 2026-10-03: `nix store delete` on the DRV does NOT clear its row (gate fixture `s62gp1s5`: drv deleted, 1 row left), so it is not a remediation; `nix store repair` is untested. Deleting the rows whose output is dead removes the exposure: `delete from BuildTraceV3 where outputPath not in (select path from ValidPaths)`, 1,927,604 of 2,116,093 rows that day; nothing references `BuildTraceV3` but its own index, and a dead row only ever forces a rebuild or this refusal. Needs root and a STOPPED daemon. An instrument is made immune by a reproducible output instead: `p11kit-xml-catalog-repro.sh` aborted on this class because its man page embedded the date; the date is now pinned and the arm passes. **NO DRIVER CHANGE IS INDICATED.** TWO FIXES WERE DESIGNED AND BOTH FALSIFIED BEFORE LANDING: an injective output name (different drvs sharing a name realise fine - measured), and a client in-flight realise guard (the daemon serializes a floating CA build per `(drv, output)` with an on-disk lock held across workers, then rechecks and reports `skipping build ... someone beat us to it`, so a second requester REUSES and never registers; the client memo race is real and harmless). Pin→head is 123 added lines of c-ares predicate, so a bump does not clear it either | none - the driver did nothing wrong | `output-name-collision-repro.sh` (A, RED BY DESIGN, excluded) still asserts the one reproducible shape: two `-t drv` runs of one edge whose discovered input set differs emit two derivations declaring ONE name. `local/pending/next-rekey-batch.md` 2026-10-03 holds the proof and every falsified reading |
| 26 | a generator execs a tool by BARE NAME the emitted PATH lacks | ONE MEMBER CLOSED, FAMILY OPEN: `a343cbf` covers `gcc-ar`/`gcc-nm`/`gcc-ranlib`; zziplib (`tar`), wayland (`doxygen`) are outside that arm and `3ec8201`'s, routed at the consumer. Widening PATH is `9c2db59`'s bill (a batching question). Members are found only by driving real packages. qemu's regeneration edge (absolute path) is OUTSIDE the family. Count in the re-key record | `a343cbf` | none for the family |
| 27 | build-dir cache/growth re-keys tasks | closed, A and B | `5e0834d` `40fafcc` `037cf7f` `ff703fa` | `emission-determinism` (A, needs STABLE), `emission-b-sandbox` (B), `second-pass-placement-repro.sh` |
| 28 | `-t drv` on an aborting edge: empty stdout | MEASURED, not a driver defect: rc=1 with an `ABORTED` line on every abort path; a caller reading stdout alone sees an empty document, which scores identical against anything, so instruments branch on rc (`emission-price-outer-write.sh` does) | | ledger entry with the four drives |
| 29 | llvm-tblgen, `.td` closure | cause closed, unconfirmed | `37e658c` `addff59` | `tablegen-closure` |
| 30 | the make-dir list outgrows the kernel's per-string exec limit | FIXED (reader + emission), task binary MOVES so every banked plain task re-keys. `NIX_NINJA_MAKE_DIRS` accumulated one entry per make directory, so it scales with the PACKAGE rather than the command; libLLVMDemangle's archive edge (`gcc-ar` on `lib/libLLVMDemangle.a`) emitted 132,267 bytes against the fixed `MAX_ARG_STRLEN` of 131,072 and `execve` refused it with `Argument list too long` over four short argv entries. The eleven other variables in that derivation totalled 3,356 bytes, so the error names the argument list and the cause is one env string. Intermittent by construction: it crosses 128 KiB only on the largest subjects, so it reads as a random failure; that drv had survived 11,485 tasks of its own package first. `passAsFile` is nix's mechanism for an attr too large to pass and the other two encoded lists already rode it; the make-dir list now joins them and the reader takes either spelling. ITS NAME IS APPENDED ONLY WHERE MAKE DIRS EXIST, which is not tidiness: `passAsFile` is emitted into EVERY task derivation, so an unconditional name re-keys every banked compile. Priced both ways, measured: unconditional moved all five shapes (`EMISSION prog: MOVED 2078 -> 2098`); conditional leaves all five BYTE IDENTICAL. The residual re-key is the task binary alone, whose reader changed and which is inside the fileset allowlist. THE MAPPING IS INERT ON THIS ROUTE and a stale one does NOT mount an old binary: the failing drv's `builder` is the store path with `NIX_NINJA_TASK_ABI` unset, nothing sets `NIX_NINJA_TASK_BUILDER`, and `/nn-task` appears nowhere in the consumer, so the `extra-sandbox-paths` entry mounts a path no task references. A stale mapping here is decoration, not a hazard, and the comment says so rather than sending a reader to a restart | `731d908` | `make-dirs-pass-as-file-drv-repro.sh` (A, in run-all, `-t drv`): a task with make dirs names the list in `passAsFile`, a task without names it nowhere, and (own build dir, `df55697`) the alias list rides it too while an empty include dir with a space in its path is carried; control `fc3985e` carries both lists inline and drops the spaced dir; mutation-checked (an unconditional append FAILS the second half). `empty-include-dir-compile-repro.sh` witnesses the task READING it from the file, and cannot see a slide back to inline (the reader takes both). `emission-price.sh` (three control arms passing BEFORE any price printed) prices it. NO PACKAGE WITNESS: the 128 KiB size is reached by no arm, and the owed witness is libLLVMDemangle's archive edge building on B |
| | x265, symlinked archive skipped by walk | verified | `build_dir_disposition` | `x265-symlink` |
| | bison, symlinked products rebuild every pass | closed | `local.rs` mtime refresh | `mtime-rebuild` |
| | linux-pam, XInclude closure | closed | `806b268` | |
| | brotli, installed RUNPATH carries a task output; structured-attrs outputs | fixed, verified B; the structured half bites only where stdenv exports `outputs` (the consumer's pin does) | `aade344` (branch) | B full |
| | a later driver pass adopts placed products as inputs | closed on three routes; the consumer's route avoids it via `dontUseNinjaCheck=1` | `f2dba47` | `second-pass-placement-repro.sh` (A, three passes), B full |
| | rdma-core, generator writes into a directory that exists EMPTY | fixed (task binary MOVES) | `2c0b429` | `empty-build-dir-directory-repro.sh` (A) |
| | rdma-core, configure-time link with an ABSOLUTE in-project target | fixed (`AliasAndUpload`) | `c5cf7c4` `7cb0877` `5d056b6` | `x265-symlink` |
| | rdma-core, man page `.. include::` fragments | fixed: `rst_invocation` + `rst_include_closure` | `afdfa7c` | unit test; rdma-core on B |
| | xkeyboard-config, source marked `build <file>: phony` with no inputs | fixed; `all: phony` over no file is still an alias | `71fdb0b` | `phony-source-input-repro.sh` |
| | xkeyboard-config, generator reads data by its own location, then `is_symlink()` on placed inputs | OPEN, package side; keyboard data is under no copy rule; whether the list should grow is open | | none |
| | rdma-core on the drop-in, header through an ABSOLUTE link with an absolute `-I` | fixed (task binary) | `cf13f43` | `shim-link-include-b.sh` (B) |
| | raptor2, declared output never written | package defect; the task binary fails "declared output X was not written by the command" | `41de370` | `declared-output-unwritten-repro.sh` (A) |
| | doxygen, custom target BYPRODUCT never written on a first run | fixed (task binary): a custom target's absent byproduct is created empty; `add_custom_command` OUTPUT stays loud | `eb9d9e2` | `declared-output-unwritten-repro.sh` (both halves) |
| | meson-python, editable wheel rebuilt in place | MEASURED 2026-10-03 from the suite's own log (04:19 round, pin `5f0fe07`): `3 failed, 135 passed`, and all three FAILED lines are the `stdout == ''` assertion on the `meson-python: building` line, which is row 24's mechanism (fixed `4ef51fd`). The stale-value-after-edit half did NOT fail there: `test_editable_rebuild`'s non-verbose variants, which edit a source and re-import, passed. No longer opted out (the package ran in that round). The 2026-09-05 reading blamed the shim's banner for the stdout half; the round shows it was the driver's missing ninja state | `4ef51fd` | `source-edit-replaces-product.sh` (A, pins behavior), row 24's gates |
| | dtc `.dtb` written by compiled generator | declared outputs produced under B; undeclared ones need declaring | | `generator-edge-execution-b.sh` |
| | drop-in driver waits forever in a wedged daemon | fixed (driver): every daemon exchange bounded | `3301b15` | `bounded_exchange_tests` |
| | libid3tag/openal-soft, a generated DATA file a TU includes, reached only through a `cmake_object_order_depends_*` phony | CARRIED at `2ab5e0a`, which merged `nn-driver-compile-artifact` into `main`; the branch alone no longer exists as a lineage. Fix `3b2efcc` (branch). Still LIVE on the pinned tree: `genre.dat` and `default_hrtf.txt` both fail at `37672a4` | `3b2efcc` (branch, merged) | none; `local/pending/next-rekey-batch.md` 2026-09-27 |
| | mesa 26.2.1 / gperftools, a macro defined once per `#if DETECT_ARCH_*` arm keeps one value, so the arm the compiler takes is not the one declared | `04539e8` (`deps-infer`, FILESET) and `e4b2223` (driver) implement the SAME predicate twice, one per crate. Both are CARRIED at `2ab5e0a`; `04539e8` is the one on the path both routes run, since the driver depends on `deps-infer` and calls its scanner | `04539e8`, `e4b2223` (both merged) | `mesa-macro-arm-repro.sh` (A, in run-all): the pin FAILS on the arm the compiler takes and both lineages PASS with the build-dir walk off, so the fix reaches the cc route. The B gate is version-mismatched and cannot instrument this row. Live at `37672a4`: `glapi/entry_x86_tls.h`, `stacktrace_libgcc-inl.h` |

| | the cc drop-in GRANTED to a build that never used it | NOT A DRIVER DEFECT and not a driver-detectable one: the shim is `~/Projects/saxum`'s file. rustc 1.98.1 on 2026-10-08 carried `recursive-nix` and the hook, printed `nn-cc: drop-in active`, and sent all 1546 compiles to the real cc, because cargo's cc crate spells `-o` ABSOLUTE and the shim's `passthrough absolute-o` arm execs the real compiler. The banner is printed by the HOOK, so it says the hook was installed, never that the shim intercepted. The consumer's resumability gate scored it resumable on the GRANT (`recursive-nix` in `requiredSystemFeatures`, `classify` in `scripts/refuse-unresumable.py`) and never read the USE. THE DRIVER'S OWN LIST IS THE HONEST SIGNAL and needs no shim change: an intercepted compile writes one one-edge manifest under `$NIX_BUILD_TOP/.nn-cc/*.ninja` before handing off, a passed-through one writes none, so the count of those files IS the intercepted count. Zero with the hook present is the false-resumable case | none in this tree | `shim-intercept-count-repro.sh` (A, in run-all): the real shim run from a copy with stub compiler and stub driver, three arms - relative `-o` intercepts (1 manifest, driver ran, real cc did not), absolute `-o` and the cc crate's argv shape pass through (0 manifests, real cc ran). The two control arms must DIFFER or the verdict is NO ANSWER; each arm also requires that SOME program ran, because a count of zero from a harness that ran nothing reads exactly like a passthrough (this gate's first draft did that). `--count <retained build top>` reads a real build dir: on the retained rustc tree it reads `intercepted=0 objects=1546` -> FAIL. `round-intercept-sweep.sh` (A, in run-all, `--self-test`) asks the same across a ROUND: per failed drv it prints grant, banner, intercepted count and verdict, and prefers the shim's own summary line (`nn-cc: N intercepted, M passed through`, consumer `55b356d`) over inferring from `nix-ninja:` lines; `--built` scans the drvs a round built. Its first two versions were wrong in ways its own controls caught: it called every never-run build NO-LOG, then swept rustc - the build with the answer - into DEP-FAILED by matching any `Reason:`, then read meson's install phase (`removed 'test cases/.../prog.c'`) as a COMPILE and turned a pure-python package into a false GRANTED-UNUSED. The compile predicate is a compiler INVOCATION, not a filename. Its reading function was inline and unreachable from the self-test at first, and a mutation ignoring the shim count SURVIVED the self-test; it is now `intercept_reading` with four arms and the same mutation fails `count-wins` (`0 yes`, want `7 yes`) |
| | nlohmann_json 3.12.0, the shim delegates a tool but composes its own argv | CONSUMER-SIDE, the shim's file; found 2026-10-10 in the KDE round at `fe4e173`. The test branch (`scripts/nn-ninja-shim.sh:156`) runs `ctest --output-on-failure` and drops the caller's flags, so nixpkgs' `checkFlagsArray+=("ARGS=-LE 'not_reproducible|git_required'")` (`package.nix:59`) never reaches ctest and the four excluded tests run and fail (`96% tests passed, 4 tests failed out of 100`; all four carry BOTH labels). THE FIX IS NOT A `"$@"` PASSTHROUGH and that is the part worth keeping: nixpkgs' checkPhase is `ctestCheckHook`, whose flags are SHELL VARIABLES, never argv - the hook builds `flagsArray=(-j$buildCores --output-on-failure)`, then `concatTo flagsArray ctestFlags checkFlags checkFlagsArray`, then `ctest "${flagsArray[@]}"`. The shim is invoked as `ninja test`, so it sees the target word and nothing else and there is no caller argument to forward; a passthrough would be a no-op that reads as a fix. The correct shape reproduces the hook's composition: the three flag variables (`concatTo` splits each on whitespace, so `ARGS=-LE 'not_reproducible|git_required'` becomes TWO argv entries and ctest's `-LE` is what excludes; `ARGS=` itself is not a ctest flag and rides through as an entry ctest ignores), `disabledTests` as `--exclude-from-file`, plus `-j$NIX_BUILD_CORES --output-on-failure`. THE MESON BRANCH IS THE SAME CLASS (`:159`, `meson test --no-rebuild -C "$build_dir"`, also no flags), and the shim's own comment already records a third instance (`ninja -t query`): the class is "the shim delegates a tool but composes its own argv", so grep for the rest rather than fixing one. NOT A DRIVER DEFECT and not driver-detectable; the shim is `~/Projects/saxum`'s file with no copy in this fork, and moving it here is James's call (hook content, so a move re-keys the cc route) | none in this tree | none; the fixture shape is a CMake project with one `git_required`-labelled test and a check phase setting `checkFlagsArray+=("ARGS=-LE git_required")`, failing when the flags are dropped and passing when they are passed - an arm asserting only that ctest ran would pass either way. `local/pending/next-rekey-batch.md` 2026-10-10 |
| | the desktop editions' unresumable residual | CONSUMER-SIDE, and it is what stops `.#saxum-kde` and `.#saxum-xfce` before iter 1: the consumer's `refuse-unresumable.py` refuses the whole edition while any derivation lacks a resumability mechanism, so a REFUSAL is not a build failure and the round's `retry loop rc 0` is the refusal returning cleanly. Measured 2026-10-08 at consumer `55b356d` by importing their `classify()` read-only (`--max-jobs 0`, nothing built), FULL per-class lists rather than the gate's four-name stderr sample: kde 4700 paths, 7 residual; xfce 6102 paths, 37. THE LIST SPLITS BY WHAT CLEARING IT COSTS, which the flat count hides: 14 `make-or-other` members are plain second-set instances (no features at all, plain `stdenv-linux`) in both editions (libxt, libx11, freetype, yasm, fftw x2, jbigkit, spandsp, colm, dbus-cplusplus, source-highlight, guile, netpbm, nspr), minutes each; 3 are hour-scale serial toolchain builds (gfortran-16.2.0 - the one member carrying `gccarch-znver4`, i.e. the MAIN set - plus ffmpeg-headless and gdb); 6 are EDGE ROUTES needing no build, every one traced with `nix why-depends`: nss, gstreamer and librsvg reach the plan through `brave-origin-nightly` (a prebuilt `.deb` repack with NO features), rusty-v8 and esbuild through `mpv-with-scripts > yt-dlp > deno`, clang-21.1.8 through `xdg-desktop-portal-luminous > rust-bindgen-hook`. THE EDGE PACKAGES ARE THEMSELVES UNWIRED ROOTS, so routing the PACKAGE cannot work and the fix is at the edge (route the consumer to a wired instance) or an explicit acceptance; the rest are cargo/go/cmake-without-ninja, for which the gate's own message says no per-TU path exists and the residual is James's to authorize. THE TOOLCHAIN ROUTE WAS MEASURED AND WITHDRAWN 2026-10-09: turning `nnWireToolchain = true` on takes the gate from 5 refusals to 1594 (1323 `make-or-other`, 154 cargo, 98 `ninja-present-but-unwired`, 13 `cmake-without-ninja`, 6 go), because the class rule wires only packages that EMIT ninja and the re-keyed plan is mostly autotools. So wiring the toolchain does not clear this residual - it re-keys the compiler and drags the desktop closure in behind a rule that cannot drive it - and gfortran joins the named acceptance rather than being a target. Do not re-propose the flag as the way to clear the list. TWO TRAPS IN READING THIS, both hit: `nix-store -q --referrers` returns NOTHING for these drvs (it sees registered paths only) and reads as no dependency, so trace DOWN from the system derivation with `nix why-depends --derivation`; and the store holds SEVERAL INSTANCES PER NAME, so globbing `/nix/store/*-libxt-1.3.1.drv` picks one carrying `recursive-nix gccarch-znver4` and reads as already-wired - read the drv HASH the gate named | none in this tree | none; `local/pending/next-rekey-batch.md` 2026-10-08 holds both editions' full lists, the traces and the per-class prices; the 2026-10-09 withdrawal is in the same file |
| | the LTO warning names the outer paths it keeps | EXPECTED on the raw arm, not a failure: an LTO task must see the real outer paths, so its inputs are re-uploaded raw and its output keys on the real `$out`, which stops being reusable when the outer output moves. The line carries a COUNT alone before `babb449` and a SORTED, SPACE-SEPARATED PATH LIST after the trailing `": "` from that commit; the consumer's `refuse-unresumable.py` classifies per path from that list rather than refusing a whole family on the number. A count-only line means the driver predates `babb449`, which is a pin question, not a driver defect | `babb449` | no gate; it is a logging-contract line, read by `local/claude-md-detail.md`'s error-signature section, and the path list is data for the consumer's classifier |
| | umockdev 0.19.3, a `valac --pkg` resolved through `--vapidir` to a file in the package's own tarball | FIXED `b0e69e5` (driver only): `valac` asks for a Vala package through `--vapidir` plus `--pkg` and resolves it to `<name>.vapi` in one of those roots. The command lists three vapis BY PATH (`src/selinux.vapi`, `src/ioctl.vapi`, `src/linux_fixes.vapi`, all carried) and reaches a fourth only through `--vapidir=/build/umockdev-0.19.3/src --pkg libpcap`, so `libpcap.vapi` appears on no command line and no edge declares it. IT IS IN THE PACKAGE'S OWN SOURCE: `tar tf .../umockdev-0.19.3.tar.xz \| grep vapi` lists `umockdev-0.19.3/src/libpcap.vapi`, confirmed by the consumer session. The sandbox never staged it and the run died `Package 'libpcap' not found in specified Vala API directories or GObject-Introspection GIR directories`. MEASURED SCALE: 610 of 842 `Cannot build` blocks over ten iterations of one desktop round are this SINGLE derivation, with the closure behind it (pipewire, gobject-introspection, gtk+3, qtbase, networkmanager, wayland, vulkan-loader, gst-plugins-base, libnice, libcanberra, pygobject, dbus-python, graphviz, sphinx) failing as a consequence. `libpcap` IS a declared input of the umockdev build, so an input-set gate would over-fire and is the wrong instrument. THE ROOT IS BELOW THE FAILURE LINE: a sweep matching `Cannot build '<pkg>'` returns one row per package and read this as twenty defects; the cause is in the `Last 6 log lines` block UNDER it. Same family as the protoc `-I`, the swig `%include` and the g-ir-scanner `--filelist` readers | `b0e69e5` `34a9cb9` | `a_valac_package_is_resolved_against_its_vapidir` and `a_valac_package_that_resolves_is_carried_and_one_that_does_not_is_dropped`, the second carrying a negative control on the same argv with the vapidir pointed where the file is not; disabling the existence guard makes it FAIL. DRIVER-ONLY, MEASURED: the task `drvPath` is identical at `b5758b8`, `b0e69e5` and `ab1de1f`; what moves is the DRIVER, which re-keys dynamic task derivations |
| | colord / libgweather / accountsservice, `g-ir-scanner` runs `ldd` by BARE NAME | FIXED `ab1de1f` (driver, EMISSION, re-keys every task's emitted PATH): `giscanner/shlibs.py::_resolve_non_libtool` calls `subprocess.check_output(['ldd', ...])` to resolve a library's dependencies, so a task with no `ldd` on PATH dies `FileNotFoundError: [Errno 2] No such file or directory: 'ldd'` before it reads anything. IT SITS UPSTREAM OF THE WHOLE INTROSPECTION TOOLCHAIN: colord's `lib-colord-colord.vapi` and libgweather's `libgweather/GWeather-4.0.gir` are the SAME failure from the same line, with accountsservice's `AccountsService-1.0.gir` behind them. `ldd` is not a binary from binutils - it is glibc's, in the `-bin` output - and it is a shell script that drives the dynamic loader, so the mount must supply it rather than the task merely naming it. It is the `SCRIPT_TOOLS` `DEFER` TRIGGER FIRING, which is worded `DEFER(a round failure naming a tool absent from this list)` and was written for exactly this case | `ab1de1f` | none yet; the consumer's round at the new pin is the witness. Listed here rather than left uncited because the DEFER trigger is what closed it and its wording is the authority for the class |
| | mesa-libclc 22.1.8.3, a macro whose SINGLE arm is an angle-bracket include | FIXED `7227990` (driver glob, task binary UNMOVED): the angle-bracket reader skipped any macro with `paths.len() < 2`, reasoning that one arm is the ordinary case the walk already resolved. TRUE FOR A QUOTED ARM, FALSE FOR AN ANGLE ONE. `clc_fabs.h:12` is `#define __CLC_BODY <clc/math/unary_decl.inc>`, its ONLY define site, and an angle include resolves on a search path no edge declares, so nothing resolved it and the task died `gentype.inc:89:10: fatal error: 'clc/math/unary_decl.inc' file not found` on `#include __CLC_BODY` - the same file-not-found the quoted spelling gave before `8622e7a`. THE DELIMITER DECIDES NOW, NOT THE ARM COUNT, and each arm remembers which it was because the bare path has already been stripped of its delimiters by the time the gate runs. A single quoted arm is STILL skipped (its own control holds that line); a single angle arm is carried only when it resolves to a regular file, the same `is_file` test that drops a directory arm and which 2.25's root. THE FIXTURE FOR THE EARLIER FIX BUILT TWO ARMS - its own comment says so ("each `clc_*.h` names a different `.inc`, so the reader sees two arms") - so it could not see this, which is the same class the row above records: a control written to the shape that passes rather than the shape that fails | `7227990` | `a_macro_with_one_define_site_still_names_its_arm` builds the ONE-site shape production has and FAILS against the previous reader with `got []`; `a_single_arm_macro_adds_nothing` keeps the quoted arm skipped, which is the pair a control needs. 346 tests, clippy and the pinned formatter clean. ONE AXIS REMAINS on this package and it is the row below: these two commits fix WHICH ARM SPELLING the reader accepts, and neither makes it read the translation unit's own body, which is where `unary_def.inc`'s defines live |
| | mesa-libclc 22.1.8.3, a macro arm the reader cannot resolve, TWO SHAPES | **BOTH SHAPES FIXED, `6993506` (the include list) and `f8d0abe` (the unit's own body), driver only, EACH priced FREE on all five shapes with the three control arms firing before the verdict, task `drvPath` UNMOVED at `3vnxcibcqqfin9v1b636fjm3rr3rsmk3`.** THE TWO ARE INDEPENDENT and were LANDED IN ONE PASS because fixing A UNMASKS B rather than closing the package. SHAPE A, the include list arriving EMPTY, MEASURED from the failing task's own derivation `args` (`00s871wf4mwnsyf9m1q15ywiwpsl65px-ninja-build.drv`, parent `fzsash3cjn2vjb4addvnshlhjrhw2dcp-mesa-libclc-22.1.8.3.drv`): the command is `cd /build/source/build && clang ... -I/build/source/clc/include -I/build/source/clc/lib/generic/math ...`, so the build dir is `/build/source/build` and BOTH `-I` directories sit ABOVE it. `include_dirs_named` answers a DIFFERENT QUESTION - it keeps an absolute entry only where it falls under the build dir and drops the rest, right for a directory a task must CREATE and wrong for one a reader must RESOLVE - so the arm reader was handed an EMPTY list, tried `build_dir.join(<arm>)`, found no file, dropped the arm in silence. The task's input set corroborated it: the drv declared `../clc/include/clc/integer/clc_clz.h` and its `gentype.inc` (LITERAL includes, the scanner's own flag reader) and NOT `clc/shared/unary_decl.inc` (a MACRO operand, this reader through the other one): two readers of ONE flag set, diverging. The fix hands the reader `include_dirs_as_given` with an absolute entry kept as a RELATIVE CLIMB (`../clc/include`) - neither dropped nor absolutised, because an absolute spelling is then dropped by the wiring filters in silence. 13 tasks died on it. SHAPE B, the define site in the unit's OWN BODY: `declaring` was drawn from `c_includes`, a unit's INCLUDES, so a macro defined only in the unit's text reached the reader at no point. `clc_native_log.cl:14` carries `#define __CLC_BODY <clc/shared/unary_def.inc>` in its body, its includes (`clc/internal/clc.h`, `clc/math/gentype.inc`) define nothing, and `clc/shared/unary_def.inc` is named by NO header anywhere in the tree (99 define sites, ALL `.cl`, ZERO `.h`); MEASURED, 27 of the package's `clc/lib/generic` units carry the shape. The fix adds the unit - already in `files`, the declared-input seed set - to the set the reader scans for defines, existing files only and properly deduped against the include list. THESE TWO ARE THE THIRD AND FOURTH AXES on this package: THE TWO MACRO-ARM ROWS ABOVE ARE THE DELIMITER AND THE ARM COUNT, and all four are real fixes at four different layers. Sibling of the whole undeclared-read family: a file a command reads that nothing declares | `6993506` `f8d0abe` | `an_arm_above_the_build_dir_resolves_through_the_include_chain` (`6993506`) asserts the FIXED reading with the NEGATIVE CONTROL that must fail - the same header handed the `include_dirs_named` list (still empty) resolves nothing, and every emitted path must be RELATIVE or the wiring drops it; it CHDIRs to the build dir because the reader resolves a relative include dir against the process cwd. `a_define_in_the_translation_units_own_body_names_its_arm` (`f8d0abe`) is shape B's, with the same shape of control: includes alone leave the arm invisible, adding the unit brings it back. The suite's `include_dirs_named_tests` keeps the drop rule pinned for the task-creation caller | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | mesa-libclc 22.1.8.3, a command that WRITES a path the task placed READ-ONLY | **FIXED `239d6b1` (TASK BINARY, priced FREE UNDER THE KNOB: control MOVED, ABI sensitivity MOVED, subject IDENTICAL).** THE ABORT'S CAUSE, and it is not the compiler and not a race: CMake declares a compile's transformed depfile `CMakeFiles/d/<hash>.d` as BOTH an INPUT of the edge and one of its OUTPUTS, so `create_symlinks` placed it as a symlink into the 0444 store, and the command's own second half - `cmake -E cmake_transform_depfile Ninja gccdepfile ... <raw .d> <CMakeFiles/d/<hash>.d>` - must WRITE it. Writing through a 0444 link fails, and cmake exits **2 WITH NO OUTPUT AT ALL**, so `nix-ninja-task` propagated that code over a FOUR-LINE log and the failure read as a reader defect or a race. `NIX_NINJA_VERBOSE_LOG=1` (4414 lines) showed no compiler text for exactly this reason: the child printed nothing, so there was nothing to capture. PROVEN BY DIFFERENCE in the kept tree of an ISOLATED single-drv failure (`2fzha93flybcg1ih8iz1pziiq8vvnsxg-ninja-build.drv`, kept at `/nix/var/nix/builds/nix-16967-1529411022/build`): the exact `cmake -E` returns rc=2 silent against the store link and rc=0 writing `x: \n  y` against a writable file, one variable changed. WHY THE UNIT VARIED (frexp, then atan, then clc_get_sub_group_id): NOT a race - it is the in-and-out collision, and which unit presents it first is placement ORDER inside the batch, deterministic. WHY ONLY SOME EDGES: the edit-in-place partition diverted an input that is also a declared output ONLY when it was a DIRECTORY (`is_dir()`), because the case that motivated it edits a TREE (ExternalProject's patch step); a single file with the same collision fell through to the symlink path. THE FIX partitions on the collision alone: a directory is still copied whole with `copy_tree`, a file is REMOVED IF A LINK and copied writable with `mode() | 0o200` restored - the convention `local::write_restored` uses. `set_readonly(false)` was tried first and clippy refused it correctly, because it clears EVERY write bit and makes the file world-writable. VALIDATED, then WITNESSED ON THE PACKAGE: with the fixed binary mounted (the consumer repointed, and the master's `lstart` 14:24:38 is after `/etc/nix/nix.conf`'s mtime 14:24:25, so the mount is live), `fzsash3cjn2vjb4addvnshlhjrhw2dcp-mesa-libclc-22.1.8.3.drv` REALIZES - rc=0, and `/nix/store/ld4d5x2fn82gcf90vlpqajply80pjkdv-mesa-libclc-22.1.8.3` is a registered path of 11,348,848 bytes carrying both `spirv-mesa3d-.spv` (5.5 MB) and `spirv64-mesa3d-.spv` (5.5 MB). The same drv reproduced the abort on this host before the fix, so this is the package witness and not a fixture. THE ABI IS UNCHANGED and is not the driver's to change: `task_abi()` reads `NIX_NINJA_TASK_ABI` from the environment and `abi_in_key` emits it only under a stable builder, so the VALUE is the consumer's hook's | `239d6b1` | `edit_in_place_tests::a_file_declared_as_an_output_is_diverted_and_a_plain_input_is_not` (`6468bce`) asserts the collision diverts AND, as its control, that an input no output declares is still left to the symlink materialisation - MUTATION-CHECKED (diverting only `.cl` files fails it). Fixture built through the task's own encoder so it cannot drift from the real spelling. The difference proof above remains the package-level evidence | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | a generator's TEMPLATE and HEADERS named bare, declared by no edge | FIXED `b145e81` and `2404fb9` (driver only, task binary UNMOVED at `8qh72nws`, so no banked compile re-keys). It was the desktop round's LARGEST SINGLE CAUSE: `glib-mkenums --template <name>.h.template <25 headers> --output=...` runs with `WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}` and declares no `DEPENDS` on any of them, so the CMake rule names a dependency on none of the files its own command reads. THE FIRST READING CALLED THIS A PACKAGE DEFECT, and the reasoning that produced it is the trap, not the conclusion: EVERY undeclared read this tree already carries a reader for (swig `%include`, msgfmt ITS, the `g-ir-scanner` filelist, protoc imports, valac `---vapidir`) also names no `DEPENDS`, because the CMake convention is to rely on the shared filesystem. THE DECIDING TEST IS WHETHER REAL NINJA SUCCEEDS - it reads them off the working directory on disk, and the sandbox materialises that directory from DECLARED inputs, so the file is absent there and present for ninja. That signature puts the line in the DRIVER. MEASURED 11 task drvs over `ayatana-ido`, `llvm`, `onetbb` and `qtbase`, 153 log lines; the task's own drv (`6p8jp3i7ikv9m4bdrzdv1pgpr041vlkc-ninja-build.drv`) declared fifteen inputs and NOT ONE was the template or a header, and `source/src/` in the kept tree (`nix-12739-1281209585`) was empty. THE READER RESOLVES AGAINST THE DIRECTORY THE COMMAND RUNS IN, not the build dir, and here they DIFFER: the rule runs `cd /build/source/src` while the build dir is `/build/source/build`. `2404fb9` fixes the TRIGGER, and its arm is the lesson: the first arms fed a BARE argv where the program is the first token, production passes it behind a `cd` prologue at index THREE, and a reader skipping a fixed index carried the prologue as headers - THE PRODUCTION-SHAPE ARM FAILED AGAINST THAT READER | `b145e81` `2404fb9` `d8aa417` | `the_argv_shape_production_passes_names_the_template` uses the argv read VERBATIM from the failing derivation; `a_glib_mkenums_name_resolves_against_the_commands_own_directory` carries four arms with the negative control that the same names resolved against the BUILD dir carry nothing; `glib_mkenums_names_its_template_in_both_spellings` and `a_command_that_is_not_glib_mkenums_is_not_an_invocation`. 353 tests, clippy and the pinned formatter clean. THE PRICE IS OWED, not claimed: the round was live and the pricer returns false verdicts beside other nix work | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | a task REFUSED for a compiler it would never look up | FIXED `30c75a3` (driver only, task binary UNMOVED at `8qh72nws`). rustc 1.98.1's vendored LLVM ran 41 `llvm-min-tblgen` edges and EVERY ONE was refused, aborting the package and the KDE stack behind it. THE GATE READ `names_only_absolute_toolchain` FALSE - "the command names no toolchain" - AS "the task needs one from PATH". `needs_cc` is true for THREE separate reasons and only two imply a lookup: a bare toolchain name (must be refused), naming none with an OBJECT output (a generator execs the compiler from inside its own process, `dtrace`, class 7 `3ec8201`, must be refused), and naming none with a non-object output (nothing asks PATH for a compiler, must NOT be refused, which is this edge). THE OBVIOUS ONE-LINE REPAIR WAS WRONG AND WOULD HAVE SHIPPED: making the predicate true when nothing is named also flips the `$PATH` push at the SECOND call site, so `dtrace` loses `cc` and dies `No such file or directory: 'gcc'` - regressing a closed class. NOTHING CAUGHT IT because the two predicates are asserted SEPARATELY and the gate computes NEITHER. THE INSERT CONDITION IS DELIBERATELY THE ORIGINAL ONE; an intermediate version of this fix inserted `cc` whenever it RESOLVED, which would have re-keyed every absolute-toolchain edge including mesa-libclc's 514 `clang` tasks, all of which have banked outputs. Keeping `!names_only_absolute_toolchain` means wherever `cc` resolves the same path goes in under the same test, so emission is unchanged for every derivation that exists today; only the ABSENT case moves, and there the old code ABORTED, so no derivation existed to invalidate | `30c75a3` | `the_refusal_gate_answers_by_why_the_compiler_is_needed` pins THE COMBINATION the gate computes, carrying the refused command VERBATIM from the round log, and MUTATION-CHECKED BOTH WAYS: reverting to the old behaviour fails it, and deleting the object-shape arm fails it at the `dtrace` assertion. 353 tests, clippy and the pinned formatter clean, fifteen consecutive suite runs with no flakes. THE PRICE IS OWED, not claimed | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | a chdir TEST and the module that owns the lock | FIXED `52714b3` (driver glob, task binary UNMOVED). `CHDIR_LOCK` was declared INSIDE `create_symlink_undeclared_output_tests` while `macro_arm_headers_tests` also calls `set_current_dir`, which is PROCESS-GLOBAL with cargo running tests on THREADS, so those tests interleaved and failed at random. LATENT, NOT INTRODUCED: it reproduces 3 runs in 6 on the commit BEFORE the change that exposed it, and adding two tests anywhere in the file shifted the schedule enough to surface it. THIS SHAPE HAS NOW COST TWO ROUNDS OF THE SAME WORK IN THIS FILE - `OUT_ENV_LOCK` was hoisted from `ninja_pool_tests` for exactly this and its comment already said so. THE RULE: a lock over a PROCESS-GLOBAL resource belongs at FILE SCOPE, and its declaration is where the next session looks | `52714b3` | Twenty consecutive suite runs pass where the same loop failed three in six. `set_current_dir` has six call sites in this file and each now takes the shared lock | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | a build product EXECUTED BY ITS PATH, whose loader search path is `$ORIGIN`-relative | FIXED `f8fb5e8` + `7877e37` (TASK BINARY: `8qh72nws` -> `mkvab8y3`, so `/nn-task` must move with `/nn-driver`, see the re-key record). orc 0.4.44 runs `tools/orcc` to generate a header and died `error while loading shared libraries: liborc-0.4.so.0`, exit 127, across 5 task drvs. THE MECHANISM IS PLACEMENT AND THE LIBRARY IS PRESENT THE WHOLE TIME: `create_symlinks` materialises the tool as a SYMLINK INTO THE STORE, so the loader resolves `$ORIGIN` to the STORE, and orcc's RUNPATH `$ORIGIN/../orc` names `/nix/store/4m052g3...-ninja-build-tools-orcc/orc/` - which does not exist - while the build tree's `orc/` holds both `liborc-0.4.so.0.44.0` and a correct alias chain (`liborc-0.4.so.0 -> liborc-0.4.so.0.44.0`). The alias was NOT the defect; the peer's first reading stopped one level high, and mine that the alias was missing was wrong. PROVEN BY DIFFERENCE on the failing task's own inputs: the old binary leaves a symlink and the tool dies rc=127; the fixed one copies a real file and the tool RUNS. THE REAL-NINJA DIVERGENCE IS THE DECIDING TEST: in a real build dir the tool IS a real file, so `$ORIGIN` means the build tree and ninja succeeds, which puts the line in the DRIVER. THE FIX is one more arm on `copy_not_link`, beside the `.py`/`node_modules`/header arms that exist for the identical reason (a script that realpaths itself loses its siblings); it asks for a `$ORIGIN` RUNPATH specifically, so a tool with an absolute search path, which is every compiler wrapper, keeps its symlink and no task gains a copy. TWO DEFECTS IN MY OWN FIRST ATTEMPT, each caught by RUNNING rather than reading: a predicate that shelled out to `patchelf` answers FALSE where patchelf is absent, i.e. it returns the bug with no diagnostic; and a test that ASSERTED a store path present broke this crate's OWN derivation, because the tests run in a sandbox where that path is not an input. THE NEGATIVE CONTROL IS NOW THE TASK BINARY ITSELF, which carries an absolute RUNPATH and is present wherever the test runs. DO NOT reach for `LD_LIBRARY_PATH` or a wider PATH - neither is the lever, and both are emission changes far wider than the defect | `f8fb5e8` `7877e37` | `an_origin_relative_tool_is_recognised_and_an_absolute_one_is_not`, mutation-checked BOTH ways (an always-true predicate fails at the wrapper control, an always-false one fails at the subject); the end-to-end witness is a sandbox drive of the failing task's own inputs, old binary FAIL rc=127 and fixed binary RUNS. NOT gated in the corpus yet: the drive needs the orc store paths and belongs beside a boundary rather than in run-all | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | a file a `cmake -P` script ARCHIVES | FIXED `10fd6c7` (TASK BINARY: `/nn-task` must move with `/nn-driver`). `file(ARCHIVE_CREATE FORMAT raw)` REFUSES ANYTHING THAT IS NOT A REGULAR FILE, and qtbase 6.11.2's rcc step archives its mimetypes xml, which the task placed as a symlink into the store. The step died `CMake Error: archive_write_header: Raw format only supports filetype AE_IFREG` and took every Qt package and the desktop stack behind them. PROVEN BY DIFFERENCE, one variable: the failing call against a symlink reproduces the round's error and writes a ZERO-BYTE archive, and against a regular file writes 45,991 bytes. SAME CONSUMER REASON as the `$ORIGIN` executable, the script realpath and the quoted-sibling header: a consumer that needs a RESOLVED or REGULAR file rather than a link. THE COMMAND SPELLS THE HOST PATH WHILE THE TASK RUNS UNDER ITS OWN ROOT, so the first version of the match carried NOTHING and the consumer's round proved it by still failing: the task's `build_dir` is `/build/source/build`, the `-D` value names `/build/qtbase-everywhere-src-6.11.2/src/corelib/...`, and the input `build_path` is `../src/corelib/...`, so the two differ ONLY IN THEIR ROOT and the TRAILING COMPONENTS are the identity because the sandbox mirrors the source tree. Fixed `a8c4890` by matching the longest trailing run of at least two components; the script lookup had the same fault and hid the arm behind it. The signal is on the COMMAND LINE rather than in the file's name, so the TASK computes it: it has the cmdline with its `-D` values and the `-P` script among its inputs, and the driver could not, because a file from another task is still a PLACEHOLDER at drive time. NARROW BY CONSTRUCTION: a `cmake -P` invocation, an `ARCHIVE_CREATE` with `PATHS`, and a file the task received; of the failing task's 466 inputs exactly two are named on its command line | `10fd6c7` `a8c4890` | `the_paths_argument_of_an_archive_call_is_found`, `a_file_call_that_is_not_an_archive_contributes_nothing`, `the_script_is_taken_from_a_dash_capital_p`, `the_definitions_come_from_the_command_line`, `a_call_body_ends_at_the_first_parenthesis`, all against qtbase's own `QtCompressMimeDatabase.cmake` text verbatim. THE FULL TASK DRIVE IS REACHABLE AND WAS RUN, and the method matters more than the result: build the task's OWN input set into a test derivation (`builtins.storePath` per path, 667 of them here) and run the real command line against it WITH THE REAL `--build-dir` from the derivation. Omitting `--build-dir` was my own first harness's error and it put the files in a different tree from the one the command cds to, which reads as the fix not firing. With the fixed binary the file is a regular 317,384-byte file and the archive is written (250,439); with the previous binary it is a symlink and the archiver refuses. A client-side `--option extra-sandbox-paths` CANNOT substitute for this, because the daemon's own entry wins | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | `glib-genmarshal`, the same reader as `glib-mkenums` | FIXED `10fd6c7` (driver only, so it rides the task-binary move above at no extra cost). `glib-genmarshal --prefix=X --header indicator-object-marshal.list --quiet --output=Y` died `can't open 'indicator-object-marshal.list'` across TEN task derivations, the round's largest single cause at that point. IT IS THE SAME SHAPE AS `glib-mkenums` in every respect - a `cd /build/source/src` prologue against a build dir of `/build/source/build`, the program named by absolute store path, an argument named RELATIVE that lives in the SOURCE tree - and differs in ONE respect worth keeping: the file is a POSITIONAL argument after `--header`, not a `--template` value, so a reader keyed on the flag carried nothing. The trigger now names the FAMILY (`glib-mkenums`, `glib-genmarshal`) rather than one program, which is the correct shape when two programs share a resolution rule | `10fd6c7` | `a_glib_genmarshal_run_names_its_list_positionally`, carrying the round's command line as spelled and asserting neither the prologue nor the flag values are inputs | `local/pending/next-rekey-batch.md` 2026-10-14 |
| | a generated build plan that NAMES ITS OWN SOURCES (`cmake -E cmake_autogen`) | FIXED `51ec251` (DRIVER ONLY: the task `drvPath` is identical at both revisions, so only `/nn-driver` moves). `cmake_autogen` is handed `AutogenInfo.json` and resolves every header it will include from the lists INSIDE it, and NO EDGE DECLARES THOSE FILES because at configure time they were reachable on a shared filesystem. qtbase 6.11.2's `Core` autogen task carried 470 inputs and NONE was under `src/3rdparty`; of the plan's 116 `MOC_INCLUDES` FOUR directories were absent from the task's tree (`src/3rdparty/forkfd`, `src/3rdparty/tinycbor/src`, `build/src/corelib/kernel`, `mkspecs/linux-g++`), so cmake died `The header file "SRC:/src/3rdparty/forkfd/forkfd.h" does not exist` on the first one it opened - and every `Core.dir` TU failed behind it, which is every Qt package and the KDE stack. SAME FAMILY as the `-P` script, the `g-ir-scanner --filelist` and the protoc imports: a file a command READS whose own references nothing declares. The plan is now read at DISCOVERY from the path the `cmake_autogen` invocation names, and `MOC_INCLUDES`/`CMAKE_LIST_FILES` are carried the way a directory argument is. THE `SRC:` PREFIX IS NOT THE DEFECT, and it was worth checking before acting: it is cmake's own marker for "relative to `CMAKE_SOURCE_DIR`", so `SRC:/src/...` resolves to `<source>/src/...`, which is the path the plan already carries. THE FIRST VERSION ABORTED THE TASK ON THE FIRST ABSENT DIRECTORY: it handed the plan's own absolute paths straight to the directory upload, whose walk calls `read_dir` and propagates the error, and the plan mixes source-tree paths with build-tree ones that do not exist until another edge has produced them. The sibling directory-argument reader is safe only because it asks `is_dir` before it calls, and that test is what `e760ebe` adds, with the store-path and project-tree filters the file readers use | `51ec251` `e760ebe` | `an_autogen_plan_names_the_directories_its_headers_live_in`, `an_autogen_plan_that_names_nothing_absolute_contributes_nothing`, `the_plan_is_taken_from_the_autogen_subtool`; mutation-checked BOTH ways (an empty reader fails, and dropping `CMAKE_LIST_FILES` fails). The guard is witnessed separately: the failing path `build/include/.../6.11.2` skips on `is_dir` false, and `../src/corelib`, 1335 files, is carried. 357 tests, clippy and the pinned formatter clean. The consumer's round is the package witness | `local/pending/next-rekey-batch.md` 2026-10-15 |
| | a generator that execs `pkg-config` BY BARE NAME on a gir edge | FIXED `56aacee` (DRIVER ONLY: the task `drvPath` is unchanged, so only `/nn-driver` moves). `g-ir-scanner` runs `subprocess(['pkg-config', '--cflags', ...])` and pkg-config is not on the emitted PATH, so libayatana-appindicator 0.5.92 died `[Errno 2] No such file or directory: 'pkg-config'`. THE WRAPPER THAT SUPPLIES IT IS ALREADY AN INPUT (`j4nx9d7i...-pkg-config-wrapper-0.29.2` ships `bin/pkg-config` and was in `srcs` while absent from a ten-entry PATH), so the file was present and only the PATH entry was missing. THE ARM ASKS THE INPUTS which one ships `bin/pkg-config`, scoped to a command that runs the scanner; it does NOT widen `SCRIPT_TOOLS`, which would put a name on every task and in every task's inputs. THE ASK IS SHARED: `ninja_input` was generalised into `input_shipping(store_dir, inputs, rel)` rather than writing a second asker, because a name test takes this driver's own binary for a tool and misses a second resident copy. PRICING TRAP, recorded so the number is not trusted: `emission-price.sh` reads FREE for a `SCRIPT_TOOLS` widening on a host where `which pkg-config` answers `/usr/bin`, because `which_store_path` rejects it and the tool contributes nothing; inside a build whose closure carries it, the widening re-keys every task | `56aacee` | `an_input_is_found_by_the_file_it_ships`, `an_input_that_does_not_ship_the_file_is_not_returned`, both mutation-checked BOTH WAYS (a predicate returning the first input fails, and one returning nothing fails). THE FIXTURE BUILDS ITS OWN STORE OBJECTS (`c1ea058`): the first version named two LITERAL store paths that were resident on the writing host, so the arm passed there and FAILED on a host without them, which is the shape `7296100` records and the same defect as `emission-price.sh` reading FREE where its tool is unreachable - the reader was correct in both places and the fixture was a property of one machine. The shipping object sits SECOND in the list so a first-input ask cannot pass. 359 tests, clippy and the pinned formatter clean. THE EXEC HALF IS WITNESSED AND THE FIX IS NOT SUFFICIENT, measured on the consumer's round 2026-10-15: the edge now RUNS pkg-config and fails differently, `Package gtk+-3.0 was not found in the pkg-config search path` for every package and then `app-indicator.c: no such a file or directory`. THE WRAPPER BINARY IS PRESENT AND ITS ENVIRONMENT IS NOT. `bin/pkg-config` sources `nix-support/add-flags.sh`, whose `accumulateRoles` fills `role_suffixes` from `NIX_PKG_CONFIG_WRAPPER_TARGET_{BUILD,HOST,TARGET}_*`, and with that EMPTY it execs the real pkg-config against the INHERITED `PKG_CONFIG_PATH`. Both the marker and the search path are exported by the wrapper's `nix-support/setup-hook`, WHICH IS AN STDENV SETUP HOOK AND A TASK RUNS NONE. MEASURED on task drv `gda42pvv9ywyc2fnfdljdwp52agl3z5p`: 11 PATH entries including the wrapper's `bin`, 176 `srcs` including gtk+3 and gtk+3-dev, 13 env keys with `PKG_CONFIG_PATH` ABSENT. THE `.pc` PATHS IN `srcs` ARE NOT EVIDENCE THE VARIABLE WAS SEEN - they arrive through `NIX_CFLAGS_COMPILE` and `NIX_LDFLAGS`, which the allowlist forwards, and pkg-config does not consume them. THE DISCRIMINATOR is the command line: colord's gir edge spells `env PKG_CONFIG_PATH=...` and works, appindicator's does not and fails. THE FIX IS ONE ENTRY IN `forwarded_to_tasks`, AN EMISSION CHANGE, PRICED AND LANDED AT `22edd62`: task binary unmoved, five shapes byte identical with the three control arms firing before the verdict. The feature shape gains 6074 bytes and 41 inputs wherever the outer build sets the variable (measured on colord's own 3336-byte 42-entry value), nothing where it is not. The driver moves (`5kg37d3d` -> `sgsarv6w`) and the task does not (`mxczpqcg`), so only `/nn-driver` moves. A g-ir-scanner EDGE WITH NO `--filelist` NAMES ITS SOURCES POSITIONALLY, FIXED `fdae614` (driver only, priced FREE on all five shapes, task `drvPath` unmoved): the round's leaf on task drv `85mh8cgmszj3295m0q9vsh978ji03kbv`, `app-indicator.c: no such a file or directory` on a 176-`srcs` task carrying neither source, so the trigger takes `.c`/`.h` bare names after the program token and resolves them against the command's own directory. The consumer's round is the package witness for the gir edges | `local/pending/next-rekey-batch.md` 2026-10-15 |
| | a TEST that asserts its subject against its own source | FIXED `7296100` (driver glob, task binary UNMOVED): `self_rss_tests::reports_a_plausible_resident_size` checked `self_rss_mib` against a "second, independent" reading that parsed THE SAME FILE'S SAME FIELD (`/proc/self/statm` field 1), with the test's own 16 MiB ballast landing between the two calls. It therefore measured the PROCESS's churn, not the subject: 25/25 green on an idle host and two CI failures at `self_rss_mib 129 MiB disagrees with statm 83 MiB`, a 45 percent swing across two consecutive reads. ITS COMMENT CLAIMED the subject read `VmRSS` from `/proc/self/status`, which was true until `e42aaee` changed the subject and the comment was never updated, so the arm lost its independence in silence. The arm reads `VmRSS` from `/proc/self/status` now - a different file, a different kernel path. THIS CLASS WOULD HAVE BLOCKED A PIN BUMP: `nix flake check` runs the unit tests, so a consumer pinning this line would have seen it go red on THEIR machine for a reason unrelated to any driver change | `7296100` | mutation-checked: a subject returning a constant FAILS the arm (`4 MiB disagrees with VmRSS 36 MiB`), which the previous version could not detect. CI green on all 13 jobs. THE GENERAL SHAPE: an "independent" reading that shares the subject's source is not a control, and a check that is green locally and red in CI across commits differing only in unrelated files is the trigger to look |

Reading rules: a passing reduction is evidence about the reduction; built
means thousands of log lines, substituted a handful; a failure count over a
round counts PACKAGE derivations and a failing task derivation carries no
package name; a failing `try_compile` probe is its answer. A ROUTED-AROUND
PACKAGE IS MASKED: a green round over `ccPreferred` is evidence about the
reroute only; some entries are UNREACHABLE BY CONSTRUCTION on the compiler
route (links are never narrowed), nss reaches both. Fixing what a rerouted
package hits on its NEW route says nothing about the defect that rerouted
it. A route with no working example has no gate.

## Gates

`local/gates/emission-price.sh` is a PRICER, not a corpus arm: run it before
landing any driver edit and before any pin bump. `local/gates/run-all.sh` is
THE corpus and the one command a pin bump must satisfy: it builds both
binaries once, runs the A arms, then the B arms inside recursive-nix, and
reports one tally. THE TWO CONFIGURATIONS STAY SEPARATE INSIDE IT, because a
verification is a claim about its configuration and an A arm cannot reach a
mirror-only class; what is gone is the second RUNNER, whose existence let a
session run A, read its tally as the state of the tree, and leave B unrun.
Arm zero is `cargo test --workspace` (re-run single-threaded on the
poisoned-lock signature), arm
one `local/gates/registered.sh`, which derives which gates any runner
invokes and fails an exclusion entry naming a script a runner DOES invoke.
IT ALSO FAILS A REGISTERED PATTERN ITS GATE NO LONGER PRINTS, and that is
the arm to run the moment a gate's verdict TEXT changes: extending
`cmake-p-include-module-repro.sh` rewrote its PASS line while `run-all.sh`
still matched the old one, so the arm scored against a pattern that could
never appear and a real PASS would have read as a failure. Editing a gate's
output and editing its registration are ONE change.
NO ANSWER is scored apart from FAIL (an exit status
is not a verdict); a runner must not re-decide a
precondition its gate owns. A gate for a discovery mechanism must switch off
every other route that could supply the file
(`NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0`). `run_gate` scores on verdict TEXT, so
a gate must also exit non-zero on failure, and the B half additionally FAILS
an arm whose script is not executable, because "permission denied" once
scored as an unbuilt arm with an empty cause. In run-all:
`local/gates/stable-task-builder-repro.sh` (the knob; its CONTROL arms
matter: two binaries DIFFER unknobbed, the store path is NAMED unknobbed)
and `logging-contract-repro.sh`. `local/gates/shim-intercept-count-repro.sh`
and `local/gates/round-intercept-sweep.sh --self-test` are also in run-all;
they read the CONSUMER's shim, so a missing shim there is NO ANSWER, not
FAIL. `local/gates/example-arm-census.sh` is in
no runner (without `--build` it asserts nothing); with `--build` it is the
ONLY arm asserting the stats line's `drv_bytes` is non-zero, so run it
beside a pin bump, since a constant-zero regression passes every other gate.
Owed:
`gcc-reproducibility-probe.sh` (James's). Run
`second-pass-placement-repro.sh` after any change to placement, the walk or
the readers. `generator-edge-execution-b.sh` takes `NN_PROBE_PKG=dtc|orc`
and demands `--run`. THREE ARMS ARE REGISTERED AND NAMED NOWHERE ELSE, so
this paragraph is their only index: `stdout-contract-sweep.sh` (STATIC
sweep of every stdout write against the logging contract, complementary to
the runtime `logging-contract-repro.sh` and not a duplicate of it),
`ninja-flag-surface.sh` (every option the shipped ninja offers is accepted
or listed as refused), and `relative-include-base-repro.sh` (a quoted
`../`-spelled include whose base decides, scipy's `_zeros.pyx`).

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

`.cargo/config.toml` redirects `target-dir` to
`~/.cache/cargo-target/nix-ninja`; never bypass it. Tests belong in
`crates/nix-ninja` reaching `pub` functions; a test under the fileset spends
every banked compile. Pin the WIRING, not only the rule: mutation-check that
reverting the guard fails a test that runs it. A predicate written twice
diverges; export one. No compatibility wrappers. Commit before mutating.

## Daemon, logging, concurrency

Daemon: nix 2.36.0pre with `builder-rpc-v0`; `dynamic-derivations`,
`ca-derivations`, `recursive-nix` in the DAEMON's nix.conf as well as the
client's (the error blames the client).

THREE NIX VERSIONS ARE IN PLAY AND NONE OF THEM DECIDES THE OTHERS. The
daemon that answers `builder-rpc-v0` is pinned by a LITERAL STORE PATH in
`/etc/s6/sv/nix-daemon-srv/run`, independent of every flake: it is
`2.36.0pre20260825_3aef07e` (GC-rooted, so a collect cannot take it). This
tree's own `nix` input builds the rpc CLIENT its gates run, and the
CONSUMER's `nix` input builds the client its rounds run. As of `1ae7425`
(2026-10-06, witness corpus `df55697`) this tree's client is master's
`bea853d2`, the same revision the consumer took at that boundary, against
a nixpkgs with which nix builds on boost 1.89 (its own pin, so the
consuming distribution's 1.91 URL patch is obsolete against this nix).
The serving daemon `3aef07e` is OLDER still. So:
- A GATE PASSING PROVES ONE PAIR, this tree's client against the s6 daemon.
  It is not evidence about the consumer's client against that daemon, and
  once the two inputs diverge it stops being the same question. Say which
  client a `builder-rpc-v0` result is about, the way a verification says
  which configuration.
- THE RUN SCRIPT HAS THE SANDBOX-PATHS DEFECT. A literal store path in a
  service file goes stale exactly as `extra-sandbox-paths` does, silently
  while the old target is resident, and this file documents that shape at
  length for the mapping and never noted the daemon carries it too. A
  daemon bump is therefore a THIRD moving part at a boundary, beside the
  pin and the mapping, and it is a root edit plus a restart.
- Compatibility between a client and this daemon is only ever established
  EMPIRICALLY; the newest witnessed pair is `bea853d2` client against
  `3aef07e` daemon, the full corpus green at `df55697`. `/tmp` is tmpfs: export `TMPDIR` to
disk before a copy or large stage; scratch there is James's to delete.
Daemon stderr is `/var/log/nix-daemon/current`; a crash names the worker pid, hence the kept build dir. A transport
error can be a build failure: serialize before blaming the daemon.
A recursive round is one long-lived worker with a thread per inner
connection; parent-log silence is not inactivity. Watchdog ladder
`300 << 2n` covers `build_paths` and every other exchange gets the first
rung once (`bounded`); fixed on purpose, stalls synchronize. A rate-limited
substituter (HTTP 429) looks identical by state. `--keep-failed` does not reach a recursive build's inner
tree.

ONE DAEMON CRASH PATH IS A RETAINED FAILING BUILD, AND `keep-failed` OPENS IT.
`unix-derivation-builder.cc`'s `cleanupBuild` reads `drv.isBuiltin()` behind
`settings.keepFailed && !force`, and that `drv` is a bare
`const BasicDerivation &` in `DerivationBuilderParams` bound from the goal's
`ref<const BasicDerivation>`. `&&` short-circuits, so with `keep-failed` off
the reference is never read; with it on, the deleter runs during goal teardown
and reads a dead referent. Signal 11, a coredump, and the retained build
directory lost - the one artifact the setting exists to produce.
`keep-failed` defaults to false in nix's own `globals.hh`; a host hits this
only where it was turned on, and `/etc/nix/nix.conf` on this one sets it true.
THIS IS ALSO WHY A CRASHED WORKER LOOKS LIKE A BUILD FAILURE: the worker dies
while supervising the failing build it retained, so the crash is downstream of
a failure and never of its assertion's result. Staged upstream as a daemon-side
dangling reference, not a nix-ninja defect.

THAT PATH IS NOT THE ONLY ONE, AND THE DIFFERENTIAL IS NOT A CLEAN SWITCH.
Measured over two builds of one package, `keep-failed=true` gave crash_delta 1
with one disconnect and one kept tree where `false` gave zero of each. But
crashes ALSO land in an EVALUATION window - two at 20:35 and 20:37 on
2026-09-29, both during `nix build --dry-run`, with `keep-failed=false` in
effect - and that window has never been priced. So read the delta around a run
rather than trusting the switch, and do not read a quiet log as the mechanism
being absent. Which path an individual crash took is not discriminated by the
log, and the kept build dirs that would name it are root-owned. Record:
`local/pending/next-rekey-batch.md`.

Logging (held by `logging-contract-repro.sh`): stdout is data (`--version`,
`-t list`, `-t drv` JSON, a `-v` build's task output); everything else is
stderr with a `nix-ninja:` / `nix-ninja-task:` / `nix-ninja-dynamic-task:`
prefix. `FAILED` is per edge (C), `ABORTED` per invocation (A, B: one line
naming the derivation, no edge counted); no count line exists at zero, so
an absence of `FAILED` is a count claim. Nothing per file or per task
without a gate. A round log is torn: read the raw line.

Concurrency: `concurrent compilers = packages in flight x max-jobs`, per
worker per connection; the driver's `-j` is not a term; in a build the
first term is also `max-jobs`, so the machine-wide figure is its square;
`cores` sets the driver's `-j` (`resolved_jobs`) AND threads per build.
Route sizing through Saxum's governor, never literals. Realise latency
under a round is the daemon's single SQLite state mutex: answer throughput
complaints by closure and realise counts per task, never core count. A
`-j` sweep on a busy machine measures the daemon. A daemon window cannot be
inferred from load average: read the worker's CPU DELTA
(`daemon-gap-cpu-probe.sh`, which also counts worker crashes). A
`DaemonStalled` is NO ANSWER about the run and NOT evidence the daemon was
busy: crashed workers produce it in milliseconds, invisible to CPU sampling
(`local/audits/2026-09-07-daemonstalled-is-a-worker-crash-loop.md`); read
the crash delta in the daemon log. `forced-include-source-header-repro.sh`
reproduces that crash loop in a second. Linux 7.3's `cgroup_mode` renumbers
every figure here.

Generation cost: the stats line `file adds N s/M calls (H s on cache hits)`
splits stamp-cache hits (`nar_stamp_hit`, ~97% of calls, never reaching the
daemon) from real uploads, the only part a change can reach. The total is
THREAD TIME under `new_opaque_files`, not wall clock; store-add calls must
equal the NAR total (`bench/e2e.sh` records `counter_disagreement`). The
cache persists across local drives (`nar_stamps_snapshot`,
`seed_nar_stamps`) and starts cold in a sandbox.

## Counting and evidence rules that cost a day each

- A zero is the instrument until proven otherwise: capture status and
  output, anchor globs, `find | head` is not a sample, a package-prefixed grep cannot see a task's log, an optional
  prefix defeats a `^` anchor, a `tail` discards the phase you would count.
- Any claim supported by the absence of matching lines is a count.
- A `grep -c` for the rejoined form does not verify a backtick repair; use
  `awk '{n=gsub(/`/,"`"); if (n%2) print NR}' FILE` and iterate to zero.
- Verify every fix on one small package on B before it reaches a closure:
  A GATE MODELS THE HALF ITS FIXTURE HAD; the package carries both.
- Read the failing task's own derivation and the LEAF derivation's log
  before naming a class (a `.drv` names its dependencies even when its log
  is `not available`); a class read from the error text has been wrong
  here repeatedly.

## Where things are

| subject | file |
|---|---|
| design, dynamic deps, dynamic derivations | `docs/design.md`, `docs/dynamic-deps.md`, `docs/dynamic-derivations.md`; dyndep/Fortran/modules in the header of `crates/nix-ninja/src/dyndep.rs` |
| full history of every class above | `local/incidents/2026-09-04-claude-md-full-record.md`, `local/incidents/2026-09-17-claude-md-full-record.md`; per-date incidents in `local/incidents/`; `local/daemon-wedge.md` |
| n2 patches, decomposition, ledger | `local/upstream/`, `local/upstream-ledger.py` (run on `main`) |
| citation gate for this file | `local/gates/claude-md-citations.sh`; cite symbols, not lines; run after every edit |
| the roadmap's own counts | `local/gates/roadmap-numbers.sh`; derives each self-descriptive number in `docs/ROADMAP.md` from the tree and fails on drift. Seconds, no build. It exists because an exact commit count, a work-item count, a row count and a corpus count were each written once and rotted: run it after ANY edit to the roadmap, `CHANGELOG.md` or this file's failure table, whose row count the roadmap quotes |
| fixture-shape sweep, owed items; opt-out triage | `local/audits/2026-09-03-fixture-shape-sweep.md`; `local/opt-out-triage.md` |
| re-key record | `local/pending/next-rekey-batch.md`, append-only; record both prices before landing |
| stable builder knob, pin bump | `local/pending/stable-task-builder-proposal.md`, `local/pending/pin-bump-37672a4-to-head.md` |
| `local/`'s second copy | the private repo `jdmanring/nix-ninja-local` (`origin` of `local/`; Actions off). Push freely. The repo's history keeps dated internal message subjects the pre-push hook reads as publication; use `--no-verify` for pushes there (James's decision 2026-10-11). |
| upstream todo (theirs); conventions; benchmarks | `docs/todo.md`; `bench/e2e.sh`, `crates/nix-ninja/benches/generate.rs`. `docs/conventions.md` was a 6-line orphan nothing linked to; its content moved into CONTRIBUTING's Conventions section on 2026-10-13 |
| Saxum's record | `~/Projects/saxum/docs/nix-ninja-recovery.md` |
