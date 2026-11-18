#!/usr/bin/env bash
# EVERY OPTION THE SHIPPED ninja OFFERS IS ACCEPTED, OR LISTED AS REFUSED.
#
# An option this driver does not implement makes clap exit 2 having built
# NOTHING, and a consumer that does not read the exit status takes that as an
# answer. meson-python's `_work_to_do` runs a dry run, captures stdout without
# checking the status, greps for ninja's no-work sentinel, finds nothing, and
# rebuilds on every import - its own comment cites the same check in meson.
# Five options were rejected that way and NOTHING WAS WATCHING: the gap
# existed for as long as the drop-in has and surfaced only because one
# package's test suite happened to assert on it.
#
# The list is derived from `ninja --help` at RUN TIME rather than pinned here,
# so a ninja upgrade that adds an option fails this gate instead of being
# inherited silently.
#
# PROBED BEHAVIOURALLY, not by reading cli.rs: several flags are `hide = true`
# and would be invisible to a source scan while parsing perfectly well. The
# discriminator is clap's own rejection - exit 2 with "unexpected argument" -
# and `-t list` is used as the payload because it prints and exits without a
# daemon.
set -u
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
NINJA=${NINJA_BIN:-$(command -v ninja)}
[ -x "$NINJA" ] || { echo "no ninja on PATH; set NINJA_BIN" >&2; exit 2; }
NN=$(nix build --no-link --print-out-paths "$REPO#nix-ninja" 2>/dev/null) || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }

# Options this driver REFUSES ON PURPOSE, with the reason. An entry here is a
# decision; an option in neither list is an accident.
declare -A REFUSED=(
  [-n]="dry run: resolution without building, and what 'would run' means for
        outputs that may substitute is a design question rather than a print
        statement. Implementing it is tracked as class 24.
        THE REFUSAL IS DELIBERATE AND IT COSTS A PACKAGE, which this entry did
        not say until the cost was measured. meson-python's editable path runs
        `ninja -n`, CAPTURES STDOUT WITHOUT CHECKING THE STATUS, and treats
        everything except the sentinel `ninja: no work to do.` as work to do.
        A driver that refuses the flag answers rc=2 with an empty stdout, which
        that code cannot tell from a dirty tree, so the wheel rebuilds on every
        import. THE CONSUMER ROUTES AROUND IT: the shim hands `-n` to the real
        ninja on the same arm as `-t` and `--version`, because a dry run is a
        question about the GRAPH rather than a build.
        SO AN ENTRY HERE IS NOT SELF-CONTAINED: refusing a flag the consumer
        does not route around is a broken package. Add the routing arm in the
        same change as any new entry, and check the package that uses the flag
        rather than assuming the refusal is inert."
)

D=$(mktemp -d -t nn-flagsurface-XXXXXX); trap 'rm -rf "$D"' EXIT
printf 'rule r\n  command = true\nbuild x: r\n' > "$D/build.ninja"

# ninja prints options as "  -x ARG  text" or "  --long  text", plus the
# "-v, --verbose" pair form. Take the option tokens only.
opts=$("$NINJA" --help 2>&1 | grep -oE '(^|[ ,])(-[a-zA-Z]|--[a-z-]+)' |
       tr -d ' ,' | sort -u)

accepted=0; refused_ok=0; missing=0; other=0
echo "ninja: $("$NINJA" --version)   driver: $(git -C "$REPO" rev-parse --short HEAD)"
echo
for o in $opts; do
  # Options taking a value need one, or clap rejects for the wrong reason.
  case "$o" in
    -C) args=("$o" "$D") ;;
    -f) args=("$o" "$D/build.ninja") ;;
    -j|-k|-l) args=("$o" "1") ;;
    -d) args=("$o" "stats") ;;
    -w) args=("$o" "dupbuild=err") ;;
    -t) args=("$o" "list") ;;
    --version) args=("$o") ;;
    *)  args=("$o") ;;
  esac
  # STATUS AND OUTPUT BOTH, never the message alone. This read the stderr
  # for one string and discarded the status, so an option that failed for
  # ANY OTHER reason - an invalid value, a missing argument, a panic - had
  # no "unexpected argument" in its output and fell to the accepted branch.
  # That is the gate reproducing the defect it exists to catch: exit
  # nonzero having built nothing, read as an answer.
  # The harness supplies -C and -t itself, so probing EITHER of those
  # supplied them twice and clap rejected the duplicate. That is not a
  # parse refusal of the option and it is not acceptance: the gate simply
  # could not probe two of the options it reports on, and the old status-
  # blind read scored both as accepted - the right answer for the wrong
  # reason, which is why the count looked complete.
  pre=(-C "$D"); post=(-t list)
  case "$o" in
    -C) pre=() ;;
    -t) post=() ;;
  esac
  err=$("$NN/bin/nix-ninja" "${pre[@]}" "${args[@]}" "${post[@]}" 2>&1 >/dev/null)
  rc=$?
  if [ "$rc" = 0 ]; then
    printf '  %-12s accepted\n' "$o"; accepted=$((accepted+1))
  elif printf '%s' "$err" | grep -q "unexpected argument"; then
    if [ -n "${REFUSED[$o]:-}" ]; then
      printf '  %-12s REFUSED on purpose\n' "$o"; refused_ok=$((refused_ok+1))
    else
      printf '  %-12s NOT ACCEPTED and not listed as refused\n' "$o"
      missing=$((missing+1))
    fi
  else
    # Parsed but did not run, which is neither of the two states this gate
    # was written to tell apart and is reported rather than absorbed.
    printf '  %-12s rc=%s, NOT a parse refusal: %s\n' "$o" "$rc" \
      "$(printf '%s' "$err" | head -1)"
    other=$((other+1))
  fi
done

echo
echo "accepted $accepted, refused on purpose $refused_ok, unaccounted $missing," \
     "failed for another reason $other"
if [ "$other" != 0 ]; then
  echo
  echo "FAIL: an option was accepted by the parser and the run then failed."
  echo "  The gate cannot call that accepted: the caller gets a nonzero"
  echo "  status and no output, which is the shape it exists to detect."
  exit 1
fi
if [ "$missing" != 0 ]; then
  echo
  echo "FAIL: an option ninja offers is neither accepted nor listed as a"
  echo "  deliberate refusal. Accept it, or add it to REFUSED with the reason."
  echo "  Leaving it makes the driver exit 2 having built nothing, which a"
  echo "  caller that ignores the status reads as an answer."
  exit 1
fi
echo "PASS: every option the shipped ninja offers is accounted for."
