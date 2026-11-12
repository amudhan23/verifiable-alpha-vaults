# OutLayer testnet setup (one-time, ~20 min, do before the go/no-go)

Everything here happens on **testnet** (`outlayer.testnet`, base URL
`https://testnet-api.outlayer.ai`).

## 0. Prereqs
- A NEAR **testnet** account (e.g. `amudhan.testnet`) with a little NEAR.
- `jq`, `xxd`, `curl` locally (for `scripts/go-no-go.sh`).
- Rust with the `wasm32-wasip1` target: `rustup target add wasm32-wasip1`.

## 1. Build the strategy WASM
```bash
cargo build -p vault-strategy --target wasm32-wasip1 --release
sha256sum target/wasm32-wasip1/release/strategy.wasm   # <- this is approved_wasm_hash
```

## 2. Deploy the project on OutLayer
Install the CLI (`cargo install outlayer-cli`), log in once with the throwaway
testnet key, then everything (upload, deploy, secrets, payment keys) works
from the terminal:
```bash
outlayer login testnet        # paste the NEAR account + full-access key
outlayer keys create          # payment key for the HTTPS API
outlayer secrets set STRATEGY_PARAMS='{"fast":3,"slow":8,"threshold_bps":50,"size_bps":4000,"leverage_bps":25000}'
```
then:
```bash
outlayer upload target/wasm32-wasip1/release/strategy.wasm   # prints sha256 + FastFS URL
outlayer deploy alpha-vault-strategy <FastFS URL>
```
(Alternative: point the project at this GitHub repo — OutLayer clones and
builds it; `version_key: "amudhan23/verifiable-alpha-vaults@<commit>"` then
pins the exact build. Prefer this for the final submission so judges see the
source→hash chain.)

## 3. Store the sealed parameters (THE SECRET)
At <https://app.outlayer.ai/secrets> (testnet), profile `default`, add:

- name: `STRATEGY_PARAMS`
- value: `{"fast":3,"slow":8,"threshold_bps":50,"size_bps":4000,"leverage_bps":25000}`

Secrets are encrypted on-chain and decrypted only inside the TDX enclave,
where they appear to the WASM as environment variables.

## 4. Create a testnet Payment Key
At <https://app.outlayer.ai> (testnet) create a Payment Key
(format `owner:nonce:secret`) and fund the prepaid balance if required.

## 5. Run the go/no-go
```bash
OUTLAYER_PAYMENT_KEY="..." PROJECT_OWNER="amudhan.testnet" ./scripts/go-no-go.sh
```

**Decision rule:** if this doesn't PASS by noon Thursday, stop and switch to
the fallback idea (Proof of Goal). If it passes, record the `call_id` — it's
demo material.

## Open questions the go/no-go answers
- real sync latency (docs say "instant"; verify under load)
- attestation availability delay after completion
- whether the attestation JSON exposes the full output (if it does, the
  `sealed` field is publicly visible after the fact → acceptable for the demo
  since reveal follows seconds later, but note it in the README trust model;
  roadmap: encrypt `sealed` to the executor's key)

## Note on the canonical wasm hash
Local builds and CI builds of `strategy.wasm` produce different hashes
(non-reproducible paths). The **canonical approved_wasm_hash is the hash of
the binary OutLayer runs** — take it from the OutLayer project after
deployment (or from `outlayer upload`'s output) and pass that to the vault's
`new()`. The TDX attestation links source → that hash → every run.
