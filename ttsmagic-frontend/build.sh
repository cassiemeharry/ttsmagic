#!/usr/bin/env bash

set -euo pipefail
set -x

cd "$(dirname $0)/.."

cargo build --target wasm32-unknown-unknown --package ttsmagic-frontend --release

static="$(pwd)/ttsmagic-server/static"
target="$(pwd)/target/wasm32-unknown-unknown/release"
rm -f ttsmagic-server/static/ttsmagic_frontend*
wasm-bindgen --target web --no-typescript --out-dir="$static" "$target/ttsmagic_frontend.wasm"
cp "$target"/ttsmagic_frontend* "$static"
