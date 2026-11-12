use super::*;
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::testing_env;

const NEAR: u128 = ONE_NEAR;
const WASM_HASH: &str = "c3b8b92a101e575d1c0e2e502c1fc21443fa0e08e864e84fe467d5840bda9f77";
const EVM: &str = "0x18f92b8ee2dc4bc3bbbdb4138e92f184bc405b7f";

fn owner() -> AccountId { "amudhan.testnet".parse().unwrap() }
fn executor() -> AccountId { "executor.testnet".parse().unwrap() }
fn alice() -> AccountId { "alice.testnet".parse().unwrap() }
fn mallory() -> AccountId { "mallory.testnet".parse().unwrap() }
fn vault_id() -> AccountId { "vault.testnet".parse().unwrap() }

fn guardrails() -> Guardrails {
    Guardrails { max_leverage_bps: 30_000, max_drawdown_bps: 2_000, max_position_bps: 5_000, allowed_pairs: vec!["ETH/USDC".into()] }
}

fn at(pred: AccountId, deposit: u128, ts_ms: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(vault_id())
        .predecessor_account_id(pred)
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .account_balance(NearToken::from_yoctonear(u128::MAX / 4))
        .block_timestamp(ts_ms * 1_000_000)
        .build());
}
fn ctx(pred: AccountId, deposit: u128) { at(pred, deposit, 1_000) }

fn new_vault() -> Vault {
    ctx(owner(), 0);
    Vault::new(owner(), executor(), WASM_HASH.into(), "https://x/y.wasm".into(), guardrails(), "alpha-vault-v1".into(), EVM.into())
}

fn decision(action: &str, lev: u64, size: u64, pair: &str) -> String {
    serde_json::json!({"action": action, "pair": pair, "size_bps": size, "leverage_bps": lev, "epoch": 1, "nonce": 7}).to_string()
}
fn commit_of(d: &str, salt: &[u8]) -> String {
    let mut pre = d.as_bytes().to_vec();
    pre.extend_from_slice(salt);
    hex::encode(near_sdk::env::sha256(&pre))
}
fn state_input(v: &Vault) -> String {
    serde_json::json!({"vault_state": {
        "pool_yocto": v.pool.to_string(), "total_shares": v.total_shares.to_string(),
        "hwm_nps": v.hwm_nav_per_share.to_string(), "external_value_yocto": v.holders_ext_value().to_string(),
        "max_leverage_bps": 30_000, "max_drawdown_bps": 2_000 }}).to_string()
}
fn commit(v: &mut Vault, d: &str, salt: &[u8]) -> u64 {
    ctx(executor(), 0);
    let si = state_input(v);
    v.commit_decision(commit_of(d, salt), WASM_HASH.into(), "att".into(), si)
}
fn panics<F: FnOnce()>(f: F) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err()
}

// ---------------------------------------------------------------- shares

#[test]
fn deposit_withdraw_roundtrip_and_min_deposit() {
    let mut v = new_vault();
    ctx(alice(), NEAR / 20);
    assert!(panics(|| { v.deposit(); }), "below 0.1 NEAR is refused");
    ctx(alice(), NEAR);
    assert_eq!(v.deposit().0, NEAR);
    ctx(owner(), 3 * NEAR);
    assert_eq!(v.deposit().0, 3 * NEAR);
    ctx(alice(), 0);
    let _ = v.withdraw(U128(NEAR));
    assert_eq!(v.pool, 3 * NEAR);
    assert_eq!(v.get_shares(alice()).0, 0);
}

#[test]
fn deposit_refused_when_nav_and_shares_disagree() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    v.pool = 0; // total loss
    ctx(mallory(), NEAR);
    assert!(panics(|| { v.deposit(); }), "nav 0 with shares outstanding");
}

#[test]
fn last_holder_out_resets_high_water_mark() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    v.hwm_nav_per_share = 2 * NPS_SCALE;
    ctx(alice(), 0);
    let _ = v.withdraw(U128(NEAR));
    assert_eq!(v.total_shares, 0);
    assert_eq!(v.hwm_nav_per_share, NPS_SCALE);
}

// ------------------------------------------------- commit / reveal / guardrails

#[test]
fn happy_path_and_backfill_and_breaches() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    let d = decision("buy", 25_000, 4_000, "ETH/USDC");
    let id = commit(&mut v, &d, b"s1");
    v.execute_decision(id, d.clone(), hex::encode(b"s1"));
    assert_eq!(v.get_commitment(id).unwrap().status, CommitmentStatus::Executed);

    // back-filled winner against an honest commitment
    let real = decision("buy", 25_000, 4_000, "ETH/USDC");
    let fake = decision("sell", 25_000, 4_000, "ETH/USDC");
    let id2 = commit(&mut v, &real, b"s2");
    assert!(panics(|| v.execute_decision(id2, fake.clone(), hex::encode(b"s2"))));

    // 10x leverage: recorded as Rejected before anything could be signed
    let greedy = decision("buy", 100_000, 4_000, "ETH/USDC");
    let id3 = commit(&mut v, &greedy, b"s3");
    v.execute_decision(id3, greedy, hex::encode(b"s3"));
    let c = v.get_commitment(id3).unwrap();
    assert_eq!(c.status, CommitmentStatus::Rejected);
    assert!(c.reject_reason.unwrap().contains("exceeds max"));
    assert!(get_logs().iter().any(|l| l.contains("guardrail_rejected")));

    // unlisted pair and oversized position
    for bad in [decision("buy", 20_000, 1_000, "DOGE/USDC"), decision("buy", 20_000, 9_000, "ETH/USDC")] {
        let i = commit(&mut v, &bad, b"x");
        v.execute_decision(i, bad, hex::encode(b"x"));
        assert_eq!(v.get_commitment(i).unwrap().status, CommitmentStatus::Rejected);
    }
}

#[test]
fn swapped_code_is_recorded_and_pauses_deposits() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    ctx(executor(), 0);
    let si = state_input(&v);
    let id = v.commit_decision("a".repeat(64), "d".repeat(64), "att".into(), si);
    assert_eq!(v.get_commitment(id).unwrap().status, CommitmentStatus::Rejected);
    assert!(v.deposits_paused && v.integrity_alert.is_some());
    ctx(mallory(), NEAR);
    assert!(panics(|| { v.deposit(); }));
}

#[test]
fn access_control_and_input_checks() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    let si = state_input(&v);
    ctx(mallory(), 0);
    assert!(panics(|| { v.commit_decision("a".repeat(64), WASM_HASH.into(), "x".into(), si.clone()); }));
    ctx(executor(), 0);
    // M1: input is mandatory and must carry vault_state
    assert!(panics(|| { v.commit_decision("a".repeat(64), WASM_HASH.into(), "x".into(), "{}".into()); }));
    // doctored pool
    let mut fake: serde_json::Value = serde_json::from_str(&si).unwrap();
    fake["vault_state"]["pool_yocto"] = serde_json::json!("1");
    assert!(panics(|| { v.commit_decision("a".repeat(64), WASM_HASH.into(), "x".into(), fake.to_string()); }));
    // H2: oversized strings refused
    assert!(panics(|| { v.commit_decision("a".repeat(64), WASM_HASH.into(), "x".repeat(MAX_REF + 1), si.clone()); }));
}

// ------------------------------------------------------------------ fees

#[test]
fn fees_respect_hwm_and_go_to_the_nft_holder() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    v.pool += NEAR / 2; // +50%
    ctx(owner(), 0);
    assert!(v.crystallize_fees().0 > 0);
    let fee_val = mul_div(v.nav(), v.get_shares(owner()).0, v.total_shares);
    assert!(fee_val.abs_diff(NEAR / 10) < NEAR / 10_000, "20% of a 0.5 NEAR gain");
    assert_eq!(v.crystallize_fees().0, 0, "no double charge");
    // NFT sale moves fee rights
    let bob: AccountId = "bob.testnet".parse().unwrap();
    ctx(owner(), 1);
    v.nft_transfer(bob.clone(), STRATEGY_TOKEN_ID.into(), None, None);
    v.pool += NEAR;
    ctx(bob.clone(), 0);
    v.crystallize_fees();
    assert!(v.get_shares(bob.clone()).0 > 0);
    // and rotation rights (M2/M3)
    ctx(owner(), 0);
    assert!(panics(|| v.propose_strategy("ab".repeat(32), "u".into())));
    ctx(bob, 0);
    v.propose_strategy("ab".repeat(32), "u".into());
}

#[test]
fn fees_wait_for_a_settlement_to_stand_24h() {
    let mut v = new_vault();
    at(alice(), 10 * NEAR, 1_000);
    v.deposit();
    at(executor(), 0, 2_000);
    v.settle_pnl(U128(NEAR / 2), 1, "a".into()); // +5% NAV
    at(owner(), 0, 3_000);
    assert_eq!(v.crystallize_fees().0, 0, "fresh valuation: no fee yet");
    at(owner(), 0, 2_000 + FEE_SETTLEMENT_AGE_MS + 1);
    assert!(v.crystallize_fees().0 > 0);
}

// ------------------------------------------------- C1/H1 regressions (audit)

#[test]
fn audit_c1_inflated_settlement_cannot_drain_the_pool() {
    let mut v = new_vault();
    at(alice(), 100 * NEAR, 1_000);
    v.deposit();
    at(mallory(), NEAR, 1_000);
    v.deposit();
    let pool0 = v.pool;
    // executor pumps the valuation as far as the bounds allow, many times
    let mut t = 10_000u64;
    let mut ext = 0u128;
    for _ in 0..30 {
        ext += v.nav() / 11; // just under 10% each step
        at(executor(), 0, t);
        v.settle_pnl(U128(ext), 1, "pump".into());
        t += MIN_SETTLE_INTERVAL_MS + 1;
    }
    // mallory burns everything: paid exactly his pro-rata slice of the POOL,
    // regardless of the pumped valuation
    let s = v.get_shares(mallory()).0;
    let fair = mul_div(v.pool, s, v.total_shares);
    at(mallory(), 0, t);
    let before = v.pool;
    let _ = v.withdraw(U128(s));
    assert_eq!(before - v.pool, fair);
    assert!(before - v.pool <= pool0 / 100 + 1, "~1% of the pool, never alice's money");
}

#[test]
fn audit_c1_settlement_bounds() {
    let mut v = new_vault();
    at(alice(), 10 * NEAR, 1_000);
    v.deposit();
    at(executor(), 0, 2_000);
    assert!(panics(|| v.settle_pnl(U128(5 * NEAR), 1, "x".into())), "+50% in one step refused");
    v.settle_pnl(U128(NEAR / 2), 1, "ok".into());
    at(executor(), 0, 2_000 + 60_000);
    assert!(panics(|| v.settle_pnl(U128(NEAR / 2), 1, "x".into())), "within 1h refused");
    // first external value creates units owned by shareholders: no share mint
    assert_eq!(v.total_shares, 10 * NEAR);
    assert_eq!(v.vault_ext_units, v.ext_units_total);
}

// -------------------------------------------- C3 regression: repatriation

#[test]
fn audit_c3_repatriation_never_pays_more_than_returned() {
    let mut v = new_vault();
    at(alice(), 10 * NEAR, 1_000);
    v.deposit();
    at(mallory(), 10 * NEAR, 1_000);
    v.deposit();
    at(executor(), 0, 2_000);
    v.settle_pnl(U128(2 * NEAR), 1, "x".into()); // +10%
    // alice leaves: half the pool + half the units
    at(alice(), 0, 3_000);
    let _ = v.withdraw(U128(10 * NEAR));
    assert_eq!(v.pool, 10 * NEAR);
    // 1 NEAR comes back from the cross-chain wallet
    at(owner(), NEAR, 4_000);
    v.repatriate();
    assert_eq!(v.pool, 10 * NEAR + NEAR / 2, "holders' half flows to the pool");
    at(alice(), 0, 5_000);
    let _ = v.claim_external();
    assert_eq!(v.ext_paid_total, NEAR / 2, "alice gets exactly her half");
    assert!(panics(|| { let _ = v.claim_external(); }), "no double claim");
    // invariant: paid out <= repatriated
    assert!(v.ext_paid_total + (v.pool - 10 * NEAR) <= v.repatriated_total);
}

// -------------------------------------------------------------- emergency

#[test]
fn emergency_is_permissionless_one_way_and_withdrawals_still_work() {
    let mut v = new_vault();
    at(alice(), 10 * NEAR, 0);
    v.deposit();
    at(mallory(), 0, 1_000);
    assert!(panics(|| v.trigger_emergency()), "not while live");
    at(mallory(), 0, DEFAULT_LIVENESS_MS + 10_000);
    v.trigger_emergency();
    assert!(v.emergency);
    at(executor(), 0, DEFAULT_LIVENESS_MS + 20_000);
    assert!(panics(|| v.settle_pnl(U128(1), 1, "x".into())));
    at(mallory(), NEAR, DEFAULT_LIVENESS_MS + 20_000);
    assert!(panics(|| { v.deposit(); }));
    at(alice(), 0, DEFAULT_LIVENESS_MS + 30_000);
    let _ = v.withdraw(U128(10 * NEAR));
    assert_eq!(v.pool, 0);
}

// -------------------------------------------------------------- timelocks

#[test]
fn strategy_and_wallet_rotations_are_timelocked() {
    let mut v = new_vault();
    at(owner(), 0, 0);
    v.propose_strategy("ab".repeat(32), "https://new".into());
    v.propose_wallet("path-2".into(), "0x0000000000000000000000000000000000000001".into());
    at(alice(), 0, ROTATION_DELAY_MS - 1);
    assert!(panics(|| v.apply_strategy()));
    assert!(panics(|| v.apply_wallet()));
    at(alice(), 0, ROTATION_DELAY_MS + 1);
    v.apply_strategy();
    v.apply_wallet();
    assert_eq!(v.approved_wasm_hash, "ab".repeat(32));
    assert_eq!(v.mpc_path, "path-2");
}

// --------------------------------------------------------- vault-signed EVM

#[test]
fn rlp_known_vectors() {
    assert_eq!(evm::rlp_bytes(b"dog"), hex::decode("83646f67").unwrap());
    assert_eq!(evm::rlp_list(&[]), vec![0xc0]);
    assert_eq!(evm::rlp_bytes(&evm::uint(0)), vec![0x80]);
    assert_eq!(evm::rlp_bytes(&evm::uint(1024)), hex::decode("820400").unwrap());
    assert_eq!(evm::rlp_bytes(&evm::uint(15)), vec![0x0f]);
    let long = vec![0xaa; 60];
    let enc = evm::rlp_bytes(&long);
    assert_eq!(&enc[..2], &[0xb8, 60]);
}

#[test]
fn sign_evm_tx_only_for_executed_decisions_once_per_leg_with_caps() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    let d = decision("sell", 20_000, 4_000, "ETH/USDC");
    let id = commit(&mut v, &d, b"k");
    ctx(executor(), 1);
    let gwei = 1_000_000_000u128;
    // not yet revealed -> refused
    assert!(panics(|| { let _ = v.sign_evm_tx(id, "swap".into(), 0, U128(10 * gwei), U128(gwei), 200_000, U128(1), U128(1), 3000); }));
    v.execute_decision(id, d, hex::encode(b"k"));
    ctx(executor(), 1);
    // gas caps
    assert!(panics(|| { let _ = v.sign_evm_tx(id, "swap".into(), 0, U128(MAX_FEE_PER_GAS + 1), U128(gwei), 200_000, U128(1), U128(1), 3000); }));
    assert!(panics(|| { let _ = v.sign_evm_tx(id, "swap".into(), 0, U128(10 * gwei), U128(gwei), MAX_GAS_LIMIT + 1, U128(1), U128(1), 3000); }));
    // slippage guard required
    assert!(panics(|| { let _ = v.sign_evm_tx(id, "swap".into(), 0, U128(10 * gwei), U128(gwei), 200_000, U128(1), U128(0), 3000); }));
    // ok once
    let _ = v.sign_evm_tx(id, "swap".into(), 0, U128(10 * gwei), U128(gwei), 200_000, U128(2_000_000_000_000_000), U128(1), 3000);
    assert!(get_logs().iter().any(|l| l.contains("evm_sign_requested")));
    ctx(executor(), 1);
    assert!(panics(|| { let _ = v.sign_evm_tx(id, "swap".into(), 1, U128(10 * gwei), U128(gwei), 200_000, U128(1), U128(1), 3000); }), "second swap for one decision refused");
    // executor-only
    ctx(mallory(), 1);
    assert!(panics(|| { let _ = v.sign_evm_tx(id, "approve".into(), 1, U128(10 * gwei), U128(gwei), 60_000, U128(0), U128(0), 3000); }));
}

#[test]
fn swap_calldata_locks_recipient_to_the_vault() {
    // rebuild the swap the contract signs and check the recipient word
    let me = parse_evm_address(EVM);
    let mut data = hex::decode("04e45aaf").unwrap();
    data.extend(evm::word_addr(&WETH));
    data.extend(evm::word_addr(&USDC));
    data.extend(evm::word_u128(3000));
    data.extend(evm::word_addr(&me));
    assert_eq!(&data[4 + 32 * 3 + 12..4 + 32 * 4], &me[..]);
}

// ---------------------------------------------------------------- payouts

#[test]
fn failed_transfer_is_credited_back() {
    let mut v = new_vault();
    testing_env!(
        VMContextBuilder::new().current_account_id(vault_id()).predecessor_account_id(vault_id()).build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![PromiseResult::Failed],
    );
    v.on_payout(alice(), U128(5));
    assert_eq!(v.get_external_position(alice())["failed_payout_yocto"], "5");
}

#[test]
fn nav_log_is_a_bounded_ring_buffer() {
    let mut v = new_vault();
    ctx(alice(), NEAR);
    v.deposit();
    for _ in 0..(NAV_LOG_CAP + 50) {
        v.record_nav();
    }
    assert_eq!(v.nav_log.len(), NAV_LOG_CAP);
    assert_eq!(v.get_nav_history(0, 10).len(), 10);
}
