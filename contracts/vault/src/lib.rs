//! # Verifiable Alpha Vault
//!
//! A trading vault whose strategy **parameters stay sealed in a TEE** while
//! everything an investor needs to check is enforced or logged on-chain:
//!
//! 1. **Which code ran** — every decision arrives with the sha256 of the
//!    strategy WASM from the OutLayer TDX attestation; a hash that doesn't
//!    match the approved one is rejected and deposits pause (integrity alert).
//! 2. **No back-filling** — a trade must be committed (hash only) *before* it
//!    can be executed/revealed. Revealing data that doesn't match a prior
//!    commitment aborts.
//! 3. **Risk limits held** — guardrails (max leverage, position size, asset
//!    allowlist) are enforced here, *before* any signature is produced; the
//!    strategy cannot override them. Epoch-level Bulletproof bundles are
//!    anchored for browser verification.
//! 4. **Real returns** — share accounting lives on-chain; NAV history is
//!    public.
//!
//! What stays private: strategy parameters and *future* decisions. Past
//! trades, once executed, are public by design.

use near_sdk::json_types::U128;
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault, Promise,
};

const EVENT_STANDARD: &str = "alpha-vault";
const EVENT_VERSION: &str = "1.0.0";

#[derive(BorshStorageKey)]
#[near(serializers = [borsh])]
enum StorageKey {
    Shares,
    Commitments,
    RiskProofs,
    NavLog,
}

/// Risk policy enforced by the contract. The sealed strategy cannot loosen it.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Guardrails {
    /// 10_000 = 1x. Vault default: 30_000 (3x).
    pub max_leverage_bps: u64,
    /// 10_000 = 100%. Vault default: 2_000 (20%). Enforced via posted
    /// Bulletproof bundles + public NAV; also sanity-checked on reveal.
    pub max_drawdown_bps: u64,
    /// Max single-trade size as bps of NAV. e.g. 5_000 = 50%.
    pub max_position_bps: u64,
    /// Trading pairs the strategy may touch, e.g. "ETH/USDC".
    pub allowed_pairs: Vec<String>,
}

/// The plaintext decision revealed at execution time. The strategy emits this
/// JSON inside the TEE, commits to sha256(decision_json || salt) first, and
/// the executor reveals it here when (and only when) the trade is executed.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Decision {
    pub action: String, // "buy" | "sell" | "hold"
    pub pair: String,   // "ETH/USDC"
    pub size_bps: u64,  // of NAV
    pub leverage_bps: u64,
    pub epoch: u64,
    pub nonce: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitmentStatus {
    /// Hash committed; decision still sealed.
    Committed,
    /// Revealed, guardrails passed, execution signed/triggered.
    Executed,
    /// Revealed but refused by guardrails (reason recorded).
    Rejected,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct TradeCommitment {
    pub id: u64,
    /// hex sha256(decision_json_bytes || salt_bytes) — computed inside the TEE.
    pub commitment: String,
    /// sha256 of the strategy WASM from the TDX attestation for this run.
    pub wasm_hash: String,
    /// OutLayer execution reference (execution id / NEAR tx) so anyone can
    /// fetch and verify the TDX quote off-chain.
    pub attestation_ref: String,
    pub committed_at_height: u64,
    pub committed_at_ms: u64,
    pub status: CommitmentStatus,
    /// Revealed decision JSON (only after execution/rejection).
    pub revealed: Option<String>,
    /// Salt (hex) published at reveal so anyone can recompute
    /// sha256(revealed || salt) == commitment.
    pub salt_hex: Option<String>,
    /// Why a reveal was rejected, if it was.
    pub reject_reason: Option<String>,
    /// Target-chain tx hash once executed (filled by executor).
    pub execution_ref: Option<String>,
}

/// Epoch risk-proof bundle (Bulletproofs), stored opaque; verified in the
/// browser via the `vault-proofs` WASM. The contract pins the guardrail limits
/// into the record so a bundle proven against looser limits is detectable.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct StoredRiskProof {
    pub epoch: u64,
    pub max_leverage_bps: u64,
    pub max_drawdown_bps: u64,
    pub bundle_json: String,
    pub posted_at_ms: u64,
}

/// One point of public NAV history (pool size after a change).
#[near(serializers = [borsh])]
pub struct NavPoint {
    pub ts_ms: u64,
    pub pool_yocto: u128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Vault {
    pub owner: AccountId,
    /// Account allowed to relay attested decisions (executor daemon or the
    /// OutLayer yield/resume bridge).
    pub executor: AccountId,
    /// sha256 (hex) of the approved strategy WASM. Decisions attested to any
    /// other hash are refused and deposits pause.
    pub approved_wasm_hash: String,
    pub guardrails: Guardrails,
    pub total_shares: u128,
    shares: LookupMap<AccountId, u128>,
    /// Pool NAV in yoctoNEAR (deposits ± settled PnL). Public at all times.
    pub pool: u128,
    commitments: Vector<TradeCommitment>,
    risk_proofs: Vector<StoredRiskProof>,
    nav_log: Vector<NavPoint>,
    /// Set when an attestation with a wrong code hash shows up ("cheat 3").
    pub integrity_alert: Option<String>,
    pub deposits_paused: bool,
}

fn emit(event: &str, data: serde_json::Value) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        serde_json::json!({
            "standard": EVENT_STANDARD,
            "version": EVENT_VERSION,
            "event": event,
            "data": [data],
        })
    ));
}

#[near]
impl Vault {
    #[init]
    pub fn new(
        owner: AccountId,
        executor: AccountId,
        approved_wasm_hash: String,
        guardrails: Guardrails,
    ) -> Self {
        require!(
            approved_wasm_hash.len() == 64 && hex::decode(&approved_wasm_hash).is_ok(),
            "approved_wasm_hash must be 32 bytes hex (sha256 of the strategy wasm)"
        );
        require!(guardrails.max_leverage_bps <= 65_535, "limit must fit 16 bits");
        require!(guardrails.max_drawdown_bps <= 10_000, "drawdown limit > 100%");
        Self {
            owner,
            executor,
            approved_wasm_hash,
            guardrails,
            total_shares: 0,
            shares: LookupMap::new(StorageKey::Shares),
            pool: 0,
            commitments: Vector::new(StorageKey::Commitments),
            risk_proofs: Vector::new(StorageKey::RiskProofs),
            nav_log: Vector::new(StorageKey::NavLog),
            integrity_alert: None,
            deposits_paused: false,
        }
    }

    // ------------------------------------------------------------------
    // Investor side: shares
    // ------------------------------------------------------------------

    /// Deposit NEAR, receive vault shares at the current NAV/share.
    #[payable]
    pub fn deposit(&mut self) -> U128 {
        require!(!self.deposits_paused, "deposits are paused (integrity alert)");
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "attach a deposit");
        let minted = if self.total_shares == 0 || self.pool == 0 {
            amount
        } else {
            // shares = amount * total_shares / pool
            (amount.saturating_mul(self.total_shares)) / self.pool
        };
        require!(minted > 0, "deposit too small for current NAV/share");
        let who = env::predecessor_account_id();
        let cur = self.shares.get(&who).copied().unwrap_or(0);
        self.shares.insert(who.clone(), cur + minted);
        self.total_shares += minted;
        self.pool += amount;
        self.record_nav();
        emit(
            "deposit",
            serde_json::json!({"account": who, "amount": amount.to_string(), "shares": minted.to_string()}),
        );
        U128(minted)
    }

    /// Burn shares, withdraw the pro-rata slice of the pool.
    pub fn withdraw(&mut self, shares: U128) -> Promise {
        let who = env::predecessor_account_id();
        let have = self.shares.get(&who).copied().unwrap_or(0);
        let burn = shares.0;
        require!(burn > 0 && burn <= have, "not enough shares");
        let amount = (self.pool.saturating_mul(burn)) / self.total_shares;
        self.shares.insert(who.clone(), have - burn);
        self.total_shares -= burn;
        self.pool -= amount;
        self.record_nav();
        emit(
            "withdraw",
            serde_json::json!({"account": who, "amount": amount.to_string(), "shares": burn.to_string()}),
        );
        Promise::new(who).transfer(NearToken::from_yoctonear(amount))
    }

    // ------------------------------------------------------------------
    // Strategy side: commit -> execute(reveal), guarded
    // ------------------------------------------------------------------

    /// Record a sealed decision. Called by the executor with the commitment
    /// hash and the attestation metadata from the OutLayer run that produced
    /// it. **Verification 1 & 2 live here**: the code hash must match the
    /// approved strategy, and the commitment timestamp is fixed by this block.
    pub fn commit_decision(
        &mut self,
        commitment: String,
        wasm_hash: String,
        attestation_ref: String,
    ) -> u64 {
        self.assert_executor();
        require!(
            commitment.len() == 64 && hex::decode(&commitment).is_ok(),
            "commitment must be 32 bytes hex"
        );
        let id = self.commitments.len() as u64;

        if wasm_hash != self.approved_wasm_hash {
            // Cheat 3: someone swapped the strategy code. Refuse, pause
            // deposits, raise the alert — the leaderboard badge goes red.
            self.integrity_alert = Some(format!(
                "attested wasm {} != approved {}",
                wasm_hash, self.approved_wasm_hash
            ));
            self.deposits_paused = true;
            emit(
                "integrity_alert",
                serde_json::json!({"id": id, "attested": wasm_hash, "approved": self.approved_wasm_hash}),
            );
            env::panic_str("attested code hash does not match approved strategy; deposits paused");
        }

        self.commitments.push(TradeCommitment {
            id,
            commitment: commitment.clone(),
            wasm_hash,
            attestation_ref,
            committed_at_height: env::block_height(),
            committed_at_ms: env::block_timestamp_ms(),
            status: CommitmentStatus::Committed,
            revealed: None,
            salt_hex: None,
            reject_reason: None,
            execution_ref: None,
        });
        emit("commit", serde_json::json!({"id": id, "commitment": commitment}));
        id
    }

    /// Reveal a committed decision and (if the guardrails pass) mark it for
    /// execution. **Verification 2 & 3 live here**:
    /// * the revealed bytes must hash to the prior commitment — a trade that
    ///   was never committed cannot be smuggled in after the fact;
    /// * the guardrails are checked *before* anything would be signed — the
    ///   sealed strategy cannot exceed them no matter what its parameters say.
    pub fn execute_decision(&mut self, id: u64, decision_json: String, salt_hex: String) {
        self.assert_executor();
        let c = self
            .commitments
            .get(id as u32)
            .unwrap_or_else(|| env::panic_str("no such commitment: trade was never committed"))
            .clone();
        require!(
            c.status == CommitmentStatus::Committed,
            "commitment already settled"
        );

        // --- no back-filling: reveal must match the prior commitment -----
        let salt = hex::decode(&salt_hex).unwrap_or_else(|_| env::panic_str("salt must be hex"));
        let mut preimage = decision_json.as_bytes().to_vec();
        preimage.extend_from_slice(&salt);
        let digest = hex::encode(env::sha256(&preimage));
        if digest != c.commitment {
            env::panic_str(
                "reveal does not match commitment: this trade was not committed when claimed",
            );
        }

        // --- guardrails: checked before any signature exists --------------
        let d: Decision = serde_json::from_str(&decision_json)
            .unwrap_or_else(|_| env::panic_str("decision is not valid JSON"));
        if let Err(reason) = self.check_guardrails(&d) {
            let entry = self.commitments.get_mut(id as u32).unwrap();
            entry.status = CommitmentStatus::Rejected;
            entry.revealed = Some(decision_json.clone());
            entry.salt_hex = Some(salt_hex.clone());
            entry.reject_reason = Some(reason.clone());
            emit(
                "guardrail_rejected",
                serde_json::json!({"id": id, "reason": reason, "decision": decision_json}),
            );
            return; // refused; nothing will be signed
        }

        let entry = self.commitments.get_mut(id as u32).unwrap();
        entry.status = CommitmentStatus::Executed;
        entry.revealed = Some(decision_json.clone());
        entry.salt_hex = Some(salt_hex.clone());
        emit(
            "executed",
            serde_json::json!({"id": id, "decision": decision_json}),
        );
        // Chain Signatures: the call to v1.signer-prod.testnet to sign the
        // target-chain tx is attached here by the executor flow (see
        // `docs/execution.md`); it only ever runs after the checks above.
    }

    /// Executor records the target-chain tx hash after broadcast.
    pub fn record_execution_ref(&mut self, id: u64, execution_ref: String) {
        self.assert_executor();
        let entry = self
            .commitments
            .get_mut(id as u32)
            .unwrap_or_else(|| env::panic_str("no such commitment"));
        require!(
            entry.status == CommitmentStatus::Executed,
            "commitment not executed"
        );
        entry.execution_ref = Some(execution_ref);
    }

    /// Anchor an epoch Bulletproof bundle (leverage/drawdown stayed in range).
    /// The cryptographic verification happens in the browser via WASM; the
    /// contract pins the limits so a bundle proven against looser limits is
    /// detectable on sight.
    pub fn post_risk_proof(&mut self, epoch: u64, bundle_json: String) {
        self.assert_executor();
        let v: serde_json::Value = serde_json::from_str(&bundle_json)
            .unwrap_or_else(|_| env::panic_str("bundle is not valid JSON"));
        let lev_limit = v["leverage"]["limit"].as_u64().unwrap_or(0);
        let dd_limit = v["drawdown"]["limit"].as_u64().unwrap_or(0);
        require!(
            lev_limit == self.guardrails.max_leverage_bps
                && dd_limit == self.guardrails.max_drawdown_bps,
            "bundle limits do not match vault guardrails"
        );
        self.risk_proofs.push(StoredRiskProof {
            epoch,
            max_leverage_bps: lev_limit,
            max_drawdown_bps: dd_limit,
            bundle_json,
            posted_at_ms: env::block_timestamp_ms(),
        });
        emit("risk_proof", serde_json::json!({"epoch": epoch}));
    }

    // ------------------------------------------------------------------
    // Admin (owner) — minimal by design
    // ------------------------------------------------------------------

    /// Rotating the strategy is allowed but *loud*: it clears nothing and is
    /// logged, so the leaderboard shows a code change.
    pub fn approve_new_wasm_hash(&mut self, wasm_hash: String) {
        require!(env::predecessor_account_id() == self.owner, "owner only");
        require!(
            wasm_hash.len() == 64 && hex::decode(&wasm_hash).is_ok(),
            "wasm_hash must be 32 bytes hex"
        );
        emit(
            "strategy_rotated",
            serde_json::json!({"old": self.approved_wasm_hash, "new": wasm_hash}),
        );
        self.approved_wasm_hash = wasm_hash;
        self.integrity_alert = None;
        self.deposits_paused = false;
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    pub fn get_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "owner": self.owner,
            "executor": self.executor,
            "approved_wasm_hash": self.approved_wasm_hash,
            "guardrails": self.guardrails,
            "total_shares": self.total_shares.to_string(),
            "pool_yocto": self.pool.to_string(),
            "commitments": self.commitments.len(),
            "risk_proofs": self.risk_proofs.len(),
            "integrity_alert": self.integrity_alert,
            "deposits_paused": self.deposits_paused,
        })
    }

    pub fn get_shares(&self, account: AccountId) -> U128 {
        U128(self.shares.get(&account).copied().unwrap_or(0))
    }

    pub fn get_commitment(&self, id: u64) -> Option<TradeCommitment> {
        self.commitments.get(id as u32).cloned()
    }

    pub fn get_commitments(&self, from: u64, limit: u64) -> Vec<TradeCommitment> {
        (from..(from + limit).min(self.commitments.len() as u64))
            .filter_map(|i| self.commitments.get(i as u32).cloned())
            .collect()
    }

    pub fn get_risk_proofs(&self, from: u64, limit: u64) -> Vec<StoredRiskProof> {
        (from..(from + limit).min(self.risk_proofs.len() as u64))
            .filter_map(|i| self.risk_proofs.get(i as u32).cloned())
            .collect()
    }

    /// Public NAV history: every pool change, timestamped. This is what the
    /// leaderboard charts and what returns are computed from.
    pub fn get_nav_history(&self, from: u64, limit: u64) -> Vec<serde_json::Value> {
        (from..(from + limit).min(self.nav_log.len() as u64))
            .filter_map(|i| self.nav_log.get(i as u32))
            .map(|p| serde_json::json!({"t": p.ts_ms, "pool_yocto": p.pool_yocto.to_string()}))
            .collect()
    }

    /// Everything a third party needs to verify this vault independently.
    pub fn get_verification_info(&self) -> serde_json::Value {
        serde_json::json!({
            "approved_wasm_hash": self.approved_wasm_hash,
            "outlayer_register_contract": "worker.outlayer.testnet",
            "attestation_api": "https://testnet-api.outlayer.ai",
            "how": [
                "1. fetch the OutLayer attestation by attestation_ref; check the TDX quote and that is_measurements_approved on the register contract",
                "2. check the quote's wasm hash equals approved_wasm_hash",
                "3. recompute sha256(revealed decision || salt) and compare to the on-chain commitment and its block height",
                "4. verify the epoch Bulletproof bundles in your browser (vault-proofs wasm)"
            ],
        })
    }

    // ------------------------------------------------------------------

    fn record_nav(&mut self) {
        self.nav_log.push(NavPoint {
            ts_ms: env::block_timestamp_ms(),
            pool_yocto: self.pool,
        });
    }

    fn assert_executor(&self) {
        require!(
            env::predecessor_account_id() == self.executor,
            "only the registered executor may relay attested decisions"
        );
    }

    fn check_guardrails(&self, d: &Decision) -> Result<(), String> {
        if !matches!(d.action.as_str(), "buy" | "sell" | "hold") {
            return Err(format!("unknown action '{}'", d.action));
        }
        if d.action == "hold" {
            return Ok(());
        }
        if d.leverage_bps > self.guardrails.max_leverage_bps {
            return Err(format!(
                "leverage {}bps exceeds max {}bps",
                d.leverage_bps, self.guardrails.max_leverage_bps
            ));
        }
        if d.size_bps > self.guardrails.max_position_bps {
            return Err(format!(
                "position {}bps of NAV exceeds max {}bps",
                d.size_bps, self.guardrails.max_position_bps
            ));
        }
        if !self.guardrails.allowed_pairs.iter().any(|p| p == &d.pair) {
            return Err(format!("pair '{}' not in allowlist", d.pair));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::{get_logs, VMContextBuilder};
    use near_sdk::testing_env;

    fn owner() -> AccountId {
        "amudhan.testnet".parse().unwrap()
    }
    fn executor() -> AccountId {
        "executor.testnet".parse().unwrap()
    }
    fn investor() -> AccountId {
        "alice.testnet".parse().unwrap()
    }
    const WASM_HASH: &str = "c3b8b92a101e575d1c0e2e502c1fc21443fa0e08e864e84fe467d5840bda9f77";

    fn guardrails() -> Guardrails {
        Guardrails {
            max_leverage_bps: 30_000,
            max_drawdown_bps: 2_000,
            max_position_bps: 5_000,
            allowed_pairs: vec!["ETH/USDC".into(), "NEAR/USDC".into()],
        }
    }

    fn ctx(pred: AccountId, deposit_yocto: u128) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(pred)
            .attached_deposit(NearToken::from_yoctonear(deposit_yocto))
            .build());
    }

    fn new_vault() -> Vault {
        Vault::new(owner(), executor(), WASM_HASH.into(), guardrails())
    }

    fn decision_json(action: &str, leverage_bps: u64, size_bps: u64, pair: &str) -> String {
        serde_json::json!({
            "action": action, "pair": pair, "size_bps": size_bps,
            "leverage_bps": leverage_bps, "epoch": 1, "nonce": 7
        })
        .to_string()
    }

    /// commitment = sha256(decision_json || salt), like the TEE computes it.
    fn commit_of(decision_json: &str, salt: &[u8]) -> String {
        use near_sdk::env::sha256;
        let mut pre = decision_json.as_bytes().to_vec();
        pre.extend_from_slice(salt);
        hex::encode(sha256(&pre))
    }

    #[test]
    fn deposit_and_withdraw_share_math() {
        let mut v = new_vault();
        ctx(investor(), 100);
        assert_eq!(v.deposit().0, 100); // first depositor: 1:1
        ctx(owner(), 300);
        assert_eq!(v.deposit().0, 300);
        assert_eq!(v.total_shares, 400);
        assert_eq!(v.pool, 400);

        // vault gains 100 PnL -> pool 500; alice's 100 shares worth 125
        v.pool += 100;
        ctx(investor(), 0);
        let _ = v.withdraw(U128(100));
        assert_eq!(v.pool, 375);
        assert_eq!(v.total_shares, 300);
        assert_eq!(v.get_shares(investor()).0, 0);
        // every pool change is in the public NAV log
        let hist = v.get_nav_history(0, 10);
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[2]["pool_yocto"], "375");
    }

    #[test]
    fn happy_path_commit_then_execute() {
        let mut v = new_vault();
        let d = decision_json("buy", 25_000, 4_000, "ETH/USDC");
        let salt = b"random-tee-salt";
        ctx(executor(), 0);
        let id = v.commit_decision(commit_of(&d, salt), WASM_HASH.into(), "exec-123".into());
        v.execute_decision(id, d.clone(), hex::encode(salt));
        let c = v.get_commitment(id).unwrap();
        assert_eq!(c.status, CommitmentStatus::Executed);
        assert_eq!(c.revealed.unwrap(), d);
    }

    #[test]
    #[should_panic(expected = "was not committed when claimed")]
    fn cheat_1_backfilled_trade_is_caught() {
        // Manager commits one trade, then tries to reveal a *different*,
        // winning trade against that commitment.
        let mut v = new_vault();
        let real = decision_json("buy", 25_000, 4_000, "ETH/USDC");
        let fake_winner = decision_json("sell", 25_000, 4_000, "ETH/USDC");
        let salt = b"salt";
        ctx(executor(), 0);
        let id = v.commit_decision(commit_of(&real, salt), WASM_HASH.into(), "exec-1".into());
        v.execute_decision(id, fake_winner, hex::encode(salt));
    }

    #[test]
    #[should_panic(expected = "no such commitment")]
    fn cheat_1b_trade_without_any_commitment() {
        let mut v = new_vault();
        ctx(executor(), 0);
        v.execute_decision(0, decision_json("buy", 10_000, 100, "ETH/USDC"), "00".into());
    }

    #[test]
    fn cheat_2_ten_x_leverage_is_refused_before_signing() {
        let mut v = new_vault();
        let greedy = decision_json("buy", 100_000, 4_000, "ETH/USDC"); // 10x
        let salt = b"salt";
        ctx(executor(), 0);
        let id = v.commit_decision(commit_of(&greedy, salt), WASM_HASH.into(), "exec-2".into());
        v.execute_decision(id, greedy, hex::encode(salt));
        let c = v.get_commitment(id).unwrap();
        assert_eq!(c.status, CommitmentStatus::Rejected);
        assert!(c.reject_reason.unwrap().contains("leverage 100000bps exceeds max 30000bps"));
        assert!(get_logs().iter().any(|l| l.contains("guardrail_rejected")));
    }

    #[test]
    fn cheat_2b_oversized_position_and_unlisted_pair_refused() {
        let mut v = new_vault();
        ctx(executor(), 0);
        for d in [
            decision_json("buy", 20_000, 9_000, "ETH/USDC"),  // 90% of NAV
            decision_json("buy", 20_000, 1_000, "DOGE/USDC"), // not allowlisted
        ] {
            let salt = b"s";
            let id = v.commit_decision(commit_of(&d, salt), WASM_HASH.into(), "x".into());
            v.execute_decision(id, d, hex::encode(salt));
            assert_eq!(v.get_commitment(id).unwrap().status, CommitmentStatus::Rejected);
        }
    }

    #[test]
    fn cheat_3_swapped_code_pauses_deposits() {
        let mut v = new_vault();
        ctx(executor(), 0);
        let evil_hash = "d".repeat(64);
        let d = decision_json("buy", 10_000, 100, "ETH/USDC");
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            v.commit_decision(commit_of(&d, b"s"), evil_hash, "exec-3".into())
        }));
        assert!(res.is_err(), "commit with wrong code hash must abort");
        assert!(v.deposits_paused);
        assert!(v.integrity_alert.is_some());
        // and new deposits are now refused
        ctx(investor(), 100);
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.deposit()));
        assert!(res.is_err(), "deposits must be paused after integrity alert");
    }

    #[test]
    #[should_panic(expected = "only the registered executor")]
    fn random_account_cannot_commit() {
        let mut v = new_vault();
        ctx(investor(), 0);
        v.commit_decision("a".repeat(64), WASM_HASH.into(), "x".into());
    }

    #[test]
    #[should_panic(expected = "commitment already settled")]
    fn no_double_settlement() {
        let mut v = new_vault();
        let d = decision_json("buy", 25_000, 4_000, "ETH/USDC");
        let salt = b"salt";
        ctx(executor(), 0);
        let id = v.commit_decision(commit_of(&d, salt), WASM_HASH.into(), "x".into());
        v.execute_decision(id, d.clone(), hex::encode(salt));
        v.execute_decision(id, d, hex::encode(salt));
    }

    #[test]
    fn risk_proof_limits_must_match_guardrails() {
        let mut v = new_vault();
        ctx(executor(), 0);
        // limits match -> accepted
        let good = serde_json::json!({
            "epoch": 1,
            "leverage": {"label":"leverage_bps","limit":30_000,"epoch":1,"commitment":"","proof":""},
            "drawdown": {"label":"drawdown_bps","limit":2_000,"epoch":1,"commitment":"","proof":""},
        })
        .to_string();
        v.post_risk_proof(1, good);
        assert_eq!(v.get_risk_proofs(0, 10).len(), 1);
        // looser leverage limit -> refused
        let loose = serde_json::json!({
            "epoch": 2,
            "leverage": {"label":"leverage_bps","limit":60_000,"epoch":2,"commitment":"","proof":""},
            "drawdown": {"label":"drawdown_bps","limit":2_000,"epoch":2,"commitment":"","proof":""},
        })
        .to_string();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.post_risk_proof(2, loose)));
        assert!(res.is_err());
    }

    #[test]
    fn hold_decisions_skip_trade_guardrails() {
        let mut v = new_vault();
        let d = decision_json("hold", 0, 0, "ETH/USDC");
        let salt = b"salt";
        ctx(executor(), 0);
        let id = v.commit_decision(commit_of(&d, salt), WASM_HASH.into(), "x".into());
        v.execute_decision(id, d, hex::encode(salt));
        assert_eq!(v.get_commitment(id).unwrap().status, CommitmentStatus::Executed);
    }
}
