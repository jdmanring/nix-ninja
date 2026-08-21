# Staging for upstream

Everything in this directory is a DRAFT for James to send. Sessions stage;
James files. Nothing here has been posted, and no draft is "ready" until it
carries an audit sign-off line naming who attacked it and when
(`~/.claude/CLAUDE.md` rule 10).

The fork exists because a real build needed it: qtwebengine 6.11.1, at least
16,077 tasks (the highest index the driver log reaches; the graph has not been
driven to completion, so that is a floor), which is three or more orders of
magnitude past the examples in this repository - six meson projects, the
largest of them four source files. Most of what follows is not a feature request. It is a
report of what breaks at that scale, with the fix that was needed to get past
it.

## What is being offered, in the order it should go

The order is not arbitrary. Each item is placed by what a maintainer needs in
hand before the next one makes sense.

| # | Item | Target | Status |
|---|---|---|---|
| 1 | The daemon wedge | a NEW issue on NixOS/nix, plus a note here | drafted, `nix-daemon-wedge-issue.md` |
| 2 | Dependency-inference input classes | PR(s) on nix-ninja | not drafted |
| 8 | Depfile support (their #17) | PR on nix-ninja | step one built, `roadmap-coverage.md`; not drafted |
| 3 | Driver performance | comment into their #7 and #4, then a PR | drafted, `issue-replies.md` |
| 4 | Multiple CLI targets | PR on nix-ninja, coordinated with their PR 43 | drafted, `pr3-multiple-targets.md` |
| 5 | Resolve-memo cache | comment into their #17 as an ALTERNATIVE | drafted, `issue-replies.md` |
| 6 | `${rspfile}` support, and the n2 dependency | `evmar/n2`, NOT `hinshun/n2` | destination corrected 2026-08-21, see below |
| 7 | `contrib/devstore.sh` | PR on nix-ninja | drafted, `devstore-pr.md`; **send first** |

**7 goes before all of them, which is why it is last in the table and first in
practice.** It is the only item answering a request the maintainer wrote down
himself, it touches no crate, and it cannot conflict with anything. Numbered 7
because it was written last; ordered first because everything else asks him to
take code he did not ask for.

**1 goes first among the rest because a wedged worker does not stay inside the
build that made it.** Seventeen of them survived as init-reparented root orphans holding
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
("Add benchmarks for end-to-end compilation of NixOS/Nix for perf work", whose
body asks why nix-ninja comes out about the same as ninja) and #4 ("Add
benchmarks for nix-ninja generating derivations to compile NixOS/Nix") are open
maintainer questions carrying no numbers, and we have two profiler samples with
a stated method. Answering an open question earns the read that a large
unsolicited PR does not.

**6 is deliberately NOT a nix-ninja PR.** This fork vendored n2 wholesale to
get `${rspfile}` bound in commands, which is roughly three quarters of its
changed files and a
standing merge liability against an upstream that has live n2 movement (their
issue #41). Offering the vendored tree to nix-ninja would be offering them our
liability.

**But the destination this file named for it was a dead end, and the check
that would have caught it is one this same file already ran on three other
repositories.** Read from the forge 2026-08-21: `hinshun/n2` is a FORK of
`evmar/n2` with issues DISABLED, ZERO pull requests in any state, and a last
push of 2025-04-23. A PR aimed there has nowhere to land, which is the exact
finding recorded above for `hinshun/nix-ninja` and `obsidiansystems/nix-ninja`
- applied to the two nix-ninja forks and not to the n2 fork, in the same
paragraph that established the rule. `evmar/n2` is the live one: 462 stars,
issues enabled, last push 2025-11-10.

That reframes the item. `Cargo.lock` pins n2 to
`hinshun/n2?branch=feature/minimal-pub`, and that branch is **5 commits ahead
and 9 behind `evmar/main` across 6 files** - visibility widening, which is
plausibly something upstream n2 would take. So the question worth putting to
the maintainer is not "who takes our `${rspfile}` patch". It is that nix-ninja
depends on a dormant fork branch that is behind its own upstream, and the
`${rspfile}` work is a reason to look at that rather than a thing to file
somewhere. Whether `evmar` wants either patch is unknown and is not ours to
assume; the honest opening is the dependency state, which is checkable.

## Their roadmap, separately indexed

The table above is indexed by our pull requests, which cannot answer "what of
the maintainer's own list is already covered here". `roadmap-coverage.md` is
that index, against their `docs/todo.md`. Two items turned out to be
substantially built and invisible as such. Read it before proposing new work.

## What is NOT being offered, and why

Honesty here is what makes the rest credible.

- **A concurrency threshold for the wedge.** It was bisected on 2026-08-20 and
  did not reproduce at N = 2, 4, 8, 12, 16, 20, 24 and 32 in the shape the
  incident describes, nor at N = 2, 8, 12, 16, 20 and 24 in the contrasting
  shape. `../daemon-wedge.md` has both tables and the canonical list of instrument
  defects found on the way. Do not restate that count anywhere else; it has
  been wrong in three files across three audit rounds. The incident is reported as an incident; the mechanism is reported
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
  ours. Our commit `81b67e3` describes it as fixed in passing, which is wrong,
  and a description written from that commit would claim their work. The commit
  body still says it: a `git notes` annotation on `81b67e3` carries the
  correction, since rebasing to fix a message would destroy the record of
  having got it wrong. An earlier draft here cited `02cd1aa`, which is not in
  this history at all - it was a pre-amend object, so the citation looked
  precise and resolved to nothing.
- **An end-to-end number for the two performance commits.** We have one from
  our tree, but our tree carries other work in the same area, so it cannot be
  attributed to those two commits without an isolated re-run. Offered as a run
  we will do on request rather than quoted.

## Where these go, verified rather than assumed

Checked against the forge on 2026-08-20 rather than carried from memory, because
an issue number is the one thing in a draft that is both trivially checkable and
silently wrong if nobody checks:

- upstream is **`pdtpartners/nix-ninja`** (260 stars, issues enabled), which is
  also this fork's `origin`. `hinshun/nix-ninja` and `obsidiansystems/nix-ninja`
  are both FORKS of it with issues DISABLED, so a comment aimed at either has
  nowhere to land. Every `hinshun` reference in these drafts is about **n2**,
  which genuinely does live at `hinshun/n2` - `Cargo.lock` resolves n2 to
  `git+https://github.com/hinshun/n2?branch=feature/minimal-pub`;
- the issues we reply into are opened by **elpdt852**, the maintainer. #43 is
  **RCoeurjoly**'s, #56 is **amaanq**'s, #52 is **andrewgazelka**'s, #41 is
  **theoparis**'s. Address the person, not the project;
- every cited number resolved with a matching title on 2026-08-20: 4, 5, 7, 17,
  20, 41, 43, 52, 56.

Read off the forge the same day, replacing claims these drafts had been
carrying on our own say-so:

- **#43** (RCoeurjoly) opened 2026-02-26, one commit, ZERO comments of any
  kind, and `mergeable_state: dirty` - it has conflicts and will not merge as
  it stands. That last fact changes PR 3's plan, which was written around "if
  43 lands first": it cannot land without a rebase, and saying so in the reply
  is more use to the contributor than anything else we had for them;
- **its `want_file` change is real**, read from the PR's own diff rather than
  inferred: `- let _ = scheduler.want_file(fid);` becomes
  `+ scheduler.want_file(fid)?;`. It touches 8 files, +170/-89 of them in
  `task.rs`, and adds the CMake example that closes #20;
- **#56** (amaanq) opened 2026-07-23, still open, no comments. Two of our own
  commit messages credit "obsidiansystems", which is a fork of upstream with
  issues disabled and not the author; `git notes` on `6cc3a6f` and `c49177b`
  carry the correction.

Re-read each thread the hour you post. These are other people's repositories and
the state moves without telling us.

## Where the incident numbers come from

The ground rule below says every number carries the run that produced it, and
until round 5 several of the drafts' own figures did not. Sourced now:

- **16,077 tasks** - the highest task index any driver log reaches, from
  `fullgraph-v82.log`: `nix-ninja: SLOW RESOLVE 5683 ms for
  gen/content/browser/resources/indexed_db/resources.grd (task 16077)`. It is a
  FLOOR, not a total: the graph has never been driven to completion. Fifteen
  logs carry task indices and this is the maximum across all of them. Note the
  trap that nearly recorded the wrong provenance - `fullgraph-v78.log` also
  contains the string 16077, as a gdb thread LWP id, so a grep for the number
  finds two files and only one is about tasks;
- **20 concurrent requests** - the `-j20` in the launch script of record,
  `fullgraph-v74.sh`: `nix-ninja -j20 QtWebEngineCore.stamp`.

Still unsourced, and they are the incident's own figures rather than the
build's: seventeen orphans, ~20 GiB summed RSS, 7.5 GiB largest. Those come
from readings taken by hand during the incident and never written to a file.
The issue draft states them with that provenance and says what one `ps` sample
can and cannot support. They cannot be recovered now; the lesson is recorded in
that draft's "what we should have measured" section instead.

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

Round 6 (2026-08-21) attacked the destinations rather than the prose, which
is where round 5 had just proved this file weakest. One blocking finding, and
it is the same shape as round 2's: **a rule this file states was not applied
to every member of the set it governs.** Item 6's destination is dead, above.
Nothing else in the routing moved - the nix-ninja numbers and authors were
re-read and stand.

Status: NOT SENDABLE. Six rounds are recorded; the blocking findings are in
each draft's own audit block. What holds the set is not draft coverage: it is
round 5's pre-PR question, which is James's to send and has not been sent.
Drafting the remaining items does not change that and must not be reported as
if it did.
