# Staging for upstream

Everything in this directory is a DRAFT for James to send. Sessions stage;
James files. Nothing here has been posted, and no draft is "ready" until it
carries an audit sign-off line naming who attacked it and when
(`~/.claude/CLAUDE.md` rule 10).

The fork exists because a real build needed it: qtwebengine 6.11.1, at least
16,077 tasks (the highest index the driver log reaches; the graph has not been
driven to completion, so that is a floor), which is one to two orders of
magnitude past what the examples in this repository reach. Most of what follows is not a feature request. It is a
report of what breaks at that scale, with the fix that was needed to get past
it.

## What is being offered, in the order it should go

The order is not arbitrary. Each item is placed by what a maintainer needs in
hand before the next one makes sense.

| # | Item | Target | Status |
|---|---|---|---|
| 1 | The daemon wedge | a NEW issue on NixOS/nix, plus a note here | drafted, `nix-daemon-wedge-issue.md` |
| 2 | Dependency-inference input classes | PR(s) on nix-ninja | not drafted |
| 3 | Driver performance | comment into their #7 and #4, then a PR | drafted, `issue-replies.md` |
| 4 | Multiple CLI targets | PR on nix-ninja, coordinated with their PR 43 | code landed here, PR not drafted |
| 5 | Resolve-memo cache | comment into their #17 as an ALTERNATIVE | drafted, `issue-replies.md` |
| 6 | `${rspfile}` support | PR on hinshun/n2, not on nix-ninja | not drafted, see below |

**1 goes first because a wedged worker does not stay inside the build that
made it.** Seventeen of them survived as init-reparented root orphans holding
gigabytes, through `SIGTERM` and through a supervisor restart. That is
concurrency-independent and needs no argument to connect it to anything: it is
worth a maintainer's attention on its own.

The weaker version of this argument, which an earlier draft led with and which
should NOT be used: that their 0.2.0 async `nix store add` work raises client
concurrency into the wedge. `nix store add` is a different RPC from
`build_paths`, and our own conclusion is that concurrency alone is not the
trigger, so that bridge is two inferences wide and made of something we just
finished disproving.

**3 goes before 2** even though 2 is the larger body of work, because their #7
("Add benchmarks for end-to-end compilation", whose body asks why nix-ninja
comes out about the same as ninja) and #4 are open maintainer questions
carrying no numbers, and we have two profiler samples with a stated method. Answering an open question earns the read that a large
unsolicited PR does not.

**6 is deliberately NOT a nix-ninja PR.** This fork vendored n2 wholesale to
get `${rspfile}` bound in commands, which is 72 of its 93 changed files and a
standing merge liability against an upstream that has live n2 movement (their
issue #41). The correct destination for that one feature is `hinshun/n2`, after
which this fork returns to a git dependency and the vendored tree is deleted.
Offering the vendored tree to nix-ninja would be offering them our liability.

## What is NOT being offered, and why

Honesty here is what makes the rest credible.

- **A concurrency threshold for the wedge.** It was bisected on 2026-08-20 and
  did not reproduce at seven levels between 2 and 32, across two request
  shapes. `../daemon-wedge.md` has the table, the exact coverage of each shape,
  and the three instrument defects found on the way. The incident is reported as an incident; the mechanism is reported
  as open.
- **Alternate linkers (their #52, `CC_LD`/`CXX_LD`, mold as the example).**
  Unhandled here. We will hit it the first
  time anyone builds with mold, and the fix belongs in the same
  `shell_words::split` and `which` path this fork already patches for quoted
  interpreter tokens.
- **PR 43's phony mechanism.** Declined on the merits, with the reasoning in
  `issue-replies.md` so the author gets an argument rather than silence. Their
  multi-target support is taken, credited, and rebased onto this fork's phony
  model. Note when writing that PR: the `want_file` `Result` fix is THEIRS, not
  ours - our own commit `02cd1aa` describes it as fixed in passing, which is
  wrong, and a description written from that commit would claim their work.
- **An end-to-end number for the two performance commits.** We have one from
  our tree, but our tree carries other work in the same area, so it cannot be
  attributed to those two commits without an isolated re-run. Offered as a run
  we will do on request rather than quoted.

## Ground rules for anything added here

1. Every number carries the run that produced it and the date. A figure whose
   method is not stated is not sendable.
2. A claim a maintainer can falsify from their own tree gets checked against
   their tree first. `origin/main` was `9a07e67` on 2026-08-20, identical to
   this fork's base.
3. No draft is finished before an adversarial reader has tried to reject it and
   failed. Record the audit beside the draft.

## Audit

Round 1 (2026-08-20): ten blocking findings, no sign-off. Round 2, after the
fixes: **NOT SIGNED OFF** again, on three independent grounds, one of which was
a live defect in the instrument rather than in any sentence.

- **The reproducer's oracle could not see the failure being reported.** It
  summed CPU ticks over each daemon child's whole subtree, and under
  `--one-client` there is exactly one child forking every builder, so one
  dead-asleep worker among N burning siblings was masked. The incident is a
  PARTIAL wedge, so the shape the drafts call the matching shape was the shape
  the oracle was blind in. Fixed: the unit is now every process in the tree,
  judged by whether its own subtree is entirely dead, and the ladder was re-run
  from scratch. A selftest fixture pins it by asserting the old reading calls
  the masking case healthy.
- **The coverage count contradicted its own table in three files.** Round 1
  replaced one uncheckable count with another. Coverage is now enumerated per
  shape rather than counted.
- **The script's docstring still carried the overclaim the prose had retired**,
  and the issue draft links that script by path, so a maintainer following the
  link read the disclaimed sentence.

Two further findings worth naming: the memory hypothesis cited the PRE-fix
reading of a figure whose fix predates the wedge, when the post-fix reading is
both later and larger; and the task count appeared in four places, including an
outward-facing reply, with no source at all.

Round 3 is owed. Status: NOT SENDABLE.
