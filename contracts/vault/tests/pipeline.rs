//! End-to-end pipeline test (no network): the exact flow of a live vault,
//! with the enclave run simulated by calling the same strategy code natively.
//!
//!   investor deposits
//!     -> strategy runs with SEALED params (vault-strategy)
//!     -> commitment lands on the vault BEFORE execution (vault-contract)
//!     -> reveal + guardrail check -> Executed
//!     -> epoch risk bundle posted and VERIFIED (vault-proofs)
//!
//! Plus the three cheats: back-filled trade, 10x leverage, swapped code.

use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, NearToken};
use vault_contract::{CommitmentStatus, Guardrails, Vault};
use vault_proofs::{verify_risk, RiskLimits};
use vault_strategy::{run, Params, RunInput};

const WASM_HASH: &str = "c3b8b92a101e575d1c0e2e502c1fc21443fa0e08e864e84fe467d5840bda9f77";

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn ctx(pred: &str, deposit_yocto: u128) {
    testing_env!(VMContextBuilder::new()
        .predecessor_account_id(acct(pred))
        .attached_deposit(NearToken::from_yoctonear(deposit_yocto))
        .build());
}

fn vault() -> Vault {
    Vault::new(
        acct("amudhan.testnet"),
        acct("executor.testnet"),
        WASM_HASH.into(),
        "https://test.fastfs.io/demo.wasm".into(),
        Guardrails {
            max_leverage_bps: 30_000,
            max_drawdown_bps: 2_000,
            max_position_bps: 5_000,
            allowed_pairs: vec!["ETH/USDC".into()],
        },
        "alpha-vault-v1".into(),
        "0x18f92b8ee2dc4bc3bbbdb4138e92f184bc405b7f".into(),
    )
}

fn state_input(v: &Vault) -> String {
    serde_json::json!({"vault_state": {
        "pool_yocto": v.pool.to_string(), "total_shares": v.total_shares.to_string(),
        "hwm_nps": v.hwm_nav_per_share.to_string(), "external_value_yocto": v.holders_ext_value().to_string(),
        "max_leverage_bps": 30_000, "max_drawdown_bps": 2_000 }}).to_string()
}

fn market_input() -> RunInput {
    RunInput {
        epoch: 1,
        nonce: 1,
        pair: "ETH/USDC".into(),
        prices: vec![100., 100., 100., 100., 100., 102., 105., 109., 114., 120.],
        nav_yocto: "990".into(),
        peak_nav_yocto: "1000".into(),
        max_leverage_bps: 30_000,
        max_drawdown_bps: 2_000,
        settle: None,
        vault_state: None,
        compact: false,
    }
}

#[test]
fn full_pipeline_honest_vault() {
    let mut v = vault();

    // investor deposits 1000 yocto
    ctx("alice.testnet", 1_000_000_000_000_000_000_000_000);
    v.deposit();

    // the "enclave" runs the strategy with sealed params
    let sealed_params = Params {
        fast: 3,
        slow: 8,
        threshold_bps: 50,
        size_bps: 4_000,
        leverage_bps: 25_000,
        seal_key_hex: None,
    };
    let out = run(&sealed_params, &market_input(), [42u8; 32]);
    let bundle = out.risk_bundle.expect("honest run proves risk");

    // executor commits the hash FIRST (this is what makes back-filling impossible)
    ctx("executor.testnet", 100_000_000_000_000_000_000_000);
    let id = v.commit_decision(out.commitment.clone(), WASM_HASH.into(), "outlayer-exec-1".into(), state_input(&v));

    // ...trade executes on the target chain... then reveal
    v.execute_decision(id, out.sealed.as_ref().unwrap().decision_json.clone(), out.sealed.as_ref().unwrap().salt_hex.clone());
    let c = v.get_commitment(id).unwrap();
    assert_eq!(c.status, CommitmentStatus::Executed);

    // epoch risk proof: posted on-chain, verified like the browser does
    let bundle_json = serde_json::to_string(&bundle).unwrap();
    v.post_risk_proof(1, bundle_json);
    let stored = &v.get_risk_proofs(0, 10)[0];
    let parsed = serde_json::from_str(&stored.bundle_json).unwrap();
    verify_risk(
        &parsed,
        &RiskLimits { max_leverage_bps: 30_000, max_drawdown_bps: 2_000 },
    )
    .expect("browser-side verification passes");
}

#[test]
fn full_pipeline_greedy_params_are_stopped_twice() {
    let mut v = vault();
    ctx("alice.testnet", 1_000_000_000_000_000_000_000_000);
    v.deposit();

    // malicious sealed params: 10x leverage
    let greedy = Params {
        fast: 3,
        slow: 8,
        threshold_bps: 50,
        size_bps: 4_000,
        leverage_bps: 100_000,
        seal_key_hex: None,
    };
    let out = run(&greedy, &market_input(), [42u8; 32]);

    // layer 1: no risk proof can exist
    assert!(out.risk_bundle.is_none());

    // layer 2: the contract refuses the reveal before anything is signed
    ctx("executor.testnet", 100_000_000_000_000_000_000_000);
    let id = v.commit_decision(out.commitment.clone(), WASM_HASH.into(), "outlayer-exec-2".into(), state_input(&v));
    v.execute_decision(id, out.sealed.as_ref().unwrap().decision_json.clone(), out.sealed.as_ref().unwrap().salt_hex.clone());
    let c = v.get_commitment(id).unwrap();
    assert_eq!(c.status, CommitmentStatus::Rejected);
    assert!(c.reject_reason.unwrap().contains("exceeds max"));
}

#[test]
#[should_panic(expected = "was not committed when claimed")]
fn full_pipeline_backfilled_winner_is_caught() {
    let mut v = vault();
    let params = Params { fast: 3, slow: 8, threshold_bps: 50, size_bps: 4_000, leverage_bps: 25_000, seal_key_hex: None };

    // honest commit...
    let out = run(&params, &market_input(), [42u8; 32]);
    ctx("executor.testnet", 100_000_000_000_000_000_000_000);
    let id = v.commit_decision(out.commitment, WASM_HASH.into(), "outlayer-exec-3".into(), state_input(&v));

    // ...but the manager tries to reveal a different, winning trade
    let mut fake_input = market_input();
    fake_input.prices = vec![120., 120., 120., 120., 120., 117., 112., 106., 100., 95.];
    let fake = run(&params, &fake_input, [42u8; 32]);
    v.execute_decision(id, fake.sealed.as_ref().unwrap().decision_json.clone(), fake.sealed.as_ref().unwrap().salt_hex.clone());
}

#[test]
fn full_pipeline_swapped_code_is_detected() {
    let mut v = vault();
    let params = Params { fast: 3, slow: 8, threshold_bps: 50, size_bps: 4_000, leverage_bps: 25_000, seal_key_hex: None };
    let out = run(&params, &market_input(), [42u8; 32]);
    ctx("executor.testnet", 100_000_000_000_000_000_000_000);
    // attestation says different code ran: recorded as Rejected (not a panic,
    // which would revert the alert on-chain) and deposits pause
    let other_hash = "ab".repeat(32);
    let id = v.commit_decision(out.commitment, other_hash, "outlayer-exec-4".into(), state_input(&v));
    assert_eq!(v.get_commitment(id).unwrap().status, CommitmentStatus::Rejected);
    assert!(v.deposits_paused);
    ctx("alice.testnet", 1_000_000_000_000_000_000_000_000);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.deposit()));
    assert!(res.is_err(), "deposits must be paused");
}
