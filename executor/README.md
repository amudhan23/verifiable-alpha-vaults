# Executor

Relays attested decisions: enclave → vault → execution. No build step.

```bash
node test.mjs                      # offline tests (4)
node run.mjs                       # full cycle, offline (local enclave + dry-run vault)
npm install                        # only needed for the testnet modes below
node run.mjs --enclave outlayer --vault near                  # testnet, simulated fill
node run.mjs --enclave outlayer --vault near --execute evm    # + Chain Signatures on Sepolia
```

Testnet env: `NEAR_ACCOUNT_ID`, `NEAR_PRIVATE_KEY`, `VAULT_CONTRACT_ID`,
`OUTLAYER_PAYMENT_KEY`, `OUTLAYER_PROJECT`, `APPROVED_WASM_HASH`.

For `--execute evm`: fund the derived Sepolia address once
(`node -e "import('./evm.mjs').then(async m => console.log(await new m.ChainSigExecutor({accountId: process.env.NEAR_ACCOUNT_ID, privateKey: process.env.NEAR_PRIVATE_KEY}).address()))"`).
