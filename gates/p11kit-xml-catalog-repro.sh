#!/usr/bin/env bash
# p11-kit's class: a task that reaches for the NETWORK because the variable
# redirecting a URL to a local file did not cross into it.
#
# Round 14, configuration B:
#   I/O error: failed to load
#   "http://docbook.sourceforge.net/release/xsl/current/manpages/docbook.xsl"
#   Attempt to load network entity
#
# XML_CATALOG_FILES names the catalogs that rewrite that URL to a docbook-xsl
# store path. The outer build has it from a setup hook; the task environment is
# an allowlist of NIX_* wrapper variables and did not. xsltproc does not fail
# when the catalog is missing, it falls through to fetching, so the sandbox's
# refusal is the first symptom and it names the network rather than the
# variable.
#
# Driven here as a plain ninja edge, which is enough: the subject is the task
# ENVIRONMENT, and one edge carries it exactly as ten thousand do.
#
#   with the variable forwarded  -> the stylesheet resolves, xsltproc exits 0
#   without it                   -> Attempt to load network entity
#
# The control arm is the point, and here it is free: run the same script with
# NN_DROP_CATALOG=1 and the harness unsets the variable in the outer
# environment, so the driver has nothing to forward and the pre-fix behaviour
# is reproduced without a second driver.
#
#   local/gates/p11kit-xml-catalog-repro.sh <driver-bin>
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
NN=${1:?usage: p11kit-xml-catalog-repro.sh <driver-bin>}
[ -x "$NN" ] || { echo "not executable: $NN" >&2; exit 2; }

b() { nice -n 10 nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
TASK=$(b "$REPO#nix-ninja-task")
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); BU=$(px binutils)
XSLT=$(px libxslt.bin); DBX=$(px docbook_xsl_ns); XML=$(px libxml2.bin)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-p11-XXXXXX); trap 'chmod -R u+w "$D" 2>/dev/null; rm -rf "$D"' EXIT
# A FIXED DATE, or the man page embeds today's (`.\"      Date: 10/03/2026`)
# and the task's output changes every day. With a BuildTraceV3 row naming a
# collected earlier output, a rebuild with different bytes is refused by the
# daemon ("already have another one locally"); that failed this arm on
# 2026-10-03. A reproducible output never meets that refusal.
cat > "$D/page.xml" <<'EOF'
<?xml version="1.0"?>
<!DOCTYPE refentry PUBLIC "-//OASIS//DTD DocBook XML V4.5//EN"
  "http://www.oasis-open.org/docbook/xml/4.5/docbookx.dtd">
<refentry id="probe"><refentryinfo><date>2000-01-01</date></refentryinfo><refmeta><refentrytitle>probe</refentrytitle>
<manvolnum>1</manvolnum></refmeta><refnamediv><refname>probe</refname>
<refpurpose>a page that exists to be transformed</refpurpose></refnamediv>
</refentry>
EOF
# The stylesheet is named by URL, exactly as p11-kit's does. Only the catalog
# turns it into a local path; nothing else in the task can.
cat > "$D/build.ninja" <<EOF
rule man
  command = xsltproc --nonet -o \$out http://docbook.sourceforge.net/release/xsl-ns/current/manpages/docbook.xsl \$in
build probe.1: man page.xml
default probe.1
EOF

CAT=$DBX/xml/xsl/docbook/catalog.xml
export XML_CATALOG_FILES="$CAT"
[ "${NN_DROP_CATALOG:-0}" = 1 ] && unset XML_CATALOG_FILES

set +e
out=$( cd "$D" && env \
  PATH="$(dirname "$NN"):$XSLT/bin:$XML/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="extra-experimental-features = $F" \
  "$NN" probe.1 2>&1 )
rc=$?
set -e
echo "$out" | tail -12
echo "catalog in outer env: ${XML_CATALOG_FILES:-<unset>}"
echo "exit $rc"
if [ "$rc" = 0 ] && [ -s "$D/probe.1" ]; then
  echo "VERDICT    the stylesheet resolved locally and the page was transformed"
elif printf '%s' "$out" | grep -qi 'network entity\|failed to load'; then
  echo "VERDICT    REPRODUCED - the task reached for the network"
else
  echo "VERDICT    failed for some other reason; read the output above"
fi
