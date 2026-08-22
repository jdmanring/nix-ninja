# DRAFT reply into pdtpartners/nix-ninja PR #26

Not posted. Sessions stage; James posts.

**This goes before `devstore-pr.md`, and it displaces item 7 as the thing that
ships first.** The reason is `README.md`'s own stated ordering rule rather than
a new one: "answering an open question earns the read that a large unsolicited
PR does not." There is an open maintainer question on the exact subject of
item 7, addressed to a named person, unanswered for a month, and it is
currently the thing keeping a draft PR from resolving in either direction.

## What was missed, and it is an audit defect rather than a wording defect

Nothing in this directory mentioned PR #26, `nix-portable`, or
`obsidiansystems/sandstone` before 2026-08-21. Every draft here was written as
though `contrib/devstore.sh` were the first attempt at the problem
`CONTRIBUTING.md` describes. It is the third.

- **PR #26** (jaen, opened 2025-04-18, still an open DRAFT, last activity
  2026-07-23) is titled "Use `nix-portable` to allow using the devshell without
  installing DD-enabled nix globally". Same problem, same motivation, in as
  many words: "I also didn't want to switch `nix` on my machine to some alpha
  release globally".
- **`obsidiansystems/sandstone`** was named by the maintainer in that thread as
  prior art, for its `--store /tmp/store` approach against a pinned nix. That
  is structurally what `contrib/devstore.sh` does.
- jaen then tried the sandstone approach, reported it working, and wrote
  `jaen/nix-dev-wrapper` in Rust to handle syncing paths back to the global
  store.

`devstore-pr.md` carries an audit line saying it was "drafted and attacked in
the same pass" and not yet attacked by an independent reader. That audit passed
over a prior attempt on the same subject, sitting open on the very repository
the PR targets. Recorded here as evidence about the audit rather than only
fixed in the draft: an audit that misses the first question a maintainer would
ask is not a weak audit, it is one that did not look at the destination.

The first question a maintainer would have asked is "how is this different from
#26?", and the draft had no answer because it did not know #26 existed.

## The open question this reply answers

Ericson2314, 2026-07-23, into #26:

> `nix flake check` no longer requires XP features on the main store --- it
> will silently skip stuff if dynamic derivations is not enabled. The VM tests
> are now also included in that command, and those will actually test Nix
> Ninja in that case.
>
> Does that mean we don't need this anymore, I think?

He is right about what `nix flake check` now does, and the answer is still no,
for a reason that is visible in this repository's own flake and is measurable
rather than arguable.

## The reply

> `nix flake check` closed the VERIFICATION half of this and left the
> ITERATION half open, and the split is visible in `modules/flake/packages.nix`
> rather than being a matter of opinion.
>
> The seven examples sit behind `lib.optionalAttrs (builtins ? outputOf)`. That
> is a feature probe, so on a store without `dynamic-derivations` the examples
> are not present-but-unbuildable, they are absent from the attribute set.
> Measured both directions against nix 2.36.0pre:
>
> ```
> builtins ? outputOf   without dynamic-derivations  ->  false
> builtins ? outputOf   with    dynamic-derivations  ->  true
> ```
>
> So a contributor whose main store has no experimental features can run
> `nix flake check`, get a green result because the NixOS tests supply the
> features inside the guest, and still not be able to run
> `nix build .#example-nix` - that attribute does not exist for them. The same
> goes for running `cargo test`, or driving any target by hand, against a
> daemon that can serve dynamic derivations.
>
> That is the thing #26 was opened for, and it is the sentence at the end of
> `CONTRIBUTING.md`'s "Developing locally": not "does nix-ninja work", which
> `nix flake check` now answers, but "let me drive it while I am changing it",
> which nothing in the repository answers today.
>
> Your earlier point in this thread still holds and is the general form of it:
> one may not want experimental features enabled on their main store at all,
> independently of whether the test suite needs them.

## What is offered, said after the answer rather than before it

> If it is useful, I have a script for the iteration half that takes the
> `--store` approach you pointed jaen at, rather than `nix-portable`: a nix
> daemon on a throwaway store, running as the invoking user, from this
> repository's own `nix` flake input so a contributor gets the version already
> pinned here. `selftest`, `run -- <cmd>`, `stop`.
>
> It is smaller than #26 in scope - it does not attempt jaen's sync-back to the
> global store, which is the hard part of that PR and the reason it grew into a
> separate Rust tool. If sync-back is wanted, #26 and `nix-dev-wrapper` are
> ahead of anything I have.
>
> Two settings in it are worth having regardless of whose script wins, because
> both fail by appearing to blame the client and each cost an afternoon:
> `trusted-users` must name the connecting user in the DAEMON's config, and
> `--extra-experimental-features` must be on the DAEMON's own argv rather than
> in the config file it was pointed at. Both produce
> `experimental Nix feature 'ca-derivations' is disabled`, which reads as the
> client missing a feature it demonstrably has.

## Why the offer is phrased as smaller than #26

Because it is. jaen's PR and `nix-dev-wrapper` solve store sync-back;
`devstore.sh` does not attempt it. Claiming to supersede #26 would be false and
would be caught by the person who wrote it, in a thread where they are still
active. Saying plainly which half each covers is the only version of this that
survives that reader.

## Audit

Round 1 (2026-08-21), drafted and attacked in the same pass:

- The first draft answered Ericson2314 and did not mention jaen, whose PR it
  is. Fixed: jaen's prior work is named first and credited with the harder
  half.
- The first draft asserted flake check "does not cover iteration" without
  evidence. Fixed: the `builtins ? outputOf` gate is quoted from the
  repository's own file and measured in both directions.
- The first draft led with the offer. Fixed: the maintainer's question is
  answered first, and the offer is a separate section after it, because a
  reply that answers a question by advertising is the shape that gets skimmed.

Not attacked by an independent reader; rule 10 requires that before sending.
Specifically unaudited: whether jaen or the maintainer would read the "smaller
than #26" framing as accurate, which is a judgment about their work rather
than ours.
