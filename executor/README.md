# Executor

Relays attested decisions: enclave → vault → execution. No build step.

```bash
node test.mjs                      # offline tests (4)
node run.mjs                       # full cycle, offline (local enclave + dry-run vault)
npm install                        # only needed for the testnet modes below
node run.mjs --enclave outlayer --vault near                  # testnet, simulated fill
node run.mjs --enclave outlayer --vault near --execute evm    # relayed: HTTPS enclave call, vault-signed swap
node run.mjs --enclave onchain  --vault near --execute evm    # attested: the vault requests the run on-chain
```

Two modes, matching the contract's:

- **`--enclave outlayer` (relayed).** The executor calls OutLayer over HTTPS and relays the output: commit, reveal, proof, bounded settlement. It pays the storage for every write (excess refunded).
- **`--enclave onchain` (attested).** The executor calls `vault.request_decision` (0.1 NEAR storage reserve plus about 0.0075 NEAR for OutLayer, mostly refunded). The vault asks OutLayer on-chain; a DCAP-verified TDX worker answers into the vault's own callback, which commits, anchors the proofs and applies the settlement. The executor can only reveal a decision whose hash is already fixed, and broadcast what the vault signs.

In both modes the trade's size, slippage floor and fee tier come from the enclave's decision. The vault builds and MPC-signs the transaction to its own fixed Sepolia address; the executor checks the hash and the pool quote, then broadcasts.

Testnet env: `NEAR_ACCOUNT_ID`, `NEAR_PRIVATE_KEY`, `VAULT_CONTRACT_ID`,
`OUTLAYER_PAYMENT_KEY` (relayed), `SEAL_KEY_HEX` (to unseal decisions for the reveal).

New vaults: `node deploy.mjs` derives the vault's own EVM address (fund it once with Sepolia ETH). Upgrades: `migrate.mjs` (deploy + migrate atomically) while the vault account still has a key; afterwards only `propose_code` → 48h → `apply_code`.
