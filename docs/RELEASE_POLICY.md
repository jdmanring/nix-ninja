# Release policy

One version number describes the tree, and its parts move under different rules.

The version in `Cargo.toml` has three parts. The first two name the milestone
currently in progress; they advance only after the preceding milestone's exit
criterion is met and its named gate passes on a clean tree, and before 1.0 they
say nothing about stability, so `0.3` means `0.1` and `0.2` are complete and
`0.3` is still underway. The criteria and completion state live in
[the roadmap](ROADMAP.md).

The third part is a point release counted in [CHANGELOG.md](../CHANGELOG.md): it
advances once per verified deliverable inside the milestone, so `0.3.1` is the
first `0.3` deliverable and the highest number in the table is the newest. A row
is added only after its work is verified, and it names the commit and the gate,
test or measured run that supports it.

**The manifest and the newest row carry the same number**, so a reader who opens
`Cargo.toml` learns where the tree actually is. `Cargo.{toml,lock}` sit inside
`nix-ninja-task`'s fileset, so the bump is not free: measured 2026-11-17 and
2026-11-18 by `emission-price.sh` in configuration A, `GLOBAL RE-KEY`, with the
task `drvPath` moving and all five emission shapes byte identical. The bump is
therefore batched with other re-keying work and taken when the consumer is not
mid-build, and it is priced each time rather than assumed from that
measurement.

Tags mark intentional releases, not every version change in the working tree.
Do not tag a milestone while its criterion is open. Before a release, run the
checks required by the milestone, inspect the changelog citations and the
pushed-head CI result, then tag the reviewed commit. A consumer pin change is a
separate campaign-boundary decision; a release tag does not authorize one or
move the daemon's stable-builder mapping.
