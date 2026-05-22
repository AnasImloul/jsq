#!/usr/bin/env bash
#
# Builds the Rust engine and stages libengine.a + engine.h at a stable path
# that Xcode picks up via LIBRARY_SEARCH_PATHS / HEADER_SEARCH_PATHS.
#
# Designed to be invoked by Xcode's "Run Script" build phase, but also
# safe to run directly from a terminal.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
ENGINE_DIR="${PROJECT_ROOT}/engine"

# Xcode strips PATH. Add the standard Rust install location and Homebrew.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found on PATH. Install Rust via https://rustup.rs/" >&2
    exit 1
fi

PROFILE="release"
TARGET="aarch64-apple-darwin"

(
    cd "${ENGINE_DIR}"
    cargo build --"${PROFILE}" --target "${TARGET}"
)

OUT_DIR="${ENGINE_DIR}/build"
mkdir -p "${OUT_DIR}/include"

LIB_SRC="${ENGINE_DIR}/target/${TARGET}/${PROFILE}/libengine.a"
LIB_DST="${OUT_DIR}/libengine.a"
if [ ! -f "${LIB_DST}" ] || [ "${LIB_SRC}" -nt "${LIB_DST}" ]; then
    cp -f "${LIB_SRC}" "${LIB_DST}"
fi

HDR_SRC="${ENGINE_DIR}/include/engine.h"
HDR_DST="${OUT_DIR}/include/engine.h"
if [ ! -f "${HDR_DST}" ] || [ "${HDR_SRC}" -nt "${HDR_DST}" ]; then
    cp -f "${HDR_SRC}" "${HDR_DST}"
fi

echo "engine: ${LIB_DST}"
