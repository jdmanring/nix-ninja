# Security Policy

## Reporting a vulnerability

Report suspected vulnerabilities privately, not in a public issue, and not in a
pull request. Use GitHub's [private vulnerability reporting][advisory] on this
repository, or email the maintainer at james_manring@yahoo.com if you would
rather not use GitHub.

Include what you can: the affected revision, a description, and the smallest
input that reproduces it. A `build.ninja` fragment plus the command that drives
it is ideal.

You can expect an acknowledgment within a few days. This is a
single-maintainer project, so a fix may take longer; you will be told if that is
the case rather than left waiting.

## Scope

nix-ninja reads a `build.ninja` and emits Nix derivations, then builds them
inside a sandbox. The areas worth scrutiny:

- **Task derivations do not use `stdenv.mkDerivation`.** `nix-ninja-task`
  materialises inputs and places outputs itself. A path that escapes the build
  directory, or an input placed outside the writable tree, is a defect worth
  reporting.
- **The include scanner reads the source tree**, and a reader added for a new
  file class could be made to carry a file it should not.
- **The daemon protocol client** speaks `builder-rpc-v0` to a Nix daemon. A
  malformed or hostile derivation response is in scope.

## Out of scope

- Anything requiring a hostile `nix` daemon or a hostile Nix store: the daemon is
  trusted by construction.
- Experimental Nix features. The project depends on `dynamic-derivations`,
  `ca-derivations` and `recursive-nix`, all unstable by upstream's own
  definition; a defect in them belongs to Nix, not here.
- Reports about the vendored `vendor-n2/`, which is n2 by Evan Martin and
  carries its own license. Those go to n2's own tracker; `vendor-n2/VENDORING.md`
  records the revision vendored here.

Reports are welcome by email even where the answer is that the issue is already
known.

[advisory]: https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability
