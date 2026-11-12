#!/usr/bin/env bash
# Builds the three WASM artifacts:
#   1. vault contract      -> wasm32-unknown-unknown (deploy to NEAR testnet)
#   2. strategy            -> wasm32-wasip1          (upload to OutLayer)
#   3. proofs verifier     -> wasm32-unknown-unknown + wasm-bindgen -> frontend/pkg
#
# Prereqs: rustup target add wasm32-unknown-unknown wasm32-wasip1
#          cargo install wasm-bindgen-cli   (for the browser verifier)
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== 1/3 vault contract =="
# -s drops the ~350 KB name section (code storage is staked NEAR: 1 NEAR / 100 KB)
RUSTFLAGS="--cfg near -C link-arg=-s" cargo build -p vault-contract --target wasm32-unknown-unknown --release
CONTRACT=target/wasm32-unknown-unknown/release/vault_contract.wasm
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --strip-debug --strip-producers --enable-bulk-memory --enable-bulk-memory-opt --enable-sign-ext --enable-mutable-globals --enable-nontrapping-float-to-int -o "$CONTRACT.opt" "$CONTRACT" && mv "$CONTRACT.opt" "$CONTRACT"
fi
ls -la "$CONTRACT"

echo "== 2/3 strategy (OutLayer enclave binary) =="
cargo build -p vault-strategy --target wasm32-wasip1 --release
STRATEGY=target/wasm32-wasip1/release/strategy.wasm
sha256sum "$STRATEGY" | tee strategy.wasm.sha256
echo "   ^ this sha256 is APPROVED_WASM_HASH — pass it to the vault's new()"

echo "== 3/3 browser proof verifier =="
cargo build -p vault-proofs --target wasm32-unknown-unknown --release --features wasm
wasm-bindgen target/wasm32-unknown-unknown/release/vault_proofs.wasm \
  --out-dir frontend/pkg --target web --no-typescript
ls -la frontend/pkg/

echo
echo "all three artifacts built."
echo "deploy order: strategy -> OutLayer (scripts/outlayer-setup.md), then contract with that hash."
