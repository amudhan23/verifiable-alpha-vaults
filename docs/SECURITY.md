# Security record

This vault holds other people's money, so we treat review as part of the
build. Testnet only; **not professionally audited**. What follows is our own
adversarial process and its results, stated plainly.

## Review 1 (12 Nov): independent adversarial review of v3

A separate reviewer with no hand in writing the code attacked v3. It found
**3 Critical, 2 High, 5 Medium, 2 Low**. Two of the Criticals came with
working exploit tests.

| # | Finding | v4 fix | Evidence |
|---|---|---|---|
| **C1** Critical | The executor could report any cross-chain value, and withdrawals paid `NAV × burn / total` out of the local pool. Inflating the value let an attacker drain the pool. | Withdrawals pay **exactly `burn/total` of the NEAR pool** plus `burn/total` of the shareholders' *external units* (a pro-rata claim on the cross-chain wallet). They never read the reported valuation. Settlements are bounded: NAV moves at most 10% per settlement, settlements are at least 1h apart, and everything is validated before any state is written. | Regression test `audit_c1_inflated_settlement_cannot_drain_the_pool` (30 maximal pumps; the attacker still gets exactly his ~1% pool slice). The fuzz invariant S5 checks this on every random withdrawal. |
| **C2** Critical | The MPC signature was requested by the *executor's* account, so the "vault's" EVM wallet was really the executor's. Guardrails and commit-reveal didn't actually restrict trading. | The contract builds the exact EIP-1559 transaction itself (its own RLP encoder) and calls `v1.signer-prod.testnet` **as the vault**. Recipient is locked to the vault's address, the only allowed targets are WETH, USDC (approve) and the Uniswap router, gas is capped, each leg can be signed once per decision, and the decision must be revealed and pass the guardrails first. The executor rebuilds the tx and refuses to broadcast unless the hash matches. | Funds moved to the vault-derived address `0x63fb…9b6f`. Live vault-signed [approve](https://sepolia.etherscan.io/tx/0xc6c453862f0bae4bec004ffa32f77f63efc13fdfb5694107c88356c509a47e3b) and [swap](https://sepolia.etherscan.io/tx/0xe6558a05490cec51c76f63ece0b2c81d934b499b5347b7e8047266eae5a4bba7), both sent from the vault's address. |
| **C3** Critical | `claim_recovered` paid early claimants against a smaller `total_claims`, so payouts could exceed what was recovered, with the excess coming from remaining holders. | Repatriated NEAR is distributed through a **per-unit accumulator**, credited as `floor(units × Δacc)`. Flooring the *difference* means no account is ever credited more than its exact share. The shareholders' part flows back into the pool. | Regression test `audit_c3_…`. Fuzz invariant **S2 (solvency against the NEAR the contract actually holds)** and **S3 (paid ≤ repatriated)**. |
| **H1** High | The seed-capital mint let the manager (or the executor's claim) mint shares for off-chain value and then withdraw on-chain NEAR. | Seed mint **removed**. The first external value creates units owned by the existing shareholders, and no shares are minted. Withdrawals only pay pool slices (see C1). | `audit_c1_settlement_bounds` asserts that `total_shares` is unchanged. |
| **H2** High | Storage griefing: free account creation, an unbounded NAV log, and uncapped strings could exhaust the contract's balance and trap the last withdrawers. | 0.1 NEAR minimum deposit, NAV log as a 2,000-entry ring buffer, caps on every string argument. | `nav_log_is_a_bounded_ring_buffer` plus the `access_control_and_input_checks` tests. |
| **M1** | Input checks were skippable, and the enclave trusted any price. | `input_json` is mandatory and re-checked in the on-chain callback. The enclave refuses non-finite or non-positive prices (no proof and no settlement). | Contract and strategy tests. |
| **M2** | Selling the strategy NFT didn't move operational control. | Decisions can be requested by the executor or the **NFT holder**. Rotations require the NFT holder. | `fees_respect_hwm_and_go_to_the_nft_holder`. |
| **M3** | Code rotation was instant and silenced the integrity alert. | `propose_strategy` → **48h** → `apply_strategy`, and the alert is only cleared then. Wallet rotation is timelocked the same way. | `strategy_and_wallet_rotations_are_timelocked`. Live: new strategy proposed, effective Sat 14 Nov 17:24 UTC. |
| **M4** | A deposit at NAV 0 handed the money to existing holders. | Deposits are refused when NAV and shares disagree. | `deposit_refused_when_nav_and_shares_disagree`. |
| **M5** | `repair_seed_mint` was reusable. | **Removed.** | — |
| **L1** | Failed transfers vanished from the books. | Every payout goes through `pay()`; failures are credited to `failed_payouts` and can be retried with `retry_payout`. | `failed_transfer_is_credited_back`. |
| **L2** | Fee timing was gameable; the high-water mark wasn't reset when the vault emptied. | Fees only crystallize on a valuation that has stood for 24h, never in emergency. The HWM resets when the last holder exits. | Unit tests and fuzz S7. |

### What the strengthened fuzzer caught in our own fix

Rebuilding the fuzz suite with the reviewer's invariants (solvency against
actual NEAR held, adversarial settlements, repatriation and claims) found
**two real bugs in the new v4 code**, both before deployment:
1. **Accumulator overflow:** a first settlement at value 0 created a single unit, and the next repatriation overflowed, which would have made returning cross-chain funds impossible. Fixed: units are created only alongside real value, with a fixed base of 1e24.
2. **One-yocto over-credit:** taking the difference of two floored totals could round *up*. Fixed by flooring the difference itself.

The suite runs 300 random scenarios in CI. We ran 600 locally at the end; all invariants hold.

## Residual risks (stated, not hidden)

- **Valuation is still relayed by a semi-trusted executor.** It is computed in the enclave from published, attested inputs and is fraud-provable (re-read Sepolia at `evm_block`). It can no longer extract NEAR, but within the ±10%/h bound it can skew **deposit pricing**. This is removed entirely by the on-chain attested path (`request_decision`: only DCAP-verified OutLayer workers can resolve it), which is built but waiting on OutLayer's testnet worker.
- **Front-running window:** the decision is revealed on NEAR seconds before the swap lands on Sepolia. The 2% `amountOutMinimum` bounds the loss; on mainnet, submit through a private mempool.
- **Cross-chain funds depend on NEAR's MPC network** to ever move. Exit guarantees cover the NEAR side fully and make the cross-chain side an enforceable pro-rata claim. They can't remove the MPC dependency, which is inherent to Chain Signatures.
- **The contract is upgradeable by its full-access key** (we used that to ship these fixes on testnet). For mainnet: delete the key, or put upgrades behind the same 48h timelock.
- **Sepolia pools are mispriced** (~10–14× real ETH), so testnet trades produce paper PnL a real pool wouldn't. The 5% pool-vs-oracle guard refuses such trades unless explicitly overridden.

## Review 2

A second independent review of v4 is in progress; its findings and our
responses will be recorded here.
