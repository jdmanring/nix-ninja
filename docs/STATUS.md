# Project status

This page records what a reader can verify from the public repository and what
remains deliberately outside it. Milestone criteria and ordered work live in
[the roadmap](ROADMAP.md); delivered work and its verification live in
[the changelog](../CHANGELOG.md). Engineering background is in
[design notes](design.md), [dynamic dependency inference](dynamic-deps.md), and
[dynamic derivations](dynamic-derivations.md).

## Current direction

The development version is **0.3.105**, the one `Cargo.toml` carries and the
newest changelog row. The inherited `0.1` correctness and `0.2` performance
milestones are complete; `0.3` real-package resilience is in progress. The
first two numbers name the milestone and the third counts verified deliverables
inside it, so 105 says how much of `0.3` is done, not that `0.3` is met. The
tagged release is `v0.3.0`; the tree carries commits after it. See
[the release policy](RELEASE_POLICY.md).

The public command for an end-to-end check is:

    nix flake check

It runs the workspace tests, formatting and lint checks, and the NixOS example
checks. `cargo test --workspace` runs the Rust tests directly. CI also runs the
example tests individually and the source gates described below. CI status is
published at [github.com/jdmanring/nix-ninja](https://github.com/jdmanring/nix-ninja/actions/workflows/ci.yml).

## Public verification surface

`gates/` carries 82 scripts. Most are per-class arms: each reproduces one
driver defect and fails on the revision before its fix, which is why they are
the evidence a closed class carries rather than prose about it. They build
what they need, so they take minutes and a working nix, and they are meant to
be run for the class being changed rather than all at once.

Six run in CI, on a clone with nothing installed, and print a verdict:

- `gates/stdout-contract-sweep.sh` checks the logging contract.
- `gates/duplicate-predicate-sweep.sh` reports predicates implemented twice.
- `gates/cmake-byproducts-target-expression.sh` checks a CMake output rule.
- `gates/changelog-citations.sh` checks that tracked-document commit citations
  are reachable from the default branch.
- `gates/status-contract.sh` checks this page against the tree.
- `gates/changelog-versions.sh` checks the changelog's version sequence.

`gates/daemon-gap-cpu-probe.sh` is a diagnostic rather than a gate: it samples
live daemon workers, prints no verdict, and is run by hand during a watchdog
gap.

Two of the CI gates read git history, so a full clone is required:
`duplicate-predicate-sweep.sh` rebuilds a control from an older commit and
reports NO ANSWER when that commit is outside the history it has, and
`changelog-citations.sh` asks whether each cited commit is on the default
branch history, which it refuses to judge in a shallow repository. That last
check is why a branch cannot cite a commit it has not pushed: a row pointing at
an unpushed commit reports DANGLING until the commit is on the default branch,
which is the same state a reader would meet. CI checks out with full history
for the history-reading gates.

The public repository also carries the workspace tests, format and lint checks,
and the NixOS example tests. Their current result is not restated here: read
the newest pushed-head CI run. The changelog records the evidence for each
delivered change.

## What remains private

The detailed failure-class ledger and the wider gate corpus live in a separate
private repository that a clone of this one does not contain. `docs/ROADMAP.md`
states the plan to publish the corpus and records the class families at
narrative level. The class-by-class table, its per-class gates and their round
logs are not in this repository, so a reader cannot audit an individual closed
class here yet. Nothing on this page should be read as a per-class verdict.

The private ledger is currently 200 rows. It includes closed classes, classes
under investigation, and inherited issues. It is not copied into this page
because copying it would create a second live record with different test
preconditions, round logs and consumer details.

## Limits

- The flake declares `x86_64-linux`; macOS and multi-architecture support are
  outside the current scope.
- `example-nix` does not build on the current route. The roadmap records the
  reproducible boundary.
- Real-package and distribution-scale measurements remain limited. The roadmap
  distinguishes measured results from projections.
- The private corpus and status table are development evidence, not public
  dependencies. A reader must not need a private path to reproduce a public
  claim.

## Authority

`Cargo.toml` holds the development version. `CHANGELOG.md` records delivered
work. `docs/ROADMAP.md` owns milestone criteria and planned work. This page
indexes their public meaning and states the public verification surface; it does
not replace any of them.
