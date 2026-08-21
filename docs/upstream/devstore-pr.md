# DRAFT PR: contrib/devstore.sh, answering CONTRIBUTING's own request

Not sent. Sessions stage; James sends. This is item 7 in `README.md`'s order.

**Send this one FIRST, ahead of everything else in this directory.** It is the
only item that answers a request the maintainer wrote down himself, it touches
no crate, and it cannot conflict with anything. Every other item asks him to
take code he did not ask for; this one asks him to take code he did.

## What it is

`CONTRIBUTING.md` ends its "Developing locally" section with:

> If there's a good UX way of iterating on `nix-ninja` in a tmp store and
> without modifying your main nix, please contribute!

The documented alternative is to make a patched nix your system daemon, which
is a large thing to ask and impossible on a shared machine. `contrib/devstore.sh`
runs an unprivileged daemon on a throwaway store instead: `selftest`, `run --`,
`stop`.

## Description

> This is the thing `CONTRIBUTING.md` asks for at the end of "Developing
> locally". It runs a nix daemon on a throwaway store as the invoking user, so
> iterating on nix-ninja no longer requires making a patched nix your system
> daemon.
>
> The daemon comes from this repository's own `nix` flake input, so a
> contributor gets the version already pinned here rather than one the script
> picked.
>
> **The two settings are the contribution as much as the script is, because
> both fail by appearing to blame the client.** Each cost an afternoon:
>
> - `trusted-users` must name the connecting user in the DAEMON's config.
>   Without it the daemon discards client settings and reports `experimental
>   Nix feature 'ca-derivations' is disabled` - which reads as the client
>   missing a feature it demonstrably has, so the obvious next move (pass
>   `--extra-experimental-features` to the client) changes nothing;
> - `--extra-experimental-features` must be on the DAEMON's own argv. It does
>   not read them from the `experimental-features` line of the config file it
>   was pointed at. The error text is identical to the first, so the two are
>   indistinguishable from the message and fixing one leaves the other.
>
> Two things I concluded before finding those, in case anyone else gets there:
> that an unprivileged daemon cannot serve CA derivations, and that a
> `local?root=` store does not support ca-derivations. Both false. The obstacle
> that IS real is that a daemon left on the default store cannot open the
> root-owned `/nix/var/nix/db/big-lock`, which is why the store has to move.
>
> `selftest` runs two probes. The second is worth explaining because its
> verdict looks inverted: `builder-rpc-v0` deliberately leaves `$out` unset and
> an ordinary builder never calls `SubmitOutput`, so a daemon that HAS the
> feature produces a FAILING build. The probe asserts the positive signature
> (`failed to submit output path`) rather than the absence of the refusal,
> because an absence test also passes when the daemon is gone, the socket is
> refused, or the expression is malformed. Every dynamic task requires that
> feature (`crates/nix-ninja/src/task.rs`), so a daemon without it otherwise
> fails much later with a message that does not name the cause.
>
> Two disclosures rather than surprises in the diff:
>
> - **builds in this store are UNSANDBOXED.** It has no build users group and
>   runs as the invoking user, so the host filesystem is visible to builds.
>   Fine for iteration, wrong for anything anyone else consumes; the script and
>   the docs both say so.
> - **the `CONTRIBUTING.md` patch also flags a stale pin.** The existing
>   snippet pins `nix@d904921`, and `flake.lock` resolves the `nix` input to
>   `bcd3bec`. I did not change the snippet - which of those you want as the
>   documented version is your call - but a reader currently gets two answers
>   three paragraphs apart, so the new section says the lock is what builds.

## Verification to quote if asked

Falsified in three states, not argued:

- fresh store: both probes pass;
- `NIX_NINJA_DEVSTORE_DAEMON` pointed at nix 2.35.2: probe 2 fails and prints
  the daemon's whole advertised feature set, which contains `ca-derivations`
  and `recursive-nix` and not `builder-rpc-v0`;
- a nonexistent daemon path: `COULD-NOT-RUN`, exit 2, rather than a pass.

## Audit

Round 1 (2026-08-21), drafted and attacked in the same pass:

- **"so nothing new is fetched" was false anywhere but the machine it was
  written on.** True there only because that nix was already realized; on a
  contributor's box the input substitutes or source-builds. Both the script
  header and the docs now say "the version already pinned here", which survives
  being read elsewhere. This is the exact class the rest of this directory
  spent the day on: a claim about one configuration standing in for a claim
  about the world;
- **`sandbox = false` shipped with no stated reason**, which is the first thing
  a nix-minded reviewer asks. Disclosed in the script, the docs and above;
- **the pin disagreement was found rather than introduced**, but a patch adding
  a second daemon recipe without noticing it would have made the file worse
  silently;
- **the item had no row in `README.md`'s table** while every other item had
  one. A `CONTRIBUTING.md` edit arriving inside an unrelated PR is the
  found-in-the-diff shape `pr-plan.md` already calls smuggling.

Not attacked by an independent reader; rule 10 requires that before sending.

Status: NOT SENDABLE pending the independent audit. It is the closest to
sendable of anything here, and it is not blocked on the pre-PR question that
holds the rest.
