#!/bin/bash
# Builds the @ironcalc/wasm package Forest depends on, as an npm tarball, without wasm-pack: the release build,
# wasm-bindgen for the web, then wasm-opt -O (binaryen, from this folder's dev dependencies), as wasm-pack does.
#
# Usage: forest_pkg.sh <version>   (e.g. 0.9.1-forest.2)
set -euo pipefail
version=${1:?usage: forest_pkg.sh <version>}
cd "$(dirname "$0")"
PYTHON=$(command -v python || command -v python3)
target=${CARGO_TARGET_DIR:-../../target}

pnpm install --frozen-lockfile
pnpm exec tsc types.ts --target esnext --module esnext
cargo build --release --target wasm32-unknown-unknown
rm -rf pkg
wasm-bindgen --target web --out-dir pkg --out-name wasm "$target/wasm32-unknown-unknown/release/wasm.wasm"
pnpm exec wasm-opt -O --all-features -o pkg/wasm_bg.wasm pkg/wasm_bg.wasm

cat > pkg/package.json <<EOF
{
  "name": "@ironcalc/wasm",
  "type": "module",
  "collaborators": ["Nicolas Hatcher <nicolas@theuniverse.today>"],
  "description": "IronCalc Web bindings",
  "version": "$version",
  "license": "MIT OR Apache-2.0",
  "repository": { "type": "git", "url": "https://github.com/wiva-hq/IronCalc" },
  "files": ["wasm_bg.wasm", "wasm.js", "wasm.d.ts"],
  "main": "wasm.js",
  "types": "wasm.d.ts",
  "sideEffects": ["./snippets/*"]
}
EOF
cp README.pkg.md pkg/README.md
"$PYTHON" fix_types.py pkg
"$PYTHON" fix_package.py pkg
cp ../../LICENSE-MIT ../../LICENSE-Apache-2.0 pkg/
"$PYTHON" third_party_notices.py --manifest-path Cargo.toml --name @ironcalc/wasm \
  --target wasm32-unknown-unknown \
  --include ../../base/THIRD-PARTY-NOTICES.md --output pkg/THIRD-PARTY-NOTICES.md
node --test tests/test_package.mjs
(cd pkg && npm pack --pack-destination ..)
