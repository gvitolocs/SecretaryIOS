#!/usr/bin/env bash
# The Stateright models (verification/models), as scripts/verify.sh runs them, without --deep: CI's unit models/stateright.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo test -j"${CARGO_JOBS:-2}" --locked -q -p verification-models
