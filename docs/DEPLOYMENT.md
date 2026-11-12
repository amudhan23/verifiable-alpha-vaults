# Live testnet deployment (Wed 11 Nov 2026, ~21:50 ET)

| What | Where |
|---|---|
| Vault contract | `vault.alpha-vaults-23.testnet` ([explorer](https://testnet.nearblocks.io/address/vault.alpha-vaults-23.testnet)) |
| OutLayer project | `alpha-vaults-23.testnet/alpha-vault-strategy` |
| Approved/attested strategy wasm | `acb0139065a6297979e955940247ce4303cd189b146603a595012c6c7bf08ad4` |
| Strategy wasm (FastFS) | https://test.fastfs.io/alpha-vaults-23.testnet/outlayer.testnet/acb0139065a6297979e955940247ce4303cd189b146603a595012c6c7bf08ad4.wasm |
| Sealed params | OutLayer secret `STRATEGY_PARAMS`, profile `default` (encrypted on-chain; decrypted only in the enclave) |
| First attested run (go/no-go) | call `c47fe5dd-a57c-4f83-84b2-bc1ce7e96215`, 923 ms, TDX quote verified |
| Frontend live mode | `?vault=vault.alpha-vaults-23.testnet` |

Go/no-go verdict: **GO** (Wed night, 14h ahead of the deadline). All three
browser checks green against live chain data: 3 decisions attested, 3/3
commitments recomputed, 3 Bulletproof bundles verified in-browser.

![Live vault, all checks green](img/live-vault.png)

Run a new attested cycle (needs the executor env, see `executor/.env.example`):
```bash
cd executor && node run.mjs --enclave outlayer --vault near
```
