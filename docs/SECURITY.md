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
| **M3** | Code rotation was instant and silenced the integrity alert. | `propose_strategy` → **48h** → `apply_strategy`, and the alert is only cleared then. *(v5: wallet rotation removed; proposals bound to the proposer.)* | `strategy_rotation_is_timelocked_and_bound_to_the_proposer`. Live: strategy v2 proposed, effective Sat 14 Nov 17:51 UTC. |
| **M4** | A deposit at NAV 0 handed the money to existing holders. | Deposits are refused when NAV and shares disagree. | `deposit_refused_when_nav_and_shares_disagree`. |
| **M5** | `repair_seed_mint` was reusable. | **Removed.** | — |
| **L1** | Failed transfers vanished from the books. | Every payout goes through `pay()`; failures are credited to `failed_payouts` and can be retried with `retry_payout`. | `failed_transfer_is_credited_back`. |
| **L2** | Fee timing was gameable; the high-water mark wasn't reset when the vault emptied. | Fees only crystallize on a valuation that has stood for 24h, never in emergency. The HWM resets when the last holder exits. *(v5: fees only on realized gains; see Review 2.)* | Unit tests and fuzz S7. |

### What the strengthened fuzzer caught in our own fix

Rebuilding the fuzz suite with the reviewer's invariants (solvency against
actual NEAR held, adversarial settlements, repatriation and claims) found
**two real bugs in the new v4 code**, both before deployment:
1. **Accumulator overflow:** a first settlement at value 0 created a single unit, and the next repatriation overflowed, which would have made returning cross-chain funds impossible. Fixed: units are created only alongside real value, with a fixed base of 1e24.
2. **One-yocto over-credit:** taking the difference of two floored totals could round *up*. Fixed by flooring the difference itself.

The suite runs 300 random scenarios in CI. We ran 600 locally at the end; all invariants hold.

## Review 2 (12 Nov): independent review of v4

A second reviewer attacked v4 and asked a sharper question: in the mode that
actually runs today, what can the relayer still do? Its answer was more than
our v4 write-up admitted. Two claims above were wrong and are corrected here:

- *"`request_decision` removes valuation trust."* Only if it is the **only**
  path. In v4 the executor could still call `settle_pnl` directly. And even on
  the on-chain path, the market inputs (prices, balances) come from whoever
  requests the run. The contract checks the vault-state part of the input,
  not the prices.
- *"2% slippage is enforced on-chain."* It wasn't. In v4 the executor passed
  `amount_out_min` and `amount_in` to `sign_evm_tx`. The contract only checked
  that the floor was non-zero.

| # | Finding (v4) | v5 response |
|---|---|---|
| **K1** Critical | The vault account's full-access key is the owner/executor key. Whoever holds it can redeploy the contract and bypass every rule. | Code changes now have a public path: `propose_code(sha256)` → **48h** → `apply_code` (anyone can submit the code; it must hash to the proposal; deploys and calls `migrate`). **The full-access key is deleted after the final deploy (Sat 14 Nov)**, which leaves `apply_code` as the only way to upgrade. Until then this is the top residual risk, stated in the README. |
| **X1–X4** High | In relayed mode the contract can't verify that a valuation came from the enclave. The reported value fed deposit pricing (a lowballed NAV sells cheap shares to a colluder), fee crystallization, and the chart. The ±10%/h bound slows this down but doesn't stop it. | The valuation no longer touches money: **deposits only open while `ext_value == 0`** (fully in NEAR, priced by NEAR the contract holds) and **fees only crystallize while `ext_value == 0`** (realized gains only). Withdrawals never read it (v4). What remains is a chart a relayer could misdraw, and the EVM block it names lets anyone check. **Attested mode** (`enable_attested`, immediate; `propose_relayed` → 48h to go back) blocks `commit_decision`, `settle_pnl` and executor proofs. Only `on_decision` output from OutLayer's `request_execution` can then commit, reveal, settle or anchor a proof hash. |
| **X5** High | The relayer chose the trade size and the slippage floor of every vault-signed swap. | **The enclave decides them** (strategy v2). `amount_in`, `min_out` (oracle −2%) and `fee_tier` are fields of the committed decision. `sign_evm_tx` takes only nonce, gas price and gas limit (capped per leg: wrap 200k, approve 120k, swap 400k, 100 gwei) plus a deadline within the next hour. The swap is a `multicall(deadline, …)`. Decisions from pre-v2 strategies carry no size and can't be signed at all. Rust and viem calldata are checked byte-for-byte in a unit test. |
| **W1** High | `propose_wallet` could rotate the vault's EVM wallet (timelocked, but it could redirect every future swap). | **Removed.** `evm_address` is fixed for the life of the vault. |
| **P1** Medium | Proposals outlived a sale of the strategy NFT: the old manager's proposal could still be applied. | Proposals record their proposer; `apply_*` requires that the proposer still holds the NFT. `cancel_proposals` for the current manager. |
| **S1** Medium | The relayer's writes (inputs up to 6 KB, bundles up to 8 KB) were paid from the contract's balance, which is shareholders' NEAR. | Every relayed write is `#[payable]` and charges the exact `storage_usage` delta (excess refunded). `request_decision` keeps 0.1 NEAR back for its callback's storage. |
| **S2** Medium | (Found by us while deploying v5.) The v4 code was 853 KB unstripped, which locked ~8.5 NEAR of storage stake and left the vault with **less free balance than its pool owed**. | Built with `-C link-arg=-s` plus `wasm-opt -Oz`: **393 KB**, freeing ~4.6 NEAR. After the migration the free balance covers the pool. |
| **E1** Medium | Cross-chain value has no on-chain redemption leg (no bridge). | Unchanged: stated as a residual. Units are an enforceable pro-rata claim on whatever is repatriated, and `repatriate` is permissionless. |
| **M** Low | Migration nits: v3→v4 reset `last_settlement_ms`, and pending proposals didn't carry over. | v4→v5 preserves both. A pending wallet rotation blocks the migration. |

Tests: 22 unit tests (new: attested delivery commits, reveals, settles and
gates proofs; attested mode locks out the relayer; timelocked mode reversal;
hash-bound timelocked code upgrade; proposer-bound rotation; deposits only
while fully in NEAR; fees only on realized gains; enclave-sized multicall
swap). Fuzz invariants **S8** (a deposit never succeeds while value is
cross-chain) and a tightened **S7** (no fee while value is cross-chain) were
added; 400 random scenarios pass. Deployed and migrated on testnet (tx
`EGUedioDCU5v8GJptkCN5HaKZFeCRG6DmiNkvGndFTWJ`). The first live relayed cycle
on v5 committed, revealed and anchored a proof with the relayer paying its own
storage.

## Residual risks (stated, not hidden)

- **Until Saturday the vault account still has a full-access key.** Whoever holds it can replace the code. We delete it after the final deploy. After that, code changes take 48h and are public (`propose_code`).
- **Relayed mode trusts the relayer for liveness and for the chart.** It can withhold runs (the 24h liveness rule then lets anyone trigger emergency exit) or misreport the cross-chain valuation shown on the NAV chart. It can't move NEAR, price deposits, cash fees, size trades or pick a trade the enclave didn't commit.
- **Attested mode works, but isn't switched on until strategy v2 applies (Sat 14 Nov).** We had blamed OutLayer's testnet worker for never answering on-chain calls. The real cause was ours: `request_decision` passed empty `resource_limits`, and OutLayer's default budget (1e9 instructions) aborts the strategy mid-Bulletproof (it needs ~2.3e9). With an explicit 5e9 budget the vault's own on-chain path works (commitment #18, ~20s round trip).
- **Market inputs are supplied by the requester, even in attested mode.** The contract checks the vault-state part of the input but not prices or balances. The browser's price check compares each published input against public market history. The next step is an enclave that fetches its own prices (wasip2 HTTP).
- **Front-running window:** the decision becomes public when it is revealed (or delivered, in attested mode), shortly before the swap lands. The enclave's 2% floor and the 1h deadline bound the loss. On mainnet, submit through a private mempool.
- **Cross-chain funds depend on NEAR's MPC network** to ever move, and have no on-chain bridge back (repatriation is a manual sale and bridge followed by `repatriate`).
- **Sepolia pools are mispriced** (~10–14× real ETH). The enclave's oracle-based floor makes swaps into a bad pool unfillable; the executor checks the quote and refuses before spending gas.
- **No rate limit** on relayed calls beyond the 1h settlement spacing. Each write is self-funded (storage), so spam costs the spammer.
