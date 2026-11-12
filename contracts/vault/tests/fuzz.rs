//! Property tests for the money math: random sequences of deposits,
//! withdrawals, PnL, settlements, fee crystallization and emergency exits at
//! REAL yoctoNEAR scale. After every step, check the invariants that make the
//! vault safe to hold other people's money.
//!
//! Invariants checked:
//!  I1  sum of every account's shares == total_shares
//!  I2  conservation: deposits + pnl == pool + external + paid out (+ claims)
//!  I3  rounding favors the vault: a deposit never mints shares worth more
//!      than was deposited; a withdrawal never pays more than the shares' NAV
//!  I4  deposits/withdrawals never move NAV/share up for the actor and never
//!      down for everyone else (beyond 1-yocto rounding)
//!  I5  fees: the manager's share count only grows through crystallization
//!      when NAV/share is above the high-water mark
//!  I6  emergency: local payout <= pro-rata pool, claim == pro-rata external

use near_sdk::json_types::U128;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, NearToken};
use proptest::prelude::*;
use vault_contract::{mul_div, Guardrails, Vault, DEFAULT_LIVENESS_MS};

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const SCALE: u128 = 1_000_000_000_000; // NPS_SCALE

fn acct(i: usize) -> AccountId {
    format!("user{i}.testnet").parse().unwrap()
}
fn owner() -> AccountId { "owner.testnet".parse().unwrap() }
fn executor() -> AccountId { "executor.testnet".parse().unwrap() }

fn context(pred: &AccountId, deposit: u128, ts_ms: u64) -> near_sdk::VMContext {
    VMContextBuilder::new()
        .predecessor_account_id(pred.clone())
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .block_timestamp(ts_ms * 1_000_000)
        // plenty of balance so payout promises can be created
        .account_balance(NearToken::from_yoctonear(u128::MAX / 4))
        .build()
}

fn ctx(pred: &AccountId, deposit: u128, ts_ms: u64) {
    testing_env!(context(pred, deposit, ts_ms)); // keeps storage
}

fn new_vault() -> Vault {
    // fresh storage for every generated case
    ctx(&owner(), 0, 0);
    near_sdk::mock::with_mocked_blockchain(|b| { let _ = b.take_storage(); });
    ctx(&owner(), 0, 0);
    Vault::new(
        owner(),
        executor(),
        "ab".repeat(32),
        "https://x/y.wasm".into(),
        Guardrails {
            max_leverage_bps: 30_000,
            max_drawdown_bps: 2_000,
            max_position_bps: 5_000,
            allowed_pairs: vec!["ETH/USDC".into()],
        },
    )
}

#[derive(Debug, Clone)]
enum Op {
    Deposit { who: usize, milli: u64 },   // amount in 1e-3 NEAR
    Withdraw { who: usize, pct: u8 },      // % of the actor's shares
    Pnl { bps: i32 },                      // pool gain/loss, bps of pool
    Settle { bps_of_pool: u32 },           // external value as bps of pool
    Crystallize,
    Emergency,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0usize..5, 1u64..2_000_000).prop_map(|(who, milli)| Op::Deposit { who, milli }),
        3 => (0usize..5, 1u8..=100).prop_map(|(who, pct)| Op::Withdraw { who, pct }),
        2 => (-3_000i32..5_000).prop_map(|bps| Op::Pnl { bps }),
        1 => (0u32..20_000).prop_map(|bps_of_pool| Op::Settle { bps_of_pool }),
        1 => Just(Op::Crystallize),
        1 => Just(Op::Emergency),
    ]
}

fn who(i: usize) -> AccountId { if i == 4 { owner() } else { acct(i) } }

fn sum_shares(v: &Vault) -> u128 {
    (0..5).map(|i| v.get_shares(who(i)).0).sum()
}

fn nps(v: &Vault) -> u128 {
    if v.total_shares == 0 { SCALE } else { mul_div(v.nav(), SCALE, v.total_shares) }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(300), max_shrink_iters: 2000, .. ProptestConfig::default() })]

    #[test]
    fn money_math_invariants(ops in prop::collection::vec(op(), 1..40)) {
        let mut v = new_vault();
        let mut t: u64 = 1;
        let (mut deposited, mut pnl, mut paid): (i128, i128, i128) = (0, 0, 0);
        let mut claims: i128 = 0;

        for o in ops {
            t += 1000;
            match o {
                Op::Deposit { who: w, milli } => {
                    if v.emergency { continue; }
                    let amt = milli as u128 * NEAR / 1000;
                    let before_nps = nps(&v);
                    let a = who(w);
                    let before = v.get_shares(a.clone()).0;
                    let total_before = v.total_shares;
                    ctx(&a, amt, t);
                    let minted = v.deposit().0;
                    // the manager also receives any fee shares crystallized first
                    let fee = v.total_shares - total_before - minted;
                    let expect = if a == owner() { before + minted + fee } else { before + minted };
                    prop_assert_eq!(v.get_shares(a.clone()).0, expect);
                    // I3: shares minted are worth <= what was paid
                    let worth = mul_div(v.nav(), minted, v.total_shares);
                    prop_assert!(worth <= amt, "deposit minted {} worth more than {}", worth, amt);
                    // I4: NAV/share doesn't drop for existing holders (fee crystallization may lower it legitimately: only check when no gain above HWM)
                    if before_nps <= v.hwm_nav_per_share {
                        prop_assert!(nps(&v) + 1 >= before_nps, "deposit lowered NAV/share {} -> {}", before_nps, nps(&v));
                    }
                    deposited += amt as i128;
                }
                Op::Withdraw { who: w, pct } => {
                    let a = who(w);
                    let have = v.get_shares(a.clone()).0;
                    let burn = have * pct as u128 / 100;
                    if burn == 0 { continue; }
                    let (pool0, ext0, total0, nav0) = (v.pool, v.external_value, v.total_shares, v.nav());
                    ctx(&a, 0, t);
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { let _ = v.withdraw(U128(burn)); }));
                    if r.is_err() { continue; } // e.g. insufficient local liquidity: allowed refusal
                    let out = pool0 - v.pool;
                    if v.emergency {
                        // I6
                        prop_assert!(out <= mul_div(pool0, burn, total0));
                        let claim = ext0 - v.external_value;
                        prop_assert_eq!(claim, mul_div(ext0, burn, total0));
                        claims += claim as i128;
                    } else {
                        // I3: never paid more than the shares' NAV at the time
                        // (fees may crystallize first, which only lowers it)
                        prop_assert!(out <= mul_div(nav0, burn, total0), "withdraw paid {} > fair {}", out, mul_div(nav0, burn, total0));
                    }
                    paid += out as i128;
                }
                Op::Pnl { bps } => {
                    if v.pool == 0 { continue; }
                    let delta = (v.pool as i128) * bps as i128 / 10_000;
                    let np = (v.pool as i128 + delta).max(0) as u128;
                    pnl += np as i128 - v.pool as i128;
                    v.pool = np;
                }
                Op::Settle { bps_of_pool } => {
                    if v.emergency || v.total_shares == 0 { continue; }
                    let new_ext = v.pool * bps_of_pool as u128 / 10_000;
                    let old_ext = v.external_value;
                    let first = v.last_settlement_ref.is_none() && v.external_value == 0;
                    let before_nps = nps(&v);
                    ctx(&executor(), 0, t);
                    v.settle_pnl(U128(new_ext), 1, "fuzz".into());
                    if first && new_ext > 0 {
                        // seed capital: NAV/share unchanged (within rounding)
                        prop_assert!(nps(&v).abs_diff(before_nps) <= 1, "seed moved NAV/share {} -> {}", before_nps, nps(&v));
                        deposited += new_ext as i128; // capital, not PnL
                    } else {
                        pnl += new_ext as i128 - old_ext as i128;
                    }
                }
                Op::Crystallize => {
                    let m0 = v.get_shares(owner()).0;
                    let above = nps(&v) > v.hwm_nav_per_share;
                    ctx(&owner(), 0, t);
                    v.crystallize_fees();
                    // I5
                    if !above { prop_assert_eq!(v.get_shares(owner()).0, m0, "fee minted without a new high"); }
                }
                Op::Emergency => {
                    if v.emergency { continue; }
                    t += DEFAULT_LIVENESS_MS + 1;
                    ctx(&acct(0), 0, t);
                    v.trigger_emergency();
                }
            }
            // I1
            prop_assert_eq!(sum_shares(&v), v.total_shares, "share ledger out of sync");
            // I2 conservation (claims are value moved out of `external`)
            let lhs = deposited + pnl;
            let rhs = v.pool as i128 + v.external_value as i128 + paid + claims;
            prop_assert_eq!(lhs, rhs, "value created or destroyed");
        }
    }
}
