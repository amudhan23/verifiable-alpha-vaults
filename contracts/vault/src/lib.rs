//! # Verifiable Alpha Vault (v4)
//!
//! A trading vault whose strategy **parameters stay sealed in a TEE** while
//! everything an investor needs to check is enforced or logged on-chain.
//!
//! v4 is the post-audit redesign. An independent adversarial review of v3
//! found that a semi-trusted executor could misreport cross-chain value and
//! drain the pool, that the cross-chain wallet was controlled by the
//! executor's key rather than the vault, and that emergency claims could pay
//! out more than was recovered. v4 closes all three:
//!
//! * **Withdrawals never depend on reported cross-chain value.** Burning
//!   shares pays your exact pro-rata slice of the NEAR pool *plus* the same
//!   slice of the vault's "external units": a pro-rata claim on the
//!   cross-chain wallet. A misreported valuation cannot pull NEAR out.
//! * **The vault owns its cross-chain wallet.** The contract itself builds the
//!   exact EVM transaction (recipient locked to the vault's own address, only
//!   WETH/Uniswap targets, capped gas) and requests the MPC signature. The
//!   executor only broadcasts what the vault signed.
//! * **Cross-chain value is paid out with an accumulator.** Repatriated NEAR
//!   is distributed per external unit, so total payouts can never exceed what
//!   was actually returned.
//! * **Settlements are bounded.** Each moves NAV by at most 10% and they are at
//!   least 1h apart; performance fees only crystallize on a valuation that has
//!   stood for 24h. What remains (a semi-trusted valuation inside those bounds)
//!   is publicly fraud-provable, and removed entirely by the on-chain attested
//!   path (`request_decision`).
//!
//! What stays private: strategy parameters and *future* decisions. Past
//! trades, once executed, are public by design.

use near_contract_standards::non_fungible_token::metadata::{
    NFTContractMetadata, NonFungibleTokenMetadataProvider, TokenMetadata, NFT_METADATA_SPEC,
};
use near_contract_standards::non_fungible_token::{NonFungibleToken, Token, TokenId};
use near_sdk::collections::LazyOption;
use near_sdk::json_types::U128;
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    env, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise,
    PromiseOrValue, PromiseResult,
};

const EVENT_STANDARD: &str = "alpha-vault";
const EVENT_VERSION: &str = "4.0.0";
/// Scale for NAV-per-share fixed-point math.
const NPS_SCALE: u128 = 1_000_000_000_000;
/// Scale for the per-external-unit payout accumulator.
const ACC_SCALE: u128 = 1_000_000_000_000_000_000_000_000;
const ONE_NEAR: u128 = 1_000_000_000_000_000_000_000_000;

/// Smallest accepted deposit. Pays for the depositor's storage many times
/// over, so spam deposits can't drain the contract's storage balance.
pub const MIN_DEPOSIT: u128 = ONE_NEAR / 10;
/// 24h without an attested decision -> anyone may trigger emergency exit.
pub const DEFAULT_LIVENESS_MS: u64 = 24 * 3_600_000;
/// Timelock for anything that changes what code runs or which keys sign.
pub const ROTATION_DELAY_MS: u64 = 48 * 3_600_000;
/// Settlement bounds: max NAV move per settlement, min spacing.
pub const MAX_SETTLE_STEP_BPS: u128 = 1_000;
pub const MIN_SETTLE_INTERVAL_MS: u64 = 3_600_000;
/// Fees crystallize only on a valuation that has stood this long.
pub const FEE_SETTLEMENT_AGE_MS: u64 = 24 * 3_600_000;
const NAV_LOG_CAP: u32 = 2_000;
/// Fixed unit count created with the first external value (1e24): with
/// ACC_SCALE 1e24, repatriations up to ~1e14 NEAR stay inside u128.
pub const EXT_UNITS_BASE: u128 = ONE_NEAR;

// String caps (storage-griefing limits).
const MAX_INPUT_JSON: usize = 6_000;
const MAX_BUNDLE_JSON: usize = 8_000;
const MAX_REF: usize = 300;
const MAX_DECISION_JSON: usize = 600;

// EVM execution target: Ethereum Sepolia, Uniswap v3 (addresses verified
// on-chain). The vault will only ever sign transactions to these.
pub const EVM_CHAIN_ID: u64 = 11_155_111;
pub const MPC_SIGNER: &str = "v1.signer-prod.testnet";
const WETH: [u8; 20] = hex20("fff9976782d46cc05630d1f6ebab18b2324d6b14");
const USDC: [u8; 20] = hex20("1c7d4b196cb0c7b01d743fbc6116a902379c7238");
const ROUTER: [u8; 20] = hex20("3bfa4769fb09eefc5a80d6e87c3b9c650f7ae48e");
const MAX_FEE_PER_GAS: u128 = 300_000_000_000; // 300 gwei
const MAX_GAS_LIMIT: u64 = 400_000;

const fn hex20(s: &str) -> [u8; 20] {
    let b = s.as_bytes();
    let mut out = [0u8; 20];
    let mut i = 0;
    while i < 20 {
        out[i] = (nib(b[2 * i]) << 4) | nib(b[2 * i + 1]);
        i += 1;
    }
    out
}
const fn nib(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("bad hex"),
    }
}

uint::construct_uint! {
    /// 256-bit unsigned, for share math: yoctoNEAR (1e24) × share counts
    /// (~1e24) overflows u128 — `saturating_mul` would silently clamp.
    pub struct U256(4);
}

/// floor(a × b / c) with a 256-bit intermediate. Panics if c == 0 or the
/// result exceeds u128 (impossible for vault-sized values).
pub fn mul_div(a: u128, b: u128, c: u128) -> u128 {
    require!(c != 0, "mul_div by zero");
    let r = U256::from(a) * U256::from(b) / U256::from(c);
    require!(r <= U256::from(u128::MAX), "mul_div overflow");
    r.as_u128()
}

// ---------------------------------------------------------------------------
// Minimal RLP + EIP-1559 transaction encoding (so the vault, not a relayer,
// decides exactly what gets signed).
// ---------------------------------------------------------------------------
pub mod evm {
    fn be_trim(mut v: Vec<u8>) -> Vec<u8> {
        while v.first() == Some(&0) {
            v.remove(0);
        }
        v
    }
    pub fn uint(x: u128) -> Vec<u8> {
        be_trim(x.to_be_bytes().to_vec())
    }
    fn len_prefix(len: usize, short: u8) -> Vec<u8> {
        if len < 56 {
            vec![short + len as u8]
        } else {
            let l = be_trim((len as u64).to_be_bytes().to_vec());
            let mut out = vec![short + 55 + l.len() as u8];
            out.extend(l);
            out
        }
    }
    pub fn rlp_bytes(b: &[u8]) -> Vec<u8> {
        if b.len() == 1 && b[0] < 0x80 {
            return b.to_vec();
        }
        let mut out = len_prefix(b.len(), 0x80);
        out.extend_from_slice(b);
        out
    }
    pub fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = items.concat();
        let mut out = len_prefix(body.len(), 0xc0);
        out.extend(body);
        out
    }
    pub struct Tx1559 {
        pub chain_id: u64,
        pub nonce: u64,
        pub max_priority_fee_per_gas: u128,
        pub max_fee_per_gas: u128,
        pub gas_limit: u64,
        pub to: [u8; 20],
        pub value: u128,
        pub data: Vec<u8>,
    }
    impl Tx1559 {
        /// 0x02 || rlp([chainId, nonce, maxPrio, maxFee, gas, to, value, data, []])
        pub fn signing_bytes(&self) -> Vec<u8> {
            let mut out = vec![0x02];
            out.extend(rlp_list(&[
                rlp_bytes(&uint(self.chain_id as u128)),
                rlp_bytes(&uint(self.nonce as u128)),
                rlp_bytes(&uint(self.max_priority_fee_per_gas)),
                rlp_bytes(&uint(self.max_fee_per_gas)),
                rlp_bytes(&uint(self.gas_limit as u128)),
                rlp_bytes(&self.to),
                rlp_bytes(&uint(self.value)),
                rlp_bytes(&self.data),
                rlp_list(&[]),
            ]));
            out
        }
    }
    pub fn word_addr(a: &[u8; 20]) -> Vec<u8> {
        let mut w = vec![0u8; 12];
        w.extend_from_slice(a);
        w
    }
    pub fn word_u128(x: u128) -> Vec<u8> {
        let mut w = vec![0u8; 16];
        w.extend_from_slice(&x.to_be_bytes());
        w
    }
    pub fn word_max() -> Vec<u8> {
        vec![0xff; 32]
    }
}

#[derive(BorshStorageKey)]
#[near(serializers = [borsh])]
enum StorageKey {
    Shares,
    Commitments,
    RiskProofs,
    NavLog,
    NftOwner,
    NftApproval,
    NftEnumeration,
    NftMetadata,
    NftContractMetadata,
    Claims,    // v3 (retired; kept so later key indices don't shift)
    ClaimPaid, // v3 (retired)
    ExtUnits,
    ExtDebt,
    ExtPending,
    SignedLegs,
    FailedPayouts,
}

pub const STRATEGY_TOKEN_ID: &str = "strategy-1";

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Guardrails {
    pub max_leverage_bps: u64,
    pub max_drawdown_bps: u64,
    pub max_position_bps: u64,
    pub allowed_pairs: Vec<String>,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Decision {
    pub action: String,
    pub pair: String,
    pub size_bps: u64,
    pub leverage_bps: u64,
    pub epoch: u64,
    pub nonce: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitmentStatus {
    Committed,
    Executed,
    Rejected,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct TradeCommitment {
    pub id: u64,
    pub commitment: String,
    pub wasm_hash: String,
    pub attestation_ref: String,
    pub committed_at_height: u64,
    pub committed_at_ms: u64,
    pub status: CommitmentStatus,
    pub revealed: Option<String>,
    pub salt_hex: Option<String>,
    pub reject_reason: Option<String>,
    pub execution_ref: Option<String>,
    pub sealed_enc: Option<String>,
    pub input_json: Option<String>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct StoredRiskProof {
    pub epoch: u64,
    pub max_leverage_bps: u64,
    pub max_drawdown_bps: u64,
    pub bundle_json: String,
    pub posted_at_ms: u64,
}

#[near(serializers = [borsh])]
pub struct NavPoint {
    pub ts_ms: u64,
    pub pool_yocto: u128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Vault {
    pub owner: AccountId,
    pub executor: AccountId,
    pub approved_wasm_hash: String,
    pub guardrails: Guardrails,
    pub total_shares: u128,
    shares: LookupMap<AccountId, u128>,
    /// NEAR held by the contract for shareholders.
    pub pool: u128,
    /// Settled value of the WHOLE cross-chain wallet (yoctoNEAR equivalent).
    pub ext_value: u128,
    pub last_settlement_ref: Option<String>,
    pub last_settlement_ms: u64,
    pub strategy_wasm_url: String,
    commitments: Vector<TradeCommitment>,
    risk_proofs: Vector<StoredRiskProof>,
    nav_log: Vector<NavPoint>,
    nav_log_head: u32,
    pub perf_fee_bps: u64,
    pub hwm_nav_per_share: u128,
    pub integrity_alert: Option<String>,
    pub deposits_paused: bool,
    nft: NonFungibleToken,
    nft_metadata: LazyOption<NFTContractMetadata>,
    pub last_attested_ms: u64,
    pub liveness_timeout_ms: u64,
    pub emergency: bool,
    // ---- external units: pro-rata ownership of the cross-chain wallet ----
    pub ext_units_total: u128,
    /// Units still owned collectively by shareholders.
    pub vault_ext_units: u128,
    ext_units: LookupMap<AccountId, u128>,
    ext_debt: LookupMap<AccountId, u128>,
    ext_pending: LookupMap<AccountId, u128>,
    /// Repatriated NEAR paid per external unit, scaled by ACC_SCALE.
    pub ext_acc: u128,
    pub repatriated_total: u128,
    pub ext_paid_total: u128,
    // ---- vault-owned cross-chain wallet ----
    pub mpc_path: String,
    /// The EVM address MPC derives for (this contract, mpc_path). Every swap's
    /// recipient is locked to it.
    pub evm_address: String,
    pub pending_wallet: Option<(String, String, u64)>,
    pub pending_strategy: Option<(String, String, u64)>,
    signed_legs: LookupMap<String, bool>,
    failed_payouts: LookupMap<AccountId, u128>,
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

fn parse_evm_address(a: &str) -> [u8; 20] {
    let h = a.strip_prefix("0x").unwrap_or(a);
    let v = hex::decode(h).unwrap_or_else(|_| env::panic_str("evm address must be hex"));
    require!(v.len() == 20, "evm address must be 20 bytes");
    let mut out = [0u8; 20];
    out.copy_from_slice(&v);
    out
}

#[near]
impl Vault {
    #[init]
    pub fn new(
        owner: AccountId,
        executor: AccountId,
        approved_wasm_hash: String,
        strategy_wasm_url: String,
        guardrails: Guardrails,
        mpc_path: String,
        evm_address: String,
    ) -> Self {
        require!(
            approved_wasm_hash.len() == 64 && hex::decode(&approved_wasm_hash).is_ok(),
            "approved_wasm_hash must be 32 bytes hex (sha256 of the strategy wasm)"
        );
        require!(guardrails.max_leverage_bps <= 65_535, "limit must fit 16 bits");
        require!(guardrails.max_drawdown_bps <= 10_000, "drawdown limit > 100%");
        require!(guardrails.max_position_bps <= 10_000, "position limit > 100%");
        parse_evm_address(&evm_address);
        let mut nft = NonFungibleToken::new(
            StorageKey::NftOwner,
            owner.clone(),
            Some(StorageKey::NftMetadata),
            Some(StorageKey::NftEnumeration),
            Some(StorageKey::NftApproval),
        );
        nft.internal_mint_with_refund(
            STRATEGY_TOKEN_ID.to_string(),
            owner.clone(),
            Some(TokenMetadata {
                title: Some("Alpha Vault Strategy #1".into()),
                description: Some(format!(
                    "Sealed-parameter strategy attested as wasm {}. Holder earns performance fees and controls the (timelocked) strategy and wallet rotations.",
                    approved_wasm_hash
                )),
                media: None, media_hash: None, copies: Some(1), issued_at: None,
                expires_at: None, starts_at: None, updated_at: None, extra: None,
                reference: None, reference_hash: None,
            }),
            None,
        );
        Self {
            owner,
            executor,
            approved_wasm_hash,
            guardrails,
            total_shares: 0,
            shares: LookupMap::new(StorageKey::Shares),
            pool: 0,
            ext_value: 0,
            last_settlement_ref: None,
            last_settlement_ms: 0,
            strategy_wasm_url,
            commitments: Vector::new(StorageKey::Commitments),
            risk_proofs: Vector::new(StorageKey::RiskProofs),
            nav_log: Vector::new(StorageKey::NavLog),
            nav_log_head: 0,
            perf_fee_bps: 2_000,
            hwm_nav_per_share: NPS_SCALE,
            integrity_alert: None,
            deposits_paused: false,
            nft,
            nft_metadata: LazyOption::new(
                StorageKey::NftContractMetadata,
                Some(&NFTContractMetadata {
                    spec: NFT_METADATA_SPEC.to_string(),
                    name: "Verifiable Alpha Vault Strategies".to_string(),
                    symbol: "VAVS".to_string(),
                    icon: None, base_uri: None, reference: None, reference_hash: None,
                }),
            ),
            last_attested_ms: env::block_timestamp_ms(),
            liveness_timeout_ms: DEFAULT_LIVENESS_MS,
            emergency: false,
            ext_units_total: 0,
            vault_ext_units: 0,
            ext_units: LookupMap::new(StorageKey::ExtUnits),
            ext_debt: LookupMap::new(StorageKey::ExtDebt),
            ext_pending: LookupMap::new(StorageKey::ExtPending),
            ext_acc: 0,
            repatriated_total: 0,
            ext_paid_total: 0,
            mpc_path,
            evm_address,
            pending_wallet: None,
            pending_strategy: None,
            signed_legs: LookupMap::new(StorageKey::SignedLegs),
            failed_payouts: LookupMap::new(StorageKey::FailedPayouts),
        }
    }

    // ==================================================================
    // Shares
    // ==================================================================

    /// Deposit NEAR, receive shares at the current NAV/share.
    #[payable]
    pub fn deposit(&mut self) -> U128 {
        require!(!self.emergency, "vault is in emergency exit mode");
        require!(!self.deposits_paused, "deposits are paused (integrity alert)");
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount >= MIN_DEPOSIT, "minimum deposit is 0.1 NEAR");
        self.crystallize_fees();
        let nav = self.nav();
        let minted = match (self.total_shares, nav) {
            (0, 0) => amount,
            (0, _) => env::panic_str("vault holds value but no shares: refusing to hand it to the next depositor"),
            (_, 0) => env::panic_str("vault NAV is zero: refusing to dilute the deposit into worthless shares"),
            (ts, n) => mul_div(amount, ts, n),
        };
        require!(minted > 0, "deposit too small for current NAV/share");
        let who = env::predecessor_account_id();
        let cur = self.shares.get(&who).copied().unwrap_or(0);
        self.shares.insert(who.clone(), cur + minted);
        self.total_shares += minted;
        self.pool += amount;
        self.record_nav();
        emit("deposit", serde_json::json!({"account": who, "amount": amount.to_string(), "shares": minted.to_string()}));
        U128(minted)
    }

    /// Burn shares. Pays exactly `burn/total` of the NEAR pool now and moves
    /// exactly `burn/total` of the shareholders' external units to you. The
    /// payout never depends on the reported cross-chain valuation, so a
    /// misreported settlement cannot extract anyone's NEAR.
    pub fn withdraw(&mut self, shares: U128) -> Promise {
        let who = env::predecessor_account_id();
        if !self.emergency {
            self.crystallize_fees();
        }
        let have = self.shares.get(&who).copied().unwrap_or(0);
        let burn = shares.0;
        require!(burn > 0 && burn <= have, "not enough shares");
        let local = mul_div(self.pool, burn, self.total_shares);
        let units = if self.vault_ext_units > 0 {
            mul_div(self.vault_ext_units, burn, self.total_shares)
        } else {
            0
        };
        self.shares.insert(who.clone(), have - burn);
        self.total_shares -= burn;
        self.pool -= local;
        if units > 0 {
            self.accrue(&who);
            self.vault_ext_units -= units;
            let u = self.ext_units.get(&who).copied().unwrap_or(0);
            self.ext_units.insert(who.clone(), u + units); // snapshot set by accrue()
        }
        if self.total_shares == 0 {
            // last holder out: rounding dust stays unaccounted (never owed),
            // and the next generation starts at NAV/share 1.0
            self.pool = 0;
            self.hwm_nav_per_share = NPS_SCALE;
        }
        self.record_nav();
        emit("withdraw", serde_json::json!({
            "account": who, "shares": burn.to_string(),
            "paid_yocto": local.to_string(), "ext_units": units.to_string(),
        }));
        self.pay(who, local)
    }

    // ==================================================================
    // Cross-chain value: units, repatriation, payouts
    // ==================================================================

    /// Return NEAR obtained from the cross-chain wallet (anyone may call:
    /// executor, manager, or a rescuer after an emergency). It is distributed
    /// per external unit: the shareholders' part flows back into the pool,
    /// unit holders accrue theirs. Payouts can never exceed what came back.
    #[payable]
    pub fn repatriate(&mut self) {
        let r = env::attached_deposit().as_yoctonear();
        require!(r > 0, "attach the repatriated NEAR");
        require!(self.ext_units_total > 0, "no external units outstanding");
        require!(self.ext_units_total >= EXT_UNITS_BASE / 1_000, "external units too few to distribute safely");
        let to_vault = mul_div(r, self.vault_ext_units, self.ext_units_total);
        self.pool += to_vault;
        self.ext_acc += mul_div(r, ACC_SCALE, self.ext_units_total);
        self.ext_value = self.ext_value.saturating_sub(r);
        self.repatriated_total += r;
        self.record_nav();
        emit("repatriated", serde_json::json!({"amount": r.to_string(), "to_pool": to_vault.to_string()}));
    }

    /// Pay out the caller's accrued share of repatriated NEAR.
    pub fn claim_external(&mut self) -> Promise {
        let who = env::predecessor_account_id();
        self.accrue(&who);
        let due = self.ext_pending.get(&who).copied().unwrap_or(0);
        require!(due > 0, "nothing to claim yet");
        self.ext_pending.insert(who.clone(), 0);
        self.ext_paid_total += due;
        emit("external_claimed", serde_json::json!({"account": who, "amount": due.to_string()}));
        self.pay(who, due)
    }

    /// Credit `units × (acc − acc_at_last_accrual)`, floored. Flooring the
    /// DIFFERENCE (not each total) means no account is ever credited more
    /// than its exact share, so the contract stays solvent to the yocto.
    /// (`ext_debt` stores each account's accumulator snapshot.)
    fn accrue(&mut self, who: &AccountId) {
        let u = self.ext_units.get(who).copied().unwrap_or(0);
        let snap = self.ext_debt.get(who).copied().unwrap_or(self.ext_acc);
        if u > 0 && self.ext_acc > snap {
            let p = self.ext_pending.get(who).copied().unwrap_or(0);
            self.ext_pending.insert(who.clone(), p + mul_div(u, self.ext_acc - snap, ACC_SCALE));
        }
        self.ext_debt.insert(who.clone(), self.ext_acc);
    }

    pub fn get_external_position(&self, account: AccountId) -> serde_json::Value {
        let u = self.ext_units.get(&account).copied().unwrap_or(0);
        let snap = self.ext_debt.get(&account).copied().unwrap_or(self.ext_acc);
        let pending = self.ext_pending.get(&account).copied().unwrap_or(0)
            + if u > 0 { mul_div(u, self.ext_acc.saturating_sub(snap), ACC_SCALE) } else { 0 };
        let value = if self.ext_units_total > 0 { mul_div(self.ext_value, u, self.ext_units_total) } else { 0 };
        serde_json::json!({
            "units": u.to_string(),
            "est_value_yocto": value.to_string(),
            "claimable_yocto": pending.to_string(),
            "failed_payout_yocto": self.failed_payouts.get(&account).copied().unwrap_or(0).to_string(),
        })
    }

    /// Transfers that fail (e.g. a deleted account) are credited back here
    /// instead of vanishing from the books.
    fn pay(&mut self, to: AccountId, amount: u128) -> Promise {
        Promise::new(to.clone())
            .transfer(NearToken::from_yoctonear(amount))
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(5))
                    .on_payout(to, U128(amount)),
            )
    }

    #[private]
    pub fn on_payout(&mut self, to: AccountId, amount: U128) {
        if !matches!(env::promise_result(0), PromiseResult::Successful(_)) {
            let f = self.failed_payouts.get(&to).copied().unwrap_or(0);
            self.failed_payouts.insert(to.clone(), f + amount.0);
            emit("payout_failed", serde_json::json!({"account": to, "amount": amount.0.to_string()}));
        }
    }

    pub fn retry_payout(&mut self) -> Promise {
        let who = env::predecessor_account_id();
        let f = self.failed_payouts.get(&who).copied().unwrap_or(0);
        require!(f > 0, "no failed payout");
        self.failed_payouts.insert(who.clone(), 0);
        self.pay(who, f)
    }

    // ==================================================================
    // Strategy: commit -> reveal (guarded) -> vault-signed execution
    // ==================================================================

    pub fn commit_decision(
        &mut self,
        commitment: String,
        wasm_hash: String,
        attestation_ref: String,
        input_json: String,
    ) -> u64 {
        self.assert_executor();
        require!(!self.emergency, "vault is in emergency exit mode");
        require!(commitment.len() == 64 && hex::decode(&commitment).is_ok(), "commitment must be 32 bytes hex");
        require!(attestation_ref.len() <= MAX_REF, "attestation_ref too long");
        require!(input_json.len() <= MAX_INPUT_JSON, "input_json too long");
        if let Err(e) = self.check_input_state(&input_json) {
            env::panic_str(&e);
        }
        let id = self.commitments.len() as u64;
        let honest = wasm_hash == self.approved_wasm_hash;
        if !honest {
            // recorded, not reverted (a panic would roll back the alert)
            self.integrity_alert = Some(format!("attested wasm {} != approved {}", wasm_hash, self.approved_wasm_hash));
            self.deposits_paused = true;
            emit("integrity_alert", serde_json::json!({"id": id, "attested": wasm_hash, "approved": self.approved_wasm_hash}));
        }
        self.commitments.push(TradeCommitment {
            id,
            commitment: commitment.clone(),
            wasm_hash,
            attestation_ref,
            committed_at_height: env::block_height(),
            committed_at_ms: env::block_timestamp_ms(),
            status: if honest { CommitmentStatus::Committed } else { CommitmentStatus::Rejected },
            revealed: None,
            salt_hex: None,
            reject_reason: if honest { None } else { Some("attested code hash does not match approved strategy; deposits paused".into()) },
            execution_ref: None,
            sealed_enc: None,
            input_json: Some(input_json),
        });
        if honest {
            self.last_attested_ms = env::block_timestamp_ms();
            emit("commit", serde_json::json!({"id": id, "commitment": commitment}));
        }
        id
    }

    pub fn execute_decision(&mut self, id: u64, decision_json: String, salt_hex: String) {
        self.assert_executor();
        require!(decision_json.len() <= MAX_DECISION_JSON, "decision too long");
        require!(salt_hex.len() <= 128, "salt too long");
        let c = self
            .commitments
            .get(id as u32)
            .unwrap_or_else(|| env::panic_str("no such commitment: trade was never committed"))
            .clone();
        require!(c.status == CommitmentStatus::Committed, "commitment already settled");
        let salt = hex::decode(&salt_hex).unwrap_or_else(|_| env::panic_str("salt must be hex"));
        let mut preimage = decision_json.as_bytes().to_vec();
        preimage.extend_from_slice(&salt);
        if hex::encode(env::sha256(&preimage)) != c.commitment {
            env::panic_str("reveal does not match commitment: this trade was not committed when claimed");
        }
        let d: Decision = serde_json::from_str(&decision_json).unwrap_or_else(|_| env::panic_str("decision is not valid JSON"));
        let entry = self.commitments.get_mut(id as u32).unwrap();
        entry.revealed = Some(decision_json.clone());
        entry.salt_hex = Some(salt_hex);
        match self.check_guardrails(&d) {
            Err(reason) => {
                let entry = self.commitments.get_mut(id as u32).unwrap();
                entry.status = CommitmentStatus::Rejected;
                entry.reject_reason = Some(reason.clone());
                emit("guardrail_rejected", serde_json::json!({"id": id, "reason": reason, "decision": decision_json}));
            }
            Ok(()) => {
                self.commitments.get_mut(id as u32).unwrap().status = CommitmentStatus::Executed;
                emit("executed", serde_json::json!({"id": id, "decision": decision_json}));
            }
        }
    }

    /// The vault builds the EVM transaction for one leg of an executed
    /// decision and asks the MPC network to sign it. Recipient is locked to
    /// the vault's own address; targets are only WETH / USDC (approve) / the
    /// Uniswap router; gas is capped; each leg can be signed once per
    /// decision. The executor merely broadcasts the result.
    #[payable]
    pub fn sign_evm_tx(
        &mut self,
        id: u64,
        leg: String,
        nonce: u64,
        max_fee_per_gas: U128,
        max_priority_fee_per_gas: U128,
        gas_limit: u64,
        amount_in: U128,
        amount_out_min: U128,
        fee_tier: u32,
    ) -> Promise {
        self.assert_executor();
        require!(!self.emergency, "vault is in emergency exit mode");
        let c = self.commitments.get(id as u32).unwrap_or_else(|| env::panic_str("no such commitment")).clone();
        require!(c.status == CommitmentStatus::Executed, "decision not executed (or rejected by guardrails)");
        let d: Decision = serde_json::from_str(c.revealed.as_deref().unwrap_or("")).unwrap_or_else(|_| env::panic_str("no revealed decision"));
        require!(d.pair == "ETH/USDC", "only ETH/USDC is wired for execution");
        require!(d.action == "buy" || d.action == "sell", "nothing to execute for this decision");
        require!(max_fee_per_gas.0 <= MAX_FEE_PER_GAS, "max_fee_per_gas above cap");
        require!(max_priority_fee_per_gas.0 <= max_fee_per_gas.0, "priority fee above max fee");
        require!(gas_limit <= MAX_GAS_LIMIT, "gas_limit above cap");
        let key = format!("{id}:{leg}");
        require!(self.signed_legs.get(&key).is_none(), "this leg was already signed for this decision");
        let me = parse_evm_address(&self.evm_address);
        let (token_in, token_out) = if d.action == "sell" { (WETH, USDC) } else { (USDC, WETH) };
        let (to, value, data) = match leg.as_str() {
            "wrap" => {
                require!(d.action == "sell", "wrap is only needed to sell ETH");
                (WETH, amount_in.0, hex::decode("d0e30db0").unwrap())
            }
            "approve" => {
                let mut data = hex::decode("095ea7b3").unwrap();
                data.extend(evm::word_addr(&ROUTER));
                data.extend(evm::word_max());
                (token_in, 0, data)
            }
            "swap" => {
                require!(matches!(fee_tier, 500 | 3000 | 10000), "unknown fee tier");
                require!(amount_out_min.0 > 0, "amount_out_min must be set (slippage guard)");
                let mut data = hex::decode("04e45aaf").unwrap();
                data.extend(evm::word_addr(&token_in));
                data.extend(evm::word_addr(&token_out));
                data.extend(evm::word_u128(fee_tier as u128));
                data.extend(evm::word_addr(&me)); // recipient: the vault itself
                data.extend(evm::word_u128(amount_in.0));
                data.extend(evm::word_u128(amount_out_min.0));
                data.extend(evm::word_u128(0)); // sqrtPriceLimitX96
                data.extend(hex::decode(&c.commitment).unwrap()); // tie trade to commitment
                (ROUTER, 0, data)
            }
            _ => env::panic_str("leg must be wrap | approve | swap"),
        };
        let tx = evm::Tx1559 {
            chain_id: EVM_CHAIN_ID,
            nonce,
            max_priority_fee_per_gas: max_priority_fee_per_gas.0,
            max_fee_per_gas: max_fee_per_gas.0,
            gas_limit,
            to,
            value,
            data,
        };
        let payload = env::keccak256(&tx.signing_bytes());
        let payload_hex = hex::encode(&payload);
        self.signed_legs.insert(key.clone(), true);
        emit("evm_sign_requested", serde_json::json!({"id": id, "leg": leg, "nonce": nonce, "payload": payload_hex}));
        let args = serde_json::json!({"request": {
            "payload_v2": {"Ecdsa": payload_hex}, "path": self.mpc_path, "domain_id": 0,
        }});
        Promise::new(MPC_SIGNER.parse().unwrap())
            .function_call("sign".to_string(), args.to_string().into_bytes(), env::attached_deposit(), Gas::from_tgas(200))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_signed(key, payload_hex))
    }

    #[private]
    pub fn on_signed(&mut self, key: String, payload_hex: String) -> Option<serde_json::Value> {
        match env::promise_result(0) {
            PromiseResult::Successful(b) => {
                let sig: serde_json::Value = serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null);
                emit("evm_signed", serde_json::json!({"leg": key, "payload": payload_hex}));
                Some(sig)
            }
            _ => {
                self.signed_legs.remove(&key); // allow a retry
                emit("evm_sign_failed", serde_json::json!({"leg": key}));
                None
            }
        }
    }

    pub fn record_execution_ref(&mut self, id: u64, execution_ref: String) {
        self.assert_executor();
        require!(execution_ref.len() <= MAX_REF, "execution_ref too long");
        let entry = self.commitments.get_mut(id as u32).unwrap_or_else(|| env::panic_str("no such commitment"));
        require!(entry.status == CommitmentStatus::Executed, "commitment not executed");
        entry.execution_ref = Some(execution_ref);
    }

    /// Settle the cross-chain wallet's value (computed in the enclave from
    /// attested, published inputs). Bounded: NAV may move at most 10% per
    /// settlement, settlements are >= 1h apart. Withdrawals never read this
    /// number, so a misreport cannot extract NEAR; it can only skew deposit
    /// pricing within the bound, and it is publicly fraud-provable.
    pub fn settle_pnl(&mut self, ext_value: U128, evm_block: u64, attestation_ref: String) {
        self.assert_executor();
        require!(!self.emergency, "vault is in emergency exit mode");
        require!(attestation_ref.len() <= MAX_REF, "attestation_ref too long");
        let now = env::block_timestamp_ms();
        require!(
            self.last_settlement_ms == 0 || now >= self.last_settlement_ms + MIN_SETTLE_INTERVAL_MS,
            "settlements must be at least 1h apart"
        );
        let old_nav = self.nav();
        let old = self.ext_value;
        // units come into existence only with real value, at a fixed large
        // base (keeps the per-unit payout accumulator far from overflow)
        let first = self.ext_units_total == 0 && ext_value.0 > 0;
        if first {
            require!(self.total_shares > 0, "no shareholders to own external value");
        }
        // validate before writing anything
        let (units_total, vault_units) = if first {
            (EXT_UNITS_BASE, EXT_UNITS_BASE)
        } else if self.ext_units_total == 0 {
            (1, 1) // value stays 0: nothing to attribute yet
        } else {
            (self.ext_units_total, self.vault_ext_units)
        };
        let new_holders = mul_div(ext_value.0, vault_units, units_total);
        let new_nav = self.pool + new_holders;
        let max_step = mul_div(old_nav, MAX_SETTLE_STEP_BPS, 10_000);
        require!(
            new_nav.abs_diff(old_nav) <= max_step,
            "settlement moves NAV by more than 10%: split it across settlements"
        );
        if first {
            // first external value ever: units owned by shareholders. No shares
            // are minted (the v3 seed-capital mint was an exploit).
            self.ext_units_total = units_total;
            self.vault_ext_units = vault_units;
        }
        self.ext_value = ext_value.0;
        self.last_settlement_ms = now;
        self.last_settlement_ref = Some(format!("evm block {} · {}", evm_block, attestation_ref));
        self.record_nav();
        emit("settlement", serde_json::json!({
            "old_ext": old.to_string(), "new_ext": ext_value.0.to_string(),
            "evm_block": evm_block, "attestation_ref": attestation_ref,
        }));
    }

    pub fn post_risk_proof(&mut self, epoch: u64, bundle_json: String) {
        self.assert_executor();
        require!(bundle_json.len() <= MAX_BUNDLE_JSON, "bundle too large");
        let v: serde_json::Value = serde_json::from_str(&bundle_json).unwrap_or_else(|_| env::panic_str("bundle is not valid JSON"));
        let lev = v["leverage"]["limit"].as_u64().unwrap_or(0);
        let dd = v["drawdown"]["limit"].as_u64().unwrap_or(0);
        require!(lev == self.guardrails.max_leverage_bps && dd == self.guardrails.max_drawdown_bps, "bundle limits do not match vault guardrails");
        self.risk_proofs.push(StoredRiskProof {
            epoch, max_leverage_bps: lev, max_drawdown_bps: dd, bundle_json, posted_at_ms: env::block_timestamp_ms(),
        });
        emit("risk_proof", serde_json::json!({"epoch": epoch}));
    }

    /// On-chain request to OutLayer (attested by construction: only
    /// DCAP-verified worker keys may resolve it). Input is checked against the
    /// vault's state both now and again in the callback.
    #[payable]
    pub fn request_decision(&mut self, input_json: String) -> Promise {
        require!(!self.emergency, "vault is in emergency exit mode");
        let caller = env::predecessor_account_id();
        require!(caller == self.executor || caller == self.manager_account(), "executor or strategy-NFT holder");
        require!(input_json.len() <= MAX_INPUT_JSON, "input_json too long");
        if let Err(e) = self.check_input_state(&input_json) {
            env::panic_str(&e);
        }
        let args = serde_json::json!({
            "source": { "Project": {
                "project_id": format!("{}/alpha-vault-strategy", self.owner),
                "version_key": self.approved_wasm_hash,
            }},
            "resource_limits": {},
            "input_data": input_json,
            "secrets_ref": { "profile": "default", "account_id": self.owner },
        });
        Promise::new("outlayer.testnet".parse().unwrap())
            .function_call("request_execution".to_string(), args.to_string().into_bytes(), env::attached_deposit(), Gas::from_tgas(150))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(40)).on_decision(input_json))
    }

    #[private]
    pub fn on_decision(&mut self, input_json: String) -> Option<u64> {
        let bytes = match env::promise_result(0) {
            PromiseResult::Successful(b) => b,
            _ => {
                emit("decision_request_failed", serde_json::json!({}));
                return None;
            }
        };
        if let Err(e) = self.check_input_state(&input_json) {
            emit("decision_request_failed", serde_json::json!({"reason": e}));
            return None;
        }
        let text = String::from_utf8(bytes).unwrap_or_default();
        let parsed: Option<serde_json::Value> = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| {
            let v = if v.is_string() { serde_json::from_str(v.as_str().unwrap()).ok()? } else { v };
            if v.get("commitment").is_some() {
                return Some(v);
            }
            let inner = v.get("output").or_else(|| v.get("result"))?.clone();
            if inner.is_string() { serde_json::from_str(inner.as_str().unwrap()).ok() } else { Some(inner) }
        });
        let out = match parsed {
            Some(o) if o["commitment"].as_str().map_or(false, |c| c.len() == 64) => o,
            _ => {
                emit("decision_request_failed", serde_json::json!({"raw": text.chars().take(300).collect::<String>()}));
                return None;
            }
        };
        let commitment = out["commitment"].as_str().unwrap().to_string();
        let id = self.commitments.len() as u64;
        self.commitments.push(TradeCommitment {
            id,
            commitment: commitment.clone(),
            wasm_hash: self.approved_wasm_hash.clone(),
            attestation_ref: "onchain request_execution (version pinned to approved hash)".into(),
            committed_at_height: env::block_height(),
            committed_at_ms: env::block_timestamp_ms(),
            status: CommitmentStatus::Committed,
            revealed: None,
            salt_hex: None,
            reject_reason: None,
            execution_ref: None,
            sealed_enc: out["sealed_enc"].as_str().map(|x| x.chars().take(2_000).collect()),
            input_json: Some(input_json),
        });
        self.last_attested_ms = env::block_timestamp_ms();
        emit("commit", serde_json::json!({"id": id, "commitment": commitment, "path": "onchain"}));
        Some(id)
    }

    // ==================================================================
    // Exit guarantees
    // ==================================================================

    pub fn is_stale(&self) -> bool {
        env::block_timestamp_ms().saturating_sub(self.last_attested_ms) > self.liveness_timeout_ms
    }

    /// Anyone, once the vault has been silent past the liveness timeout.
    /// One-way. Freezes deposits, trading and settlement; withdrawals keep
    /// working (pool slice + external units), repatriation keeps working.
    pub fn trigger_emergency(&mut self) {
        require!(!self.emergency, "already in emergency mode");
        require!(self.is_stale(), "vault is live: last attested decision is within the timeout");
        self.emergency = true;
        self.deposits_paused = true;
        emit("emergency_triggered", serde_json::json!({
            "by": env::predecessor_account_id(), "last_attested_ms": self.last_attested_ms,
        }));
    }

    // ==================================================================
    // Timelocked rotations (strategy code, signing wallet)
    // ==================================================================

    pub fn propose_strategy(&mut self, wasm_hash: String, wasm_url: String) {
        self.assert_manager();
        require!(wasm_hash.len() == 64 && hex::decode(&wasm_hash).is_ok(), "wasm_hash must be 32 bytes hex");
        require!(wasm_url.len() <= MAX_REF, "url too long");
        let at = env::block_timestamp_ms() + ROTATION_DELAY_MS;
        emit("strategy_proposed", serde_json::json!({"hash": wasm_hash, "url": wasm_url, "effective_at_ms": at}));
        self.pending_strategy = Some((wasm_hash, wasm_url, at));
    }

    /// Anyone may apply once the 48h timelock has passed. This is the only
    /// way an integrity alert is cleared: deliberately, publicly, late.
    pub fn apply_strategy(&mut self) {
        let (h, u, at) = self.pending_strategy.clone().unwrap_or_else(|| env::panic_str("nothing pending"));
        require!(env::block_timestamp_ms() >= at, "timelock not elapsed");
        emit("strategy_rotated", serde_json::json!({"old": self.approved_wasm_hash, "new": h}));
        self.approved_wasm_hash = h;
        self.strategy_wasm_url = u;
        self.pending_strategy = None;
        self.integrity_alert = None;
        if !self.emergency {
            self.deposits_paused = false;
        }
    }

    pub fn propose_wallet(&mut self, mpc_path: String, evm_address: String) {
        self.assert_manager();
        parse_evm_address(&evm_address);
        require!(mpc_path.len() <= 64, "path too long");
        let at = env::block_timestamp_ms() + ROTATION_DELAY_MS;
        emit("wallet_proposed", serde_json::json!({"path": mpc_path, "evm_address": evm_address, "effective_at_ms": at}));
        self.pending_wallet = Some((mpc_path, evm_address, at));
    }

    pub fn apply_wallet(&mut self) {
        let (p, a, at) = self.pending_wallet.clone().unwrap_or_else(|| env::panic_str("nothing pending"));
        require!(env::block_timestamp_ms() >= at, "timelock not elapsed");
        emit("wallet_rotated", serde_json::json!({"old": self.evm_address, "new": a, "path": p}));
        self.mpc_path = p;
        self.evm_address = a;
        self.pending_wallet = None;
    }

    // ==================================================================
    // Fees
    // ==================================================================

    /// Mint the manager's fee as shares for gain above the high-water mark.
    /// Only on a valuation that has stood for 24h (a pumped settlement can't
    /// be cashed into fees before anyone can see it), and never in emergency.
    pub fn crystallize_fees(&mut self) -> U128 {
        if self.emergency || self.total_shares == 0 || self.perf_fee_bps == 0 {
            return U128(0);
        }
        if self.ext_value > 0 && env::block_timestamp_ms() < self.last_settlement_ms + FEE_SETTLEMENT_AGE_MS {
            return U128(0);
        }
        let nav = self.nav();
        if nav == 0 {
            return U128(0);
        }
        let nps = mul_div(nav, NPS_SCALE, self.total_shares);
        if nps <= self.hwm_nav_per_share {
            return U128(0);
        }
        let gain_per_share = nps - self.hwm_nav_per_share;
        let fee_value = mul_div(mul_div(gain_per_share, self.total_shares, NPS_SCALE), u128::from(self.perf_fee_bps), 10_000);
        if fee_value == 0 {
            self.hwm_nav_per_share = nps;
            return U128(0);
        }
        let f = mul_div(fee_value, NPS_SCALE, nav);
        let fee_shares = mul_div(self.total_shares, f, NPS_SCALE - f);
        if fee_shares > 0 {
            let m = self.manager_account();
            let cur = self.shares.get(&m).copied().unwrap_or(0);
            self.shares.insert(m.clone(), cur + fee_shares);
            self.total_shares += fee_shares;
            emit("fees_crystallized", serde_json::json!({"manager": m, "fee_shares": fee_shares.to_string(), "fee_value_yocto": fee_value.to_string()}));
        }
        self.hwm_nav_per_share = mul_div(self.nav(), NPS_SCALE, self.total_shares);
        U128(fee_shares)
    }

    // ==================================================================
    // Views
    // ==================================================================

    /// Shareholders' part of the cross-chain wallet.
    pub fn holders_ext_value(&self) -> u128 {
        if self.ext_units_total == 0 { 0 } else { mul_div(self.ext_value, self.vault_ext_units, self.ext_units_total) }
    }

    /// NAV attributable to shareholders: NEAR pool + their external units.
    pub fn nav(&self) -> u128 {
        self.pool + self.holders_ext_value()
    }

    pub fn get_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "version": EVENT_VERSION,
            "owner": self.owner,
            "executor": self.executor,
            "manager": self.manager_account(),
            "approved_wasm_hash": self.approved_wasm_hash,
            "strategy_wasm_url": self.strategy_wasm_url,
            "guardrails": self.guardrails,
            "total_shares": self.total_shares.to_string(),
            "pool_yocto": self.pool.to_string(),
            "ext_value_yocto": self.ext_value.to_string(),
            "external_value_yocto": self.holders_ext_value().to_string(),
            "nav_yocto": self.nav().to_string(),
            "ext_units_total": self.ext_units_total.to_string(),
            "vault_ext_units": self.vault_ext_units.to_string(),
            "repatriated_total_yocto": self.repatriated_total.to_string(),
            "ext_paid_total_yocto": self.ext_paid_total.to_string(),
            "last_settlement_ref": self.last_settlement_ref,
            "last_settlement_ms": self.last_settlement_ms,
            "hwm_nav_per_share": self.hwm_nav_per_share.to_string(),
            "last_attested_ms": self.last_attested_ms,
            "liveness_timeout_ms": self.liveness_timeout_ms,
            "stale": self.is_stale(),
            "emergency": self.emergency,
            "mpc_path": self.mpc_path,
            "evm_address": self.evm_address,
            "pending_wallet": self.pending_wallet,
            "pending_strategy": self.pending_strategy,
            "commitments": self.commitments.len(),
            "risk_proofs": self.risk_proofs.len(),
            "integrity_alert": self.integrity_alert,
            "deposits_paused": self.deposits_paused,
            "min_deposit_yocto": MIN_DEPOSIT.to_string(),
        })
    }

    pub fn get_shares(&self, account: AccountId) -> U128 {
        U128(self.shares.get(&account).copied().unwrap_or(0))
    }
    pub fn get_commitment(&self, id: u64) -> Option<TradeCommitment> {
        self.commitments.get(id as u32).cloned()
    }
    pub fn get_commitments(&self, from: u64, limit: u64) -> Vec<TradeCommitment> {
        (from..(from + limit).min(self.commitments.len() as u64)).filter_map(|i| self.commitments.get(i as u32).cloned()).collect()
    }
    pub fn get_risk_proofs(&self, from: u64, limit: u64) -> Vec<StoredRiskProof> {
        (from..(from + limit).min(self.risk_proofs.len() as u64)).filter_map(|i| self.risk_proofs.get(i as u32).cloned()).collect()
    }
    /// Chronological NAV history (ring buffer of the last NAV_LOG_CAP points).
    pub fn get_nav_history(&self, from: u64, limit: u64) -> Vec<serde_json::Value> {
        let len = self.nav_log.len();
        let start = if len < NAV_LOG_CAP { 0 } else { self.nav_log_head };
        (from..(from + limit).min(len as u64))
            .filter_map(|i| self.nav_log.get((start + i as u32) % len.max(1)))
            .map(|p| serde_json::json!({"t": p.ts_ms, "pool_yocto": p.pool_yocto.to_string()}))
            .collect()
    }
    pub fn get_manager(&self) -> AccountId {
        self.manager_account()
    }

    // ------------------------------------------------------------------

    fn manager_account(&self) -> AccountId {
        self.nft.owner_by_id.get(&STRATEGY_TOKEN_ID.to_string()).unwrap_or_else(|| self.owner.clone())
    }
    fn assert_manager(&self) {
        require!(env::predecessor_account_id() == self.manager_account(), "strategy-NFT holder only");
    }
    fn assert_executor(&self) {
        require!(env::predecessor_account_id() == self.executor, "only the registered executor may relay attested decisions");
    }

    fn record_nav(&mut self) {
        let p = NavPoint { ts_ms: env::block_timestamp_ms(), pool_yocto: self.nav() };
        if self.nav_log.len() < NAV_LOG_CAP {
            self.nav_log.push(p);
        } else {
            self.nav_log.set(self.nav_log_head, p);
            self.nav_log_head = (self.nav_log_head + 1) % NAV_LOG_CAP;
        }
    }

    fn check_input_state(&self, input_json: &str) -> Result<(), String> {
        let v: serde_json::Value = serde_json::from_str(input_json).map_err(|_| "input_json is not JSON".to_string())?;
        let Some(vs) = v.get("vault_state") else {
            return Err("input_json has no vault_state: risk would not be bound to real positions".into());
        };
        let want = [
            ("pool_yocto", self.pool.to_string()),
            ("total_shares", self.total_shares.to_string()),
            ("hwm_nps", self.hwm_nav_per_share.to_string()),
            ("external_value_yocto", self.holders_ext_value().to_string()),
        ];
        for (k, w) in want {
            let got = vs.get(k).and_then(|x| x.as_str()).unwrap_or("");
            if got != w {
                return Err(format!("enclave input vault_state.{k}={got} but the vault has {w}"));
            }
        }
        if vs["max_leverage_bps"].as_u64() != Some(self.guardrails.max_leverage_bps)
            || vs["max_drawdown_bps"].as_u64() != Some(self.guardrails.max_drawdown_bps)
        {
            return Err("enclave input guardrails differ from the vault's".into());
        }
        Ok(())
    }

    fn check_guardrails(&self, d: &Decision) -> Result<(), String> {
        if !matches!(d.action.as_str(), "buy" | "sell" | "hold") {
            return Err(format!("unknown action '{}'", d.action));
        }
        if d.action == "hold" {
            return Ok(());
        }
        if d.leverage_bps > self.guardrails.max_leverage_bps {
            return Err(format!("leverage {}bps exceeds max {}bps", d.leverage_bps, self.guardrails.max_leverage_bps));
        }
        if d.size_bps > self.guardrails.max_position_bps {
            return Err(format!("position {}bps of NAV exceeds max {}bps", d.size_bps, self.guardrails.max_position_bps));
        }
        if !self.guardrails.allowed_pairs.iter().any(|p| p == &d.pair) {
            return Err(format!("pair '{}' not in allowlist", d.pair));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Migration v3 -> v4. Deployed atomically with the code.
// ---------------------------------------------------------------------------
#[near(serializers = [borsh])]
pub struct VaultV3 {
    owner: AccountId,
    executor: AccountId,
    approved_wasm_hash: String,
    guardrails: Guardrails,
    total_shares: u128,
    shares: LookupMap<AccountId, u128>,
    pool: u128,
    external_value: u128,
    last_settlement_ref: Option<String>,
    strategy_wasm_url: String,
    commitments: Vector<TradeCommitment>,
    risk_proofs: Vector<StoredRiskProof>,
    nav_log: Vector<NavPoint>,
    perf_fee_bps: u64,
    hwm_nav_per_share: u128,
    integrity_alert: Option<String>,
    deposits_paused: bool,
    nft: NonFungibleToken,
    nft_metadata: LazyOption<NFTContractMetadata>,
    last_attested_ms: u64,
    liveness_timeout_ms: u64,
    emergency: bool,
    external_claims: LookupMap<AccountId, u128>,
    claim_paid: LookupMap<AccountId, u128>,
    total_claims: u128,
    recovered_total: u128,
    mpc_path: String,
    pending_mpc_path: Option<(String, u64)>,
}

#[near]
impl Vault {
    #[private]
    #[init(ignore_state)]
    pub fn migrate_v4(mpc_path: String, evm_address: String) -> Self {
        let o: VaultV3 = env::state_read().expect("no v3 state to migrate");
        require!(o.total_claims == 0, "v3 claims outstanding: migrate them first");
        parse_evm_address(&evm_address);
        let units = if o.external_value > 0 { EXT_UNITS_BASE } else { 0 };
        emit("migrated", serde_json::json!({"to": "v4", "ext_units": units.to_string()}));
        Self {
            owner: o.owner,
            executor: o.executor,
            approved_wasm_hash: o.approved_wasm_hash,
            guardrails: o.guardrails,
            total_shares: o.total_shares,
            shares: o.shares,
            pool: o.pool,
            ext_value: o.external_value,
            last_settlement_ref: o.last_settlement_ref,
            last_settlement_ms: 0,
            strategy_wasm_url: o.strategy_wasm_url,
            commitments: o.commitments,
            risk_proofs: o.risk_proofs,
            nav_log: o.nav_log,
            nav_log_head: 0,
            perf_fee_bps: o.perf_fee_bps,
            hwm_nav_per_share: o.hwm_nav_per_share,
            integrity_alert: o.integrity_alert,
            deposits_paused: o.deposits_paused,
            nft: o.nft,
            nft_metadata: o.nft_metadata,
            last_attested_ms: o.last_attested_ms,
            liveness_timeout_ms: o.liveness_timeout_ms,
            emergency: o.emergency,
            ext_units_total: units,
            vault_ext_units: units,
            ext_units: LookupMap::new(StorageKey::ExtUnits),
            ext_debt: LookupMap::new(StorageKey::ExtDebt),
            ext_pending: LookupMap::new(StorageKey::ExtPending),
            ext_acc: 0,
            repatriated_total: 0,
            ext_paid_total: 0,
            mpc_path,
            evm_address,
            pending_wallet: None,
            pending_strategy: None,
            signed_legs: LookupMap::new(StorageKey::SignedLegs),
            failed_payouts: LookupMap::new(StorageKey::FailedPayouts),
        }
    }
}

near_contract_standards::impl_non_fungible_token_core!(Vault, nft);
near_contract_standards::impl_non_fungible_token_approval!(Vault, nft);
near_contract_standards::impl_non_fungible_token_enumeration!(Vault, nft);

#[near]
impl NonFungibleTokenMetadataProvider for Vault {
    fn nft_metadata(&self) -> NFTContractMetadata {
        self.nft_metadata.get().unwrap()
    }
}

#[cfg(test)]
mod tests;
