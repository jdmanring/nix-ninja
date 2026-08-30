# DRAFT: nix's boost override collides with nixpkgs' own context patches

Not sent. Target is **NixOS/nix**. Not nix-ninja: nothing in pdtpartners'
tree causes it or can fix it. It reaches this fork only because
`modules/flake/overlays.nix:96` and `:100` build
`inputs.nix.packages.<sys>.nix`.

## The bug

`packaging/dependencies.nix:120` overrides boost with a `patches` argument:

    boost = (pkgs.boost.override {
      extraB2Args = [ ... ];
      patches = [ ./patches/0001-Fix-uncaught_exceptions-not-accounting-for-forced_un.patch ];
      enableIcu = false;
    })

nixpkgs' `boost/generic.nix:168` does `patches = patches ++ ...`, so the
argument is PREPENDED to nixpkgs' own list rather than replacing it. nixpkgs
at `nixos-26.05` carries two fetched boostorg/context patches whose order is
required: `0921b9fd` adds `BOOST_NOINLINE` to `manage_exception_state`'s
constructor and destructor, and `5883212` rewrites those same lines.

Prepending makes nix's patch apply to the un-annotated file first, after which
`0921b9fd` no longer matches:

    applying patch .../0001-Fix-uncaught_exceptions-not-accounting-for-forced_un.patch
    patching file boost/context/fiber_fcontext.hpp
    applying patch .../cmake-paths-188.patch
    applying patch .../0921b9fd5c776aec7748475c6c10807e0d51bc6d.patch
    patching file boost/context/fiber_fcontext.hpp
    Hunk #1 FAILED at 87.

## Why the fix is to pass nothing

The patch nix carries is the SAME FIX that nixpkgs already applies, rebased.
Not the same file: its header reads

    From 5883212311535a0046031d74d1568ae173c1e35b
    Subject: [PATCH] Fix uncaught_exceptions() not accounting for forced_unwind

but a `From` header survives any amount of hand-editing, so the header is the
weakest available evidence and on its own it establishes nothing. Compared
against `boostorg/context` commit `5883212` as fetched by nixpkgs:

    upstream        144 lines, two files (fiber_fcontext.hpp and
                    test/test_fiber.cpp), paths under a/include/boost/context/
    nix's copy      102 lines, hpp only, paths under a/boost/context/

It is a rebased, path-rewritten, test-stripped derivative. What makes it the
same fix is that the ADDED lines of the hpp hunks are byte-identical - 27
lines, `diff` clean.

**And the rebase is the root cause, stated properly.** nix's copy carries
context lines from BEFORE `0921b9fd`: it expects a bare
`manage_exception_state()` where nixpkgs, having applied `0921b9fd` first,
has `BOOST_NOINLINE manage_exception_state()`. Prepending a patch cut against
the older text is what breaks the pair.

Deleting the `patches` argument restores nixpkgs' ordered pair, which already
contains this fix. Nothing is lost.

## Scope

Anything pairing a current nix with a nixpkgs carrying both patches. It is not
specific to this fork, and it is not caused by anything in nix-ninja.

## What is verified and what is not

Verified by build here on 2026-08-29: the failure, the patch order in the log,
the commit hash in nix's vendored patch, the identical hash among nixpkgs'
fetched patches, and that NixOS/nix master still carries the override, read
from the forge.

NOT verified: that removing the argument produces a working boost. It has not
been tested, and the earlier reason given here - that this fork *cannot* test
it - is no longer true and should not be repeated. `flake.nix:29` now reads
`inputs.nixpkgs.follows = "nixpkgs-for-nix"`, pinned at `flake.nix:26` to a
pre-collision revision as a workaround. Repointing that input at `nixpkgs`
and dropping the `patches` argument reproduces the failure and tests the
remedy here. "Did not" is the honest word, not "cannot".

ArtNix independently hit the same failure and fixed it by dropping `patches`
from the override in its own package set, which is evidence for the remedy
from a different tree.

## Audit

Round 1 (2026-08-29):

- The provenance claim was made once before and withdrawn as unverified, and
  it is now verified by reading the patch header rather than inferring from
  the filename. Worth noting in the body only as certainty, not as a story.
- The "which revisions does this bite" question was asked and answered, and
  the answer is a BOOST VERSION RANGE rather than a nixpkgs range, which is
  the sharper form. In `boost/generic.nix`, `0921b9fd` is gated
  `>= 1.88 && < 1.92` and `5883212` is gated `>= 1.88 && < 1.93`. nix's
  `boost` is `boost189`, so both apply and the collision is structural for
  boost 1.88 through 1.91 on any nixpkgs carrying that backport, which
  landed in `b5e044308f12` on 2026-08-02.
- Still owed: run the remedy. See above - it is now a repoint of one input,
  not an impossibility.

Status: NOT SENDABLE, pending an actual test of the proposed remedy, which is
one input repoint away.
