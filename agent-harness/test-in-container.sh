#!/usr/bin/env bash
# test-in-container.sh — run the workspace's tests and clippy on Linux with
# the minimum supported Rust (RUST_IMAGE from images.env) and real CBMC
# installed (CBMC_PACKAGE). AGENT_HARNESS_REQUIRE_CBMC=1 makes the CBMC
# integration tests fail instead of skipping if CBMC were missing.
#
# Usage: ./test-in-container.sh [cargo test args…]   (default: --workspace)
#   e.g. ./test-in-container.sh -p agent-harness-tools-cbmc
#
# Needs docker. Writes only target/container-*/ under this directory and a
# named docker volume for the cargo registry cache.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=images.env
source "$ROOT/images.env"
ARCH="$(docker version --format '{{.Server.Arch}}')"

docker run --rm \
  -v "$ROOT":/src -w /src \
  -v agent-harness-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR="/src/target/container-$ARCH" \
  -e AGENT_HARNESS_REQUIRE_CBMC=1 \
  -e CBMC_PACKAGE="$CBMC_PACKAGE" \
  "$RUST_IMAGE" \
  sh -euc '
    apt-get -qq update >/dev/null
    apt-get -qq install -y --no-install-recommends cmake "$CBMC_PACKAGE" >/dev/null
    rustup component add clippy >/dev/null 2>&1
    echo "test-in-container: $(rustc --version); $(cbmc --version | head -1) at $(command -v cbmc)"
    [ "$#" -gt 0 ] || set -- --workspace
    cargo test --locked "$@"
    cargo clippy --workspace --all-targets --locked --quiet -- -D warnings
    echo "test-in-container: clippy ok"
  ' test-in-container "$@"
