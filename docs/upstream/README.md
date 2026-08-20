# Staging for upstream

Everything in this directory is a DRAFT for James to send. Sessions stage;
James files. Nothing here has been posted, and no draft is "ready" until it
carries an audit sign-off line naming who attacked it and when
(`~/.claude/CLAUDE.md` rule 10).

The fork exists because a real build needed it: qtwebengine 6.11.1, about
15,800 tasks, which is one to two orders of magnitude past what the examples in
this repository reach. Most of what follows is not a feature request. It is a
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

**1 goes first because it changes what other work is safe.** Their 0.2.0
milestone includes an async `nix store add` issue, and anyone implementing it
naively raises client-side concurrency against a daemon that has been observed
wedging under concurrent `build_paths`. A maintainer about to raise concurrency
should know that before, not after.

**3 goes before 2** even though 2 is the larger body of work, because their #7
("why is nix-ninja the same speed as ninja") and #4 (derivation-generation
benchmarks) are open maintainer questions carrying no numbers, and we have
numbers with a method. Answering an open question earns the read that a large
unsolicited PR does not.

**6 is deliberately NOT a nix-ninja PR.** This fork vendored n2 wholesale to
get `${rspfile}` bound in commands, which is 76 of its 88 changed files and a
standing merge liability against an upstream that has live n2 movement (their
issue #41). The correct destination for that one feature is `hinshun/n2`, after
which this fork returns to a git dependency and the vendored tree is deleted.
Offering the vendored tree to nix-ninja would be offering them our liability.

## What is NOT being offered, and why

Honesty here is what makes the rest credible.

- **A concurrency threshold for the wedge.** It was bisected on 2026-08-20 and
  did not reproduce at any concurrency from 2 to 32, in either request shape.
  `../daemon-wedge.md` has the table and the four instrument defects found on
  the way. The incident is reported as an incident; the mechanism is reported
  as open.
- **`-fuse-ld=mold` (their #52).** Unhandled here. We will hit it the first
  time anyone builds with mold, and the fix belongs in the same
  `shell_words::split` and `which` path this fork already patches for quoted
  interpreter tokens.
- **PR 43's phony mechanism.** Declined on the merits, with the reasoning in
  `issue-replies.md` so the author gets an argument rather than silence. Their
  multi-target support is taken, credited, and rebased onto this fork's phony
  model.

## Ground rules for anything added here

1. Every number carries the run that produced it and the date. A figure whose
   method is not stated is not sendable.
2. A claim a maintainer can falsify from their own tree gets checked against
   their tree first. `origin/main` was `9a07e67` on 2026-08-20, identical to
   this fork's base.
3. No draft is finished before an adversarial reader has tried to reject it and
   failed. Record the audit beside the draft.
