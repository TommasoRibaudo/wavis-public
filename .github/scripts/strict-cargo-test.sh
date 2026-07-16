#!/usr/bin/env bash
# Runs a cargo test invocation and fails the job if any named --test binary
# reported "running 0 tests". Integration test files in wavis-backend are
# gated with #![cfg(feature = "test-support")] (and similar), so a missing
# feature flag lets the file compile to an empty binary that still exits 0 —
# a silently ghost-green suite. Real test failures still fail immediately via
# pipefail; this only adds a check for the "0 tests ran" case that would
# otherwise pass.
set -euo pipefail

log="$(mktemp)"
"$@" 2>&1 | tee "$log"

if grep -q "running 0 tests" "$log"; then
  echo "::error::A named test binary reported 'running 0 tests' — check for a missing --features flag or a #![cfg(...)] gate silently skipping the whole suite (ghost-green coverage)."
  exit 1
fi
