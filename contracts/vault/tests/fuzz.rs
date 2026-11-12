//! Property tests for the money math (v4), at real yoctoNEAR scale.
//!
//! Random sequences of deposits, withdrawals, ADVERSARIAL settlements (any
//! value the bounds allow), repatriations, external claims, fee
//! crystallization and emergency exits. After every step:
//!
//!  S1  sum of all accounts' shares == total_shares
//!  S2  SOLVENCY: NEAR the contract actually holds (all inflows minus all
//!      outflows) >= everything it owes (pool + every unit holder's claimable
//!      repatriated NEAR). This is the invariant the v3 bug violated.
//!  S3  external payouts never exceed what was repatriated
//!  S4  units conserved: vault units + every holder's units == units total
//!  S5  a withdrawal pays exactly the pro-rata slice of the pool, whatever
//!      the settled valuation says
//!  S6  a deposit never mints shares worth more than it paid
//!  S7  no fee is minted without a new high, and none while value is cross-chain
//!  S8  a deposit only ever succeeds while the vault is fully in NEAR

use near_sdk::json_types::U128;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, NearToken};
use proptest::prelude::*;
use vault_contract::{
    mul_div, Guardrails, Vault, DEFAULT_LIVENESS_MS, MIN_DEPOSIT, MIN_SETTLE_INTERVAL_MS,
};

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const SCALE: u128 = 1_000_000_000_000;

fn acct(i: usize) -> AccountId {
    if i == 4 { "owner.testnet".parse().unwrap() } else { format!("user{i}.testnet").parse().unwrap() }
}
fn executor() -> AccountId { "executor.testnet".parse().unwrap() }

fn ctx(pred: &AccountId, deposit: u128, ts_ms: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id("vault.testnet".parse().unwrap())
        .predecessor_account_id(pred.clone())
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .block_timestamp(ts_ms * 1_000_000)
        .account_balance(NearToken::from_yoctonear(u128::MAX / 4))
        .build());
}

fn new_vault() -> Vault {
    ctx(&acct(4), 0, 0);
    near_sdk::mock::with_mocked_blockchain(|b| { let _ = b.take_storage(); });
    ctx(&acct(4), 0, 0);
    Vault::new(
        acct(4), executor(), "ab".repeat(32), "https://x/y.wasm".into(),
        Guardrails { max_leverage_bps: 30_000, max_drawdown_bps: 2_000, max_position_bps: 5_000, allowed_pairs: vec!["ETH/USDC".into()] },
        "alpha-vault-v1".into(), "0x18f92b8ee2dc4bc3bbbdb4138e92f184bc405b7f".into(),
    )
}

#[derive(Debug, Clone)]
enum Op {
    Deposit { who: usize, milli: u64 },
    Withdraw { who: usize, pct: u8 },
    /// settle at `bps` of the allowed band: -10000 = max down, +10000 = max up
    Settle { band: i32 },
    Repatriate { milli: u64 },
    Claim { who: usize },
    Crystallize,
    Emergency,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0usize..5, 100u64..2_000_000).prop_map(|(who, milli)| Op::Deposit { who, milli }),
        3 => (0usize..5, 1u8..=100).prop_map(|(who, pct)| Op::Withdraw { who, pct }),
        3 => (-10_000i32..=10_000).prop_map(|band| Op::Settle { band }),
        1 => (1u64..500_000).prop_map(|milli| Op::Repatriate { milli }),
        2 => (0usize..5).prop_map(|who| Op::Claim { who }),
        1 => Just(Op::Crystallize),
        1 => Just(Op::Emergency),
    ]
}

fn nps(v: &Vault) -> u128 {
    if v.total_shares == 0 { SCALE } else { mul_div(v.nav(), SCALE, v.total_shares) }
}

fn owed_claims(v: &Vault) -> u128 {
    (0..5).map(|i| v.get_external_position(acct(i))["claimable_yocto"].as_str().unwrap().parse::<u128>().unwrap()).sum()
}
fn holder_units(v: &Vault) -> u128 {
    (0..5).map(|i| v.get_external_position(acct(i))["units"].as_str().unwrap().parse::<u128>().unwrap()).sum()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(300),
        max_shrink_iters: 2000, .. ProptestConfig::default()
    })]

    #[test]
    fn v4_money_invariants(ops in prop::collection::vec(op(), 1..45)) {
        let mut v = new_vault();
        let mut t: u64 = 1;
        let mut held: i128 = 0; // NEAR actually in the contract (model)

        for o in ops {
            t += 1_000;
            match o {
                Op::Deposit { who, milli } => {
                    if v.emergency { continue; }
                    let amt = (milli as u128 * NEAR / 1000).max(MIN_DEPOSIT);
                    let a = acct(who);
                    ctx(&a, amt, t);
                    let ext_before = v.ext_value;
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.deposit().0));
                    let Ok(minted) = r else { continue };
                    prop_assert_eq!(ext_before, 0, "S8: deposit priced off a cross-chain valuation");
                    // S6
                    prop_assert!(mul_div(v.nav(), minted, v.total_shares) <= amt);
                    held += amt as i128;
                }
                Op::Withdraw { who, pct } => {
                    let a = acct(who);
                    let burn = v.get_shares(a.clone()).0 * pct as u128 / 100;
                    if burn == 0 { continue; }
                    ctx(&a, 0, t);
                    // fees may crystallize first (diluting): compute fair slice after
                    if !v.emergency { v.crystallize_fees(); }
                    let fair = mul_div(v.pool, burn, v.total_shares);
                    let p0 = v.pool;
                    let _ = v.withdraw(U128(burn));
                    let paid = if v.total_shares == 0 { fair } else { p0 - v.pool };
                    prop_assert_eq!(paid, fair, "S5: withdrawal must be the exact pool slice");
                    held -= paid as i128;
                }
                Op::Settle { band } => {
                    if v.emergency || v.total_shares == 0 { continue; }
                    t += MIN_SETTLE_INTERVAL_MS + 1;
                    let nav = v.nav();
                    let step = nav / 10;
                    let holders_now = v.holders_ext_value() as i128;
                    let target_holders = (holders_now + step as i128 * band as i128 / 10_000).max(0) as u128;
                    // convert holders' target back to whole-wallet value
                    let new_ext = if v.ext_units_total == 0 { target_holders }
                        else { mul_div(target_holders, v.ext_units_total, v.vault_ext_units.max(1)) };
                    ctx(&executor(), NEAR / 10, t);
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.settle_pnl(U128(new_ext), 1, "fuzz".into())));
                }
                Op::Repatriate { milli } => {
                    if v.ext_units_total == 0 { continue; }
                    let amt = milli as u128 * NEAR / 1000;
                    ctx(&acct(4), amt, t);
                    v.repatriate();
                    held += amt as i128;
                }
                Op::Claim { who } => {
                    let a = acct(who);
                    ctx(&a, 0, t);
                    let before = v.ext_paid_total;
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { let _ = v.claim_external(); })).is_ok() {
                        held -= (v.ext_paid_total - before) as i128;
                    }
                }
                Op::Crystallize => {
                    let m0 = v.get_shares(acct(4)).0;
                    let above = nps(&v) > v.hwm_nav_per_share;
                    let cross_chain = v.ext_value > 0;
                    ctx(&acct(4), 0, t);
                    v.crystallize_fees();
                    if !above || cross_chain { prop_assert_eq!(v.get_shares(acct(4)).0, m0, "S7"); }
                }
                Op::Emergency => {
                    if v.emergency { continue; }
                    t += DEFAULT_LIVENESS_MS + 1;
                    ctx(&acct(0), 0, t);
                    v.trigger_emergency();
                }
            }
            // S1
            let sum: u128 = (0..5).map(|i| v.get_shares(acct(i)).0).sum();
            prop_assert_eq!(sum, v.total_shares, "S1 share ledger");
            // S2 solvency
            let owed = v.pool + owed_claims(&v);
            prop_assert!(held >= owed as i128, "S2 insolvent: holds {} owes {}", held, owed);
            // S3
            prop_assert!(v.ext_paid_total <= v.repatriated_total, "S3 paid more than repatriated");
            // S4
            prop_assert_eq!(v.vault_ext_units + holder_units(&v), v.ext_units_total, "S4 units");
        }
    }
}
