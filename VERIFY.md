# Check every claim yourself

Verifiable Alpha Vaults says one thing: **you don't have to trust the AI fund
manager.** So every claim below comes with three ways to check it, and none of
them needs our help:

- **Click.** Open the live vault page and press **Run all checks**. Your browser does the cryptography itself.
- **Run.** `cd executor && npm ci && node verify.mjs`. The same checks plus two more, from your terminal. It signs nothing, spends nothing and needs no keys. Exit code 0 means everything passed.
- **Look.** Every row links to the code that enforces it, the test that attacks it, and a live transaction where it happened.

Live vaults on NEAR testnet:

- **`vault.alpha-vaults-23.testnet`**: the main vault, with 19 decisions since 1 Nov.
- **`vault2.alpha-vaults-23.testnet`**: the rehearsal vault, which ran the fully attested cycle with a real swap.

To check either one: `node verify.mjs --vault <id>`.

Most recent output (13 Nov), main vault:

```
✓ 1. Which code ran (Intel TDX attestations)
  6 TDX attestations fetched: executed code = recorded code on all (2 logged strategy rotation); 1 delivered on-chain by OutLayer's TDX workers straight to the vault
✓ 2. Every decision committed before it executed
  16/16 commitments recomputed here
✓ 3. Risk limits held (Bulletproofs, verified here)
  13 Bulletproof bundles verified here (leverage ≤ 3x, drawdown ≤ 20%); latest 8 computed from actual holdings, inputs checked by the contract
✓ 4. Real market prices went in
  4 enclave inputs match their attested input_hash
✓ 5. Swaps on Sepolia match their commitments
  4 recorded swaps all carry their decision's commitment in calldata: 1 signed by the vault's own wallet 0x63fbAd3b…, 2 from before v4 signed by the executor's wallet (the design review 1 replaced), 1 recorded honestly as reverted
✓ 6. The vault can pay every holder
  free balance 4.4768 NEAR ≥ pool 1.0487 NEAR (code + state stake 4.6622 NEAR); mode Relayed, deposits closed while value is cross-chain
6/6 passed
```

## The four seals

### 1. The approved code ran, and only it

- **Claim:** every decision came from the strategy build the vault approved, running inside Intel TDX. A strategy change is public for 48h before it applies.
- **Enforced by:** `commit_decision` checks the hash against the approved build and raises an integrity alert on a mismatch. `request_decision` pins OutLayer to the approved build, and only DCAP-verified TDX workers can answer it. `propose_strategy` / `apply_strategy` apply the 48h timelock, bound to whoever proposed it. All in `contracts/vault/src/lib.rs`.
- **Attacked by:** `swapped_code_is_recorded_and_pauses_deposits`, `strategy_rotation_is_timelocked_and_bound_to_the_proposer`, `attested_delivery_commits_reveals_settles_and_gates_proofs`.
- **Happened:**
  - A TDX attestation reports `executed_wasm_sha256` for each relayed decision; `verify.mjs` fetches them.
  - On-chain delivery to the vault: tx `CpUKMrgDPGwCWXEmmQFhyfv4sLb22c6WY6bttZDm5527` (commitment #18).
  - Strategy v2 proposal, public since 12 Nov: tx `HE8s5eQL…`.

### 2. Every decision was committed before it executed

- **Claim:** nobody, including us, can backfill a winning trade or swap a decision after the fact.
- **Enforced by:** `commit_decision` / `on_decision` store `sha256(decision ‖ salt)` first. `reveal` only accepts a preimage that hashes to it. `sign_evm_tx` only signs revealed, guardrail-passed decisions, at most once per leg. The swap's calldata ends in the commitment.
- **Attacked by:** `happy_path_and_backfill_and_breaches`, `attested_full_output_anchors_proofs_and_relayer_can_only_reveal_what_was_committed`, `sign_evm_tx_signs_only_the_committed_trade`. The **Counterfeit Bureau** on the site lets you try it yourself.
- **Happened:** vault2 commitment #2 (`55b77309…`) was revealed, then sold 0.01308 ETH in Sepolia tx `0xa7ca6d0ebe7144bb829194832ea047c9c7a6594dbada642bbbd5a97fc19f13a1`. The calldata ends in `55b77309…`.

### 3. Risk limits held, without revealing the strategy

- **Claim:**
  - Leverage ≤ 3x and drawdown ≤ 20% held on every run. Pedersen commitments and aggregated Bulletproofs prove it without revealing the values.
  - The contract refuses any decision outside its guardrails before a signature exists.
  - The size of every trade comes from the enclave, never from the relayer.
- **Enforced by:**
  - `crates/proofs`: the range proofs, using the complement trick to prove a value is at most the limit.
  - `check_guardrails` and `check_input_state` in the contract: the latter means the enclave's view of the vault can't be doctored.
  - `sign_evm_tx`: amount, 2% floor and fee tier are read from the committed decision, with per-leg gas caps and a deadline.
- **Attacked by:** `ten_x_leverage_cannot_be_proven`, `breaching_drawdown_cannot_be_proven`, `proof_does_not_verify_against_a_looser_or_tighter_limit`, `tampered_commitment_fails`, `proof_is_bound_to_label_and_epoch`, `commitment_hides_the_value`, plus the swap calldata test (byte-identical in Rust and viem).
- **Happened:** 13 bundles on the main vault and 3 on vault2, all verified by `verify.mjs` and by the vault page with the same WASM verifier.

### 4. Real market prices went in

- **Claim:** the enclave saw real prices.
- **Enforced by:**
  - Every relayed decision publishes its exact enclave input, and its sha256 must equal the attestation's `input_hash`.
  - The oracle price inside is checked against public CoinGecko history (±2%).
  - The strategy refuses bad prices (`prices_ok`): no proof and no settlement.
  - Demo-series prices are labelled as such, never passed off as real.
- **Happened:** 4 inputs on the main vault match their attested hashes.

## The money

| Claim | Enforced by | Attacked by |
|---|---|---|
| A withdrawal pays an exact pro-rata slice of the NEAR pool, and never reads a valuation | `withdraw` | fuzz **S5**, `deposit_withdraw_roundtrip_and_min_deposit` |
| The contract can always pay what it owes | `free balance ≥ pool` (check 6) | fuzz **S2**, solvency against a model of the NEAR it holds |
| A valuation can never price a deposit | `deposit` only while `ext_value == 0` | fuzz **S8**, `deposits_open_only_while_fully_in_near` |
| Fees only come from realized gains above the high-water mark, and go to the strategy NFT holder | `crystallize_fees` | fuzz **S7**, `fees_only_on_realized_gains`, `fees_respect_hwm_and_go_to_the_nft_holder` |
| If the vault goes silent for 24h, anyone can trigger a one-way emergency exit | `trigger_emergency` | `emergency_is_permissionless_one_way_and_withdrawals_still_work` |
| The vault's EVM wallet is fixed; swaps pay only the vault | no rotation exists; recipient hard-wired in `swap_calldata` | `swap_calldata_is_a_deadlined_multicall_paying_the_vault` |
| Cross-chain value comes back as NEAR (NEAR Intents) | `propose_exit` (public 24h, proposer-bound) → `sign_exit_tx` (one transfer) → `unwrap_and_repatriate` | `exit_via_near_intents_is_public_for_24h_signed_once_and_bound_to_the_proposer`, `unwrapped_near_from_intents_is_credited_like_repatriation` |
| The storage stake only comes back when the vault is empty | `propose_close` (public 48h, deposits stop) → `close_vault` only when no shares remain and nothing is reported cross-chain; the contract deletes its own account | `close_is_public_48h_stops_deposits_and_needs_an_empty_vault` |
| Nobody can point a vault at another chain | the network is chosen from the vault's own account id (`.testnet` → Sepolia, otherwise Base); there is no setter | `network_is_chosen_by_the_vault_account_not_by_anyone` |
| The relayer pays for every byte it writes | `charge_storage` on every relayed write | live: every relayed call attaches a deposit; the unused part is refunded |

The NEAR Intents arrival half ran live: tx `AFfuBsgvysevy8JVU5YRRB73ie5y14G5PubfqZ99kZUK` unwrapped 0.04875 NEAR and credited it to holders. Live 1Click quote: `node executor/intents.mjs quote`.

## What we don't claim

Read these before you trust anything above. Each is explained in [`docs/SECURITY.md`](docs/SECURITY.md):

- **Testnet only.** Sepolia's pools are mispriced about 10×, so only sells fill.
- **Relayed mode is live; attested mode is switched on 14 Nov.** In relayed mode the relayer is trusted to run on time and to report the valuation on the NAV chart. It can't move NEAR, price a deposit, cash a fee, size a trade, or pick a decision the enclave didn't commit.
- **The requester supplies the market prices.** This is true in both modes. Check 4 compares them with public history.
- **The NEAR Intents send half is mainnet-only.** NEAR Intents has no testnet.
- **No professional audit yet.** Two independent AI-assisted reviews; every finding, fix and remaining risk is in `docs/SECURITY.md`.

## Re-run everything

```bash
cargo test --workspace                 # 56 Rust tests incl. a property-based fuzz of the money invariants
(cd executor && npm ci && node test.mjs)  # executor tests incl. a full local-enclave cycle
node executor/verify.mjs               # the six live checks, read-only
```
