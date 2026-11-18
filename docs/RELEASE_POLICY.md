# Release policy

Two numbers describe the tree, and they move under different rules.

The milestone version in `Cargo.toml` names the milestone currently in
progress. It is not a calendar release or a promise of stability. Before 1.0,
`0.3.0` means that 0.1 and 0.2 are complete and 0.3 is still underway. The
milestone version advances only after the preceding milestone's exit criterion
is met and its named gate passes on a clean tree. The criteria and completion
state live in [the roadmap](ROADMAP.md).

A point release counted in [CHANGELOG.md](../CHANGELOG.md) advances once per
verified deliverable inside the milestone: `0.3.1` is the first `0.3`
deliverable, and the highest number in the table is the newest. A row is added
only after its work is verified, and it names the commit and the gate, test or
measured run that supports it.

**The point-release number is `Cargo.toml`'s.** The manifest and the newest
changelog row carry the same version, so a reader who opens `Cargo.toml` learns
where the tree actually is. `Cargo.{toml,lock}` sit inside `nix-ninja-task`'s
fileset, so the bump is not free: measured 2026-11-17 by `emission-price.sh` on
a one-line version edit, `GLOBAL RE-KEY`, with all five emission shapes byte
identical. The bump is therefore batched with other re-keying work and taken
when the consumer is not mid-build, and it is priced each time rather than
assumed from that measurement.

Tags mark intentional releases, not every version change in the working tree.
Do not tag a milestone while its criterion is open. Before a release, run the
checks required by the milestone, inspect the changelog citations and the
pushed-head CI result, then tag the reviewed commit. A consumer pin change is a
separate campaign-boundary decision; a release tag does not authorize one or
move the daemon's stable-builder mapping.
