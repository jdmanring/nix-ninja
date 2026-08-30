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

The patch nix carries is not merely redundant in effect, it is the SAME
COMMIT. Its header reads:

    From 5883212311535a0046031d74d1568ae173c1e35b
    Subject: [PATCH] Fix uncaught_exceptions() not accounting for forced_unwind

and `5883212311535a0046031d74d1568ae173c1e35b` is exactly what nixpkgs fetches
as its second context patch. So nix vendors a copy of a patch its own nixpkgs
already applies, and applies it in the one order that breaks.

Deleting the `patches` argument restores nixpkgs' ordered pair, which includes
that commit. Nothing is lost. That is a stronger claim than "supply the right
order" and it is checkable from the patch header alone.

## Scope

Anything pairing a current nix with a nixpkgs carrying both patches. It is not
specific to this fork, and it is not caused by anything in nix-ninja.

## What is verified and what is not

Verified by build here on 2026-08-29: the failure, the patch order in the log,
the commit hash in nix's vendored patch, the identical hash among nixpkgs'
fetched patches, and that NixOS/nix master still carries the override, read
from the forge.

NOT verified: that removing the argument produces a working boost here. This
fork cannot test it - `inputs.nix.inputs.nixpkgs.follows = "nixpkgs"`, but
nix's flake imports nixpkgs itself, so an overlay in this tree never reaches
the `pkgs.boost` that `dependencies.nix` overrides. ArtNix independently hit
the same failure and fixed it by dropping `patches` from the override in its
own package set, which is evidence for the remedy but from a different tree.
Say that rather than implying we tested the patch we are proposing.

## Audit

Round 1 (2026-08-29):

- The provenance claim was made once before and withdrawn as unverified, and
  it is now verified by reading the patch header rather than inferring from
  the filename. Worth noting in the body only as certainty, not as a story.
- Not established: whether a `nixos-26.05` pin is the ONLY affected nixpkgs,
  or how long both patches have coexisted. A maintainer will ask which
  nixpkgs revisions this bites, and "the one we pin" is a weak answer.
  Cheap to improve and not yet done.

Status: NOT SENDABLE, pending the nixpkgs-range question above.
