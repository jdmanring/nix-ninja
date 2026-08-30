#!/usr/bin/env bash
# Writes the header the compile edge includes. Nothing has written it when
# the driver scans main.cpp, which is the case under test.
set -eu
cat > "$1" <<'HDR'
#pragma once
#define GENERATED_GREETING "generated header example"
HDR
