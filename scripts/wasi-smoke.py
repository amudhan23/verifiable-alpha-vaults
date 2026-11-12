#!/usr/bin/env python3
"""Run the built strategy.wasm exactly as OutLayer does (WASI p1: stdin input,
secret as env var, stdout output) and check the commitment recomputes.
Prereqs: pip install wasmtime; cargo build -p vault-strategy --target wasm32-wasip1 --release
"""
import hashlib, json, tempfile, sys
from wasmtime import Store, Module, Linker, Engine, WasiConfig

WASM = "target/wasm32-wasip1/release/strategy.wasm"
INPUT = {
    "epoch": 1, "nonce": 7, "pair": "ETH/USDC",
    "prices": [100, 100, 100, 100, 100, 102, 105, 109, 114, 120],
    "nav_yocto": "990", "peak_nav_yocto": "1000",
    "max_leverage_bps": 30000, "max_drawdown_bps": 2000,
}
PARAMS = {"fast": 3, "slow": 8, "threshold_bps": 50, "size_bps": 4000, "leverage_bps": 25000}

engine = Engine()
module = Module.from_file(engine, WASM)
stdin = tempfile.NamedTemporaryFile("w", delete=False, suffix=".json")
stdin.write(json.dumps(INPUT)); stdin.close()
stdout_path = tempfile.mktemp()

wasi = WasiConfig()
wasi.stdin_file = stdin.name
wasi.stdout_file = stdout_path
wasi.env = [("STRATEGY_PARAMS", json.dumps(PARAMS))]
wasi.inherit_stderr()

store = Store(engine); store.set_wasi(wasi)
linker = Linker(engine); linker.define_wasi()
linker.instantiate(store, module).exports(store)["_start"](store)

out = json.load(open(stdout_path))
pre = out["sealed"]["decision_json"].encode() + bytes.fromhex(out["sealed"]["salt_hex"])
assert hashlib.sha256(pre).hexdigest() == out["commitment"], "commitment mismatch"
assert out["risk_bundle"] is not None, "no risk proof from an honest run"
print("OK: strategy.wasm runs under WASI; decision =", json.loads(out["sealed"]["decision_json"])["action"],
      "; commitment recomputes; Bulletproofs generated inside the wasm")
