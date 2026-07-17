#!/usr/bin/env bash
# Runs a cargo test invocation and fails the job if any named --test binary
# reported "running 0 tests". Integration test files in wavis-backend are
# gated with #![cfg(feature = "test-support")] (and similar), so a missing
# feature flag lets the file compile to an empty binary that still exits 0 —
# a silently ghost-green suite. Real test failures still fail immediately via
# pipefail; this only adds a check for the "0 tests ran" case that would
# otherwise pass.
#
# The "Doc-tests <crate>" section legitimately reports "running 0 tests" when
# a crate has no doc tests (true here) — that block is excluded, everything
# else reporting 0 tests is treated as phantom coverage.
set -euo pipefail

log="$(mktemp)"
"$@" 2>&1 | tee "$log"

clean_log="$(mktemp)"
sed -E 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$log" > "$clean_log"

if awk '
  /Doc-tests/ { in_doctest = 1; next }
  /Running / { in_doctest = 0 }
  /running 0 tests/ && !in_doctest { found = 1 }
  END { exit(found ? 0 : 1) }
' "$clean_log"; then
  echo "::error::A named test binary reported 'running 0 tests' — check for a missing --features flag or a #![cfg(...)] gate silently skipping the whole suite (ghost-green coverage)."
  exit 1
fi
