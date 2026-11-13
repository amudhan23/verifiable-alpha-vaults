#!/usr/bin/env bash
# One command to check that the project builds, its tests pass, a full decision
# cycle runs offline, and the live testnet vault passes every read-only check.
# Needs: Rust (cargo) and Node 20+. No keys, no funds. Live checks need network
# access to NEAR testnet RPC, OutLayer's public API and an Ethereum Sepolia RPC;
# without it they are reported as SKIP, not FAIL.
set -u
cd "$(dirname "$0")/.."
results=()
step() { # name, command...
  local name="$1"; shift
  echo; echo "=== $name"
  if "$@"; then results+=("PASS  $name"); else results+=("FAIL  $name"); fi
}
need() { command -v "$1" >/dev/null || { echo "missing: $1 ($2)"; exit 2; }; }
need cargo "install Rust from https://rustup.rs"
need node "install Node.js 20+"

export PROPTEST_CASES="${PROPTEST_CASES:-128}"
step "Rust tests (contract, proofs, strategy, pipeline, fuzz)" cargo test --workspace --quiet
step "Executor dependencies" bash -c "cd executor && npm ci --silent --no-audit --no-fund"
step "Executor tests (incl. a full local-enclave cycle)" node executor/test.mjs
step "One full decision cycle, offline (decide, commit, reveal, guardrails, Bulletproofs)" node executor/run.mjs

echo; echo "=== Live read-only checks against vault.alpha-vaults-23.testnet"
if node -e "fetch('https://test.rpc.fastnear.com',{method:'POST',headers:{'content-type':'application/json'},body:'{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"status\",\"params\":[]}'}).then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"; then
  if node executor/verify.mjs; then results+=("PASS  Live checks: main vault"); else results+=("FAIL  Live checks: main vault"); fi
  if node executor/verify.mjs --vault vault2.alpha-vaults-23.testnet; then results+=("PASS  Live checks: attested rehearsal vault"); else results+=("FAIL  Live checks: attested rehearsal vault"); fi
else
  echo "NEAR testnet RPC unreachable from here: skipping live checks"
  results+=("SKIP  Live checks (no network)")
fi

echo; echo "=== Summary"
printf '%s\n' "${results[@]}"
! printf '%s\n' "${results[@]}" | grep -q '^FAIL'
