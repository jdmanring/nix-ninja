# Release policy

The version in `Cargo.toml` names the milestone currently in progress. It is
not a calendar release or a promise of stability. Before 1.0, `0.3.0` means
that 0.1 and 0.2 are complete and 0.3 is still underway. The milestone
criteria and completion state live in [the roadmap](ROADMAP.md).

A version advances only after the preceding milestone's exit criterion is met
and its named gate passes on a clean tree. A delivered change gets a
[CHANGELOG.md](../CHANGELOG.md) row only after it is verified; the row names
the commit and the gate, test or measured run that supports it. Tags mark
intentional releases, not every version change in the working tree. Do not tag
a milestone while its criterion is open.

Before a release, run the checks required by the milestone, inspect the
changelog citations and the pushed-head CI result, then tag the reviewed commit.
A consumer pin change is a separate campaign-boundary decision; a release tag
does not authorize one or move the daemon's stable-builder mapping.
