//! # Verifiable Alpha Vault (v5)
//!
//! v5 (after a second independent review) makes the trust model explicit with
//! two modes:
//!
//! * **Relayed** (default, live today): a named executor relays enclave output.
//!   It is openly trusted for *liveness and valuation reports*, and every
//!   mechanical lever it used to have is removed: trade sizes and slippage
//!   floors come from the sealed strategy's committed decision, deposits only
//!   open while the vault is fully in NEAR (so a valuation can never misprice
//!   a deposit), fees only crystallize on realized (repatriated) gains, the
//!   cross-chain wallet can never be rotated, and the executor pays for every
//!   byte it writes.
//! * **Attested**: only output delivered by OutLayer's on-chain
//!   `request_execution` (resolved exclusively by DCAP-verified TDX workers)
//!   can commit, execute, settle or prove risk. Switching on is immediate;
//!   switching back is timelocked 48h.
//!
//! Code upgrades go through the same 48h timelock (`propose_code` /
//! `apply_code`) once the account's full-access key is deleted.
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
const EVENT_VERSION: &str = "5.0.0";
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
/// NEAR an attested `request_decision` keeps back to pay for the storage its
/// callback writes (the rest goes to OutLayer).
pub const ATTESTED_STORAGE_RESERVE: u128 = ONE_NEAR / 10;
/// A swap's deadline must fall within this window from now.
pub const MAX_DEADLINE_S: u64 = 3_600;
/// An exit (cross-chain value back to NEAR via NEAR Intents) is public this
/// long before the vault will sign it.
pub const EXIT_DELAY_MS: u64 = 24 * 3_600_000;
const GAS_CAP_EXIT: u64 = 120_000;
const EXIT_KEY: &[u8] = b"exit";
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
const MAX_FEE_PER_GAS: u128 = 100_000_000_000; // 100 gwei
/// Per-leg gas caps (gas is paid from the vault's own ETH).
const GAS_CAP_WRAP: u64 = 250_000;
const GAS_CAP_APPROVE: u64 = 250_000; // Sepolia WETH: wrap alone burns ~133k
const GAS_CAP_SWAP: u64 = 400_000;
const MAX_GAS_LIMIT: u64 = GAS_CAP_SWAP;

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
    fn hexb(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
    /// SwapRouter02 `multicall(uint256 deadline, bytes[] data)` wrapping one
    /// `exactInputSingle`, with the 32-byte decision commitment appended to
    /// the inner call (ignored by the router, ties the trade to the commit).
    #[allow(clippy::too_many_arguments)]
    pub fn swap_calldata(
        token_in: &[u8; 20], token_out: &[u8; 20], fee: u32, recipient: &[u8; 20],
        amount_in: u128, min_out: u128, deadline_s: u64, commitment_hex: &str,
    ) -> Vec<u8> {
        let mut inner = hexb("04e45aaf");
        inner.extend(word_addr(token_in));
        inner.extend(word_addr(token_out));
        inner.extend(word_u128(fee as u128));
        inner.extend(word_addr(recipient));
        inner.extend(word_u128(amount_in));
        inner.extend(word_u128(min_out));
        inner.extend(word_u128(0)); // sqrtPriceLimitX96
        inner.extend(hexb(commitment_hex));
        let mut out = hexb("5ae401dc");
        out.extend(word_u128(deadline_s as u128));
        out.extend(word_u128(0x40)); // offset of bytes[]
        out.extend(word_u128(1)); // array length
        out.extend(word_u128(0x20)); // offset of element 0
        out.extend(word_u128(inner.len() as u128));
        let pad = (32 - inner.len() % 32) % 32;
        out.extend(inner);
        out.extend(vec![0u8; pad]);
        out
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
    AttestedIds,
    ExpectedBundles,
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
    /// Trade size decided inside the enclave (sell: wei of ETH, buy: USDC
    /// 6dp), the slippage floor in the output token, and the pool fee tier.
    /// The vault signs exactly these; nobody else chooses them.
    #[serde(default)]
    pub amount_in: String,
    #[serde(default)]
    pub min_out: String,
    #[serde(default)]
    pub fee_tier: u32,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A named executor relays enclave output (openly trusted for liveness
    /// and valuation reports; mechanically constrained everywhere else).
    Relayed,
    /// Only OutLayer's on-chain, TDX-attested delivery can commit, execute,
    /// settle or prove.
    Attested,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Proposal {
    /// strategy: wasm sha256; code: contract sha256; mode: "relayed"
    pub hash: String,
    pub url: String,
    pub effective_at_ms: u64,
    /// Bound to whoever proposed it: if the strategy NFT changes hands, a
    /// pending proposal can no longer be applied.
    pub proposer: AccountId,
}

/// One pending exit: send `amount` USDC (6dp) from the vault's EVM wallet to
/// a NEAR Intents (1Click) deposit address whose quote pays NEAR to this
/// vault. The contract can't see inside the quote, so the order is public for
/// 24h first: anyone can check the address with 1Click's status API (its
/// recipient must be this vault) and leave before it is signed.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct ExitOrder {
    pub deposit_address: String,
    pub amount_usdc: U128,
    pub quote_ref: String,
    pub effective_at_ms: u64,
    pub proposer: AccountId,
    pub signed: bool,
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
    pub pending_strategy: Option<Proposal>,
    signed_legs: LookupMap<String, bool>,
    failed_payouts: LookupMap<AccountId, u128>,
    // ---- v5 ----
    pub mode: Mode,
    pub pending_relayed: Option<Proposal>,
    pub pending_code: Option<Proposal>,
    /// Commitments delivered by the attested on-chain path.
    attested_ids: LookupMap<u64, bool>,
    /// sha256 of risk bundles the enclave produced (attested) -> epoch.
    expected_bundles: LookupMap<String, u64>,
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
                    "Sealed-parameter strategy attested as wasm {}. Holder earns performance fees on realized gains and controls the (timelocked) strategy, mode and code changes.",
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
            pending_strategy: None,
            signed_legs: LookupMap::new(StorageKey::SignedLegs),
            failed_payouts: LookupMap::new(StorageKey::FailedPayouts),
            mode: Mode::Relayed,
            pending_relayed: None,
            pending_code: None,
            attested_ids: LookupMap::new(StorageKey::AttestedIds),
            expected_bundles: LookupMap::new(StorageKey::ExpectedBundles),
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
        // A cross-chain valuation can never price a deposit: new money only
        // comes in while every yocto of NAV is NEAR held by this contract.
        require!(
            self.ext_value == 0,
            "deposits open only while the vault is fully in NEAR: cross-chain value must be repatriated first"
        );
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
        if let Err(e) = self.credit_repatriation(r) {
            env::panic_str(&e);
        }
    }

    fn credit_repatriation(&mut self, r: u128) -> Result<(), String> {
        if self.ext_units_total == 0 { return Err("no external units outstanding".into()); }
        if self.ext_units_total < EXT_UNITS_BASE / 1_000 { return Err("external units too few to distribute safely".into()); }
        let to_vault = mul_div(r, self.vault_ext_units, self.ext_units_total);
        self.pool += to_vault;
        self.ext_acc += mul_div(r, ACC_SCALE, self.ext_units_total);
        self.ext_value = self.ext_value.saturating_sub(r);
        self.repatriated_total += r;
        self.record_nav();
        emit("repatriated", serde_json::json!({"amount": r.to_string(), "to_pool": to_vault.to_string()}));
        Ok(())
    }

    // ==================================================================
    // The way back: cross-chain value -> NEAR via NEAR Intents
    // ==================================================================

    fn wrap_account() -> AccountId {
        let me = env::current_account_id();
        (if me.as_str().ends_with(".testnet") { "wrap.testnet" } else { "wrap.near" }).parse().unwrap()
    }

    pub fn get_exit(&self) -> Option<ExitOrder> {
        env::storage_read(EXIT_KEY).and_then(|b| near_sdk::borsh::from_slice(&b).ok())
    }

    fn put_exit(&mut self, e: Option<&ExitOrder>) {
        match e {
            Some(e) => { env::storage_write(EXIT_KEY, &near_sdk::borsh::to_vec(e).unwrap()); }
            None => { env::storage_remove(EXIT_KEY); }
        }
    }

    /// Manager: name a 1Click deposit address (from a NEAR Intents quote whose
    /// recipient is this vault) and the USDC to send. Signable after 24h.
    #[payable]
    pub fn propose_exit(&mut self, deposit_address: String, amount_usdc: U128, quote_ref: String) {
        self.assert_manager();
        let before = env::storage_usage();
        require!(!self.emergency, "vault is in emergency exit mode");
        let _ = parse_evm_address(&deposit_address);
        require!(amount_usdc.0 > 0, "nothing to send");
        require!(quote_ref.len() <= MAX_REF, "quote_ref too long");
        let e = ExitOrder {
            deposit_address, amount_usdc, quote_ref,
            effective_at_ms: env::block_timestamp_ms() + EXIT_DELAY_MS,
            proposer: env::predecessor_account_id(), signed: false,
        };
        emit("exit_proposed", serde_json::json!({"deposit_address": e.deposit_address, "amount_usdc": e.amount_usdc, "quote_ref": e.quote_ref, "effective_at_ms": e.effective_at_ms}));
        self.put_exit(Some(&e));
        self.charge_storage(before);
    }

    pub fn cancel_exit(&mut self) {
        self.assert_manager();
        self.put_exit(None);
        emit("exit_cancelled", serde_json::json!({}));
    }

    /// Executor: after the delay, the vault signs exactly one USDC transfer to
    /// the proposed deposit address.
    #[payable]
    pub fn sign_exit_tx(&mut self, nonce: u64, max_fee_per_gas: U128, max_priority_fee_per_gas: U128, gas_limit: u64) -> Promise {
        self.assert_executor();
        let mut e = self.get_exit().unwrap_or_else(|| env::panic_str("no exit proposed"));
        require!(!e.signed, "this exit was already signed");
        require!(env::block_timestamp_ms() >= e.effective_at_ms, "exit timelock not elapsed");
        require!(self.manager_account() == e.proposer, "proposer no longer holds the strategy NFT");
        require!(max_fee_per_gas.0 <= MAX_FEE_PER_GAS, "max_fee_per_gas above cap");
        require!(max_priority_fee_per_gas.0 <= max_fee_per_gas.0, "priority fee above max fee");
        require!(gas_limit <= GAS_CAP_EXIT, "gas_limit above the exit cap");
        let before = env::storage_usage();
        let mut data = hex::decode("a9059cbb").unwrap(); // transfer(address,uint256)
        data.extend(evm::word_addr(&parse_evm_address(&e.deposit_address)));
        data.extend(evm::word_u128(e.amount_usdc.0));
        let tx = evm::Tx1559 {
            chain_id: EVM_CHAIN_ID, nonce,
            max_priority_fee_per_gas: max_priority_fee_per_gas.0, max_fee_per_gas: max_fee_per_gas.0,
            gas_limit, to: USDC, value: 0, data,
        };
        let payload_hex = hex::encode(env::keccak256(&tx.signing_bytes()));
        e.signed = true;
        self.put_exit(Some(&e));
        let key = format!("exit:{}", e.deposit_address);
        emit("evm_sign_requested", serde_json::json!({"leg": "exit", "nonce": nonce, "payload": payload_hex}));
        let storage = self.storage_cost(before);
        let att = env::attached_deposit().as_yoctonear();
        require!(att > storage, "attach enough for storage plus the MPC signing fee");
        let args = serde_json::json!({"request": {"payload_v2": {"Ecdsa": payload_hex}, "path": self.mpc_path, "domain_id": 0}});
        Promise::new(MPC_SIGNER.parse().unwrap())
            .function_call("sign".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(att - storage), Gas::from_tgas(200))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_signed(key, payload_hex))
    }

    /// Anyone: NEAR Intents delivers wNEAR to the vault. Unwrap everything the
    /// vault holds and credit it to holders exactly like `repatriate`.
    pub fn unwrap_and_repatriate(&mut self) -> Promise {
        Promise::new(Self::wrap_account())
            .function_call("ft_balance_of".to_string(), serde_json::json!({"account_id": env::current_account_id()}).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(5))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(40)).on_wrap_balance())
    }

    #[private]
    pub fn on_wrap_balance(&mut self, #[callback_result] bal: Result<U128, near_sdk::PromiseError>) -> PromiseOrValue<U128> {
        let bal = bal.map(|b| b.0).unwrap_or(0);
        if bal == 0 {
            emit("unwrap_skipped", serde_json::json!({"reason": "no wNEAR held"}));
            return PromiseOrValue::Value(U128(0));
        }
        PromiseOrValue::Promise(
            Promise::new(Self::wrap_account())
                .function_call("near_withdraw".to_string(), serde_json::json!({"amount": bal.to_string()}).to_string().into_bytes(), NearToken::from_yoctonear(1), Gas::from_tgas(10))
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_unwrapped(U128(bal))),
        )
    }

    #[private]
    pub fn on_unwrapped(&mut self, amount: U128) -> U128 {
        if !matches!(env::promise_result(0), PromiseResult::Successful(_)) {
            emit("unwrap_failed", serde_json::json!({"amount": amount}));
            return U128(0);
        }
        if let Err(e) = self.credit_repatriation(amount.0) {
            // NEAR is held but can't be attributed yet; stays in the contract
            emit("unwrap_uncredited", serde_json::json!({"amount": amount, "reason": e}));
            return U128(0);
        }
        amount
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

    #[payable]
    pub fn commit_decision(
        &mut self,
        commitment: String,
        wasm_hash: String,
        attestation_ref: String,
        input_json: String,
    ) -> u64 {
        self.assert_executor();
        self.assert_relayed();
        let before = env::storage_usage();
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
        self.charge_storage(before);
        id
    }

    /// Reveal a committed decision. The preimage must hash to the stored
    /// commitment, so whoever submits it cannot change it. In attested mode it
    /// only applies to commitments the enclave delivered on-chain.
    #[payable]
    pub fn execute_decision(&mut self, id: u64, decision_json: String, salt_hex: String) {
        self.assert_executor();
        if self.mode == Mode::Attested {
            require!(self.attested_ids.get(&id).is_some(), "attested mode: only enclave-delivered commitments can be revealed");
        }
        let before = env::storage_usage();
        if let Err(e) = self.reveal(id, decision_json, salt_hex) {
            env::panic_str(&e);
        }
        self.charge_storage(before);
    }

    fn reveal(&mut self, id: u64, decision_json: String, salt_hex: String) -> Result<(), String> {
        if decision_json.len() > MAX_DECISION_JSON { return Err("decision too long".into()); }
        if salt_hex.len() > 128 { return Err("salt too long".into()); }
        let c = self.commitments.get(id as u32).ok_or("no such commitment: trade was never committed")?.clone();
        if c.status != CommitmentStatus::Committed { return Err("commitment already settled".into()); }
        let salt = hex::decode(&salt_hex).map_err(|_| "salt must be hex")?;
        let mut preimage = decision_json.as_bytes().to_vec();
        preimage.extend_from_slice(&salt);
        if hex::encode(env::sha256(&preimage)) != c.commitment {
            return Err("reveal does not match commitment: this trade was not committed when claimed".into());
        }
        let d: Decision = serde_json::from_str(&decision_json).map_err(|_| "decision is not valid JSON")?;
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
        Ok(())
    }

    /// The vault builds the EVM transaction for one leg of an executed
    /// decision and asks the MPC network to sign it. Amount, slippage floor
    /// and fee tier come from the committed decision itself (decided in the
    /// enclave); the recipient is the vault's own address; targets are only
    /// WETH / USDC (approve) / the Uniswap router; gas is capped per leg; the
    /// swap carries a deadline; each leg is signed at most once per decision.
    /// The caller only picks the EVM nonce and gas price (capped) and
    /// broadcasts the result.
    #[payable]
    pub fn sign_evm_tx(
        &mut self,
        id: u64,
        leg: String,
        nonce: u64,
        max_fee_per_gas: U128,
        max_priority_fee_per_gas: U128,
        gas_limit: u64,
        deadline_s: u64,
    ) -> Promise {
        self.assert_executor();
        require!(!self.emergency, "vault is in emergency exit mode");
        if self.mode == Mode::Attested {
            require!(self.attested_ids.get(&id).is_some(), "attested mode: only enclave-delivered decisions can trade");
        }
        let before = env::storage_usage();
        let c = self.commitments.get(id as u32).unwrap_or_else(|| env::panic_str("no such commitment")).clone();
        require!(c.status == CommitmentStatus::Executed, "decision not executed (or rejected by guardrails)");
        let d: Decision = serde_json::from_str(c.revealed.as_deref().unwrap_or("")).unwrap_or_else(|_| env::panic_str("no revealed decision"));
        require!(d.pair == "ETH/USDC", "only ETH/USDC is wired for execution");
        require!(d.action == "buy" || d.action == "sell", "nothing to execute for this decision");
        let amount_in: u128 = d.amount_in.parse().unwrap_or(0);
        let min_out: u128 = d.min_out.parse().unwrap_or(0);
        require!(amount_in > 0, "decision carries no trade size (pre-v2 strategy): nothing the vault can sign");
        require!(min_out > 0, "decision carries no slippage floor");
        require!(matches!(d.fee_tier, 500 | 3000 | 10000), "decision has an unknown fee tier");
        require!(max_fee_per_gas.0 <= MAX_FEE_PER_GAS, "max_fee_per_gas above cap");
        require!(max_priority_fee_per_gas.0 <= max_fee_per_gas.0, "priority fee above max fee");
        let key = format!("{id}:{leg}");
        require!(self.signed_legs.get(&key).is_none(), "this leg was already signed for this decision");
        let me = parse_evm_address(&self.evm_address);
        let (token_in, token_out) = if d.action == "sell" { (WETH, USDC) } else { (USDC, WETH) };
        let (to, value, data, cap) = match leg.as_str() {
            "wrap" => {
                require!(d.action == "sell", "wrap is only needed to sell ETH");
                (WETH, amount_in, hex::decode("d0e30db0").unwrap(), GAS_CAP_WRAP)
            }
            "approve" => {
                let mut data = hex::decode("095ea7b3").unwrap();
                data.extend(evm::word_addr(&ROUTER));
                data.extend(evm::word_max());
                (token_in, 0, data, GAS_CAP_APPROVE)
            }
            "swap" => {
                let now_s = env::block_timestamp_ms() / 1_000;
                require!(deadline_s > now_s && deadline_s <= now_s + MAX_DEADLINE_S, "deadline must be within the next hour");
                (ROUTER, 0, evm::swap_calldata(&token_in, &token_out, d.fee_tier, &me, amount_in, min_out, deadline_s, &c.commitment), GAS_CAP_SWAP)
            }
            _ => env::panic_str("leg must be wrap | approve | swap"),
        };
        require!(gas_limit <= cap, "gas_limit above this leg's cap");
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
        // storage for the leg record comes out of the attached deposit; the
        // rest pays the MPC signer
        let storage = self.storage_cost(before);
        let att = env::attached_deposit().as_yoctonear();
        require!(att > storage, "attach enough for storage plus the MPC signing fee");
        let args = serde_json::json!({"request": {
            "payload_v2": {"Ecdsa": payload_hex}, "path": self.mpc_path, "domain_id": 0,
        }});
        Promise::new(MPC_SIGNER.parse().unwrap())
            .function_call("sign".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(att - storage), Gas::from_tgas(200))
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

    #[payable]
    pub fn record_execution_ref(&mut self, id: u64, execution_ref: String) {
        self.assert_executor();
        let before = env::storage_usage();
        require!(execution_ref.len() <= MAX_REF, "execution_ref too long");
        let entry = self.commitments.get_mut(id as u32).unwrap_or_else(|| env::panic_str("no such commitment"));
        require!(entry.status == CommitmentStatus::Executed, "commitment not executed");
        require!(entry.execution_ref.is_none(), "execution already recorded");
        entry.execution_ref = Some(execution_ref);
        self.charge_storage(before);
    }

    /// Settle the cross-chain wallet's value (relayed mode: the executor's
    /// report of an enclave valuation). Bounded: NAV moves at most 10% per
    /// settlement, settlements are >= 1h apart. No withdrawal, deposit or fee
    /// reads this number (deposits need it at 0, fees need it at 0), so a
    /// misreport cannot move NEAR; it only shows on the NAV chart, publicly
    /// checkable against the EVM block it names.
    #[payable]
    pub fn settle_pnl(&mut self, ext_value: U128, evm_block: u64, attestation_ref: String) {
        self.assert_executor();
        self.assert_relayed();
        let before = env::storage_usage();
        if let Err(e) = self.apply_settlement(ext_value.0, evm_block, attestation_ref) {
            env::panic_str(&e);
        }
        self.charge_storage(before);
    }

    fn apply_settlement(&mut self, ext_value: u128, evm_block: u64, attestation_ref: String) -> Result<(), String> {
        if self.emergency { return Err("vault is in emergency exit mode".into()); }
        if attestation_ref.len() > MAX_REF { return Err("attestation_ref too long".into()); }
        let now = env::block_timestamp_ms();
        if self.last_settlement_ms != 0 && now < self.last_settlement_ms + MIN_SETTLE_INTERVAL_MS {
            return Err("settlements must be at least 1h apart".into());
        }
        let old_nav = self.nav();
        let old = self.ext_value;
        // units come into existence only with real value, at a fixed large
        // base (keeps the per-unit payout accumulator far from overflow)
        let first = self.ext_units_total == 0 && ext_value > 0;
        if first && self.total_shares == 0 {
            return Err("no shareholders to own external value".into());
        }
        let (units_total, vault_units) = if first {
            (EXT_UNITS_BASE, EXT_UNITS_BASE)
        } else if self.ext_units_total == 0 {
            (1, 1) // value stays 0: nothing to attribute yet
        } else {
            (self.ext_units_total, self.vault_ext_units)
        };
        let new_holders = mul_div(ext_value, vault_units, units_total);
        let new_nav = self.pool + new_holders;
        let max_step = mul_div(old_nav, MAX_SETTLE_STEP_BPS, 10_000);
        if new_nav.abs_diff(old_nav) > max_step {
            return Err("settlement moves NAV by more than 10%: split it across settlements".into());
        }
        if first {
            self.ext_units_total = units_total;
            self.vault_ext_units = vault_units;
        }
        self.ext_value = ext_value;
        self.last_settlement_ms = now;
        self.last_settlement_ref = Some(format!("evm block {} · {}", evm_block, attestation_ref));
        self.record_nav();
        emit("settlement", serde_json::json!({
            "old_ext": old.to_string(), "new_ext": ext_value.to_string(),
            "evm_block": evm_block, "attestation_ref": attestation_ref,
        }));
        Ok(())
    }

    /// Relayed: the executor posts the enclave's bundle. Attested: anyone may
    /// post it, but only a bundle whose sha256 the enclave itself delivered
    /// on-chain for that epoch. Proofs are verified client-side either way.
    #[payable]
    pub fn post_risk_proof(&mut self, epoch: u64, bundle_json: String) {
        if self.mode == Mode::Attested {
            let h = hex::encode(env::sha256(bundle_json.as_bytes()));
            require!(self.expected_bundles.get(&h) == Some(&epoch), "attested mode: bundle hash was not delivered by the enclave for this epoch");
            self.expected_bundles.remove(&h);
        } else {
            self.assert_executor();
        }
        let before = env::storage_usage();
        require!(bundle_json.len() <= MAX_BUNDLE_JSON, "bundle too large");
        let v: serde_json::Value = serde_json::from_str(&bundle_json).unwrap_or_else(|_| env::panic_str("bundle is not valid JSON"));
        let lev = v["leverage"]["limit"].as_u64().unwrap_or(0);
        let dd = v["drawdown"]["limit"].as_u64().unwrap_or(0);
        require!(lev == self.guardrails.max_leverage_bps && dd == self.guardrails.max_drawdown_bps, "bundle limits do not match vault guardrails");
        self.risk_proofs.push(StoredRiskProof {
            epoch, max_leverage_bps: lev, max_drawdown_bps: dd, bundle_json, posted_at_ms: env::block_timestamp_ms(),
        });
        emit("risk_proof", serde_json::json!({"epoch": epoch}));
        self.charge_storage(before);
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
        let att = env::attached_deposit().as_yoctonear();
        require!(att > ATTESTED_STORAGE_RESERVE, "attach 0.1 NEAR for the callback's storage plus the OutLayer fee");
        let args = serde_json::json!({
            "source": { "Project": {
                "project_id": format!("{}/alpha-vault-strategy", self.owner),
                "version_key": self.approved_wasm_hash,
            }},
            // Bulletproof generation needs ~2.3e9 instructions; OutLayer's
            // default budget (1e9) aborts the run mid-proof.
            "resource_limits": {"max_instructions": 5_000_000_000u64, "max_memory_mb": 256, "max_execution_seconds": 60},
            "response_format": "Json",
            "input_data": input_json,
            "secrets_ref": { "profile": "default", "account_id": self.owner },
        });
        Promise::new("outlayer.testnet".parse().unwrap())
            .function_call("request_execution".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(att - ATTESTED_STORAGE_RESERVE), Gas::from_tgas(150))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(60)).on_decision(input_json))
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
            input_json: Some(input_json.clone()),
        });
        self.attested_ids.insert(id, true);
        self.last_attested_ms = env::block_timestamp_ms();
        emit("commit", serde_json::json!({"id": id, "commitment": commitment, "path": "onchain"}));
        // compact output reveals with the commitment: the contract checks the
        // preimage and applies guardrails right here
        if let (Some(dj), Some(salt)) = (out["sealed"]["decision_json"].as_str(), out["sealed"]["salt_hex"].as_str()) {
            if let Err(e) = self.reveal(id, dj.to_string(), salt.to_string()) {
                emit("attested_reveal_failed", serde_json::json!({"id": id, "reason": e}));
            }
        }
        if let Some(v) = out["settlement"]["external_value_yocto"].as_str().and_then(|x| x.parse::<u128>().ok()) {
            let blk = out["settlement"]["evm_block"].as_u64().unwrap_or(0);
            if let Err(e) = self.apply_settlement(v, blk, format!("attested commit #{id}")) {
                emit("attested_settlement_skipped", serde_json::json!({"id": id, "reason": e}));
            }
        }
        let epoch_in = serde_json::from_str::<serde_json::Value>(&input_json).ok().and_then(|v| v["epoch"].as_u64()).unwrap_or(0);
        if out["risk_bundle"].is_object() {
            // the enclave delivered the proofs themselves: anchor them directly
            let bundle_json = out["risk_bundle"].to_string();
            let lev = out["risk_bundle"]["leverage"]["limit"].as_u64().unwrap_or(0);
            let dd = out["risk_bundle"]["drawdown"]["limit"].as_u64().unwrap_or(0);
            if bundle_json.len() <= MAX_BUNDLE_JSON && lev == self.guardrails.max_leverage_bps && dd == self.guardrails.max_drawdown_bps {
                self.risk_proofs.push(StoredRiskProof {
                    epoch: epoch_in, max_leverage_bps: lev, max_drawdown_bps: dd, bundle_json, posted_at_ms: env::block_timestamp_ms(),
                });
                emit("risk_proof", serde_json::json!({"epoch": epoch_in, "path": "onchain"}));
            } else {
                emit("attested_bundle_skipped", serde_json::json!({"id": id, "reason": "limits differ from guardrails or bundle too large"}));
            }
        } else if let Some(h) = out["bundle_sha256"].as_str().filter(|h| h.len() == 64) {
            self.expected_bundles.insert(h.to_string(), epoch_in);
        }
        if let Some(e) = out["risk_error"].as_str() {
            emit("risk_breach", serde_json::json!({"id": id, "reason": e.chars().take(200).collect::<String>()}));
        }
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
        let p = self.proposal(wasm_hash, wasm_url);
        emit("strategy_proposed", serde_json::json!({"hash": p.hash, "url": p.url, "effective_at_ms": p.effective_at_ms}));
        self.pending_strategy = Some(p);
    }

    /// Anyone may apply once the 48h timelock has passed, provided the
    /// proposer still holds the strategy NFT. This is the only way an
    /// integrity alert is cleared: deliberately, publicly, late.
    pub fn apply_strategy(&mut self) {
        let p = self.pending_strategy.clone().unwrap_or_else(|| env::panic_str("nothing pending"));
        self.check_due(&p);
        emit("strategy_rotated", serde_json::json!({"old": self.approved_wasm_hash, "new": p.hash}));
        self.approved_wasm_hash = p.hash;
        self.strategy_wasm_url = p.url;
        self.pending_strategy = None;
        self.integrity_alert = None;
        if !self.emergency {
            self.deposits_paused = false;
        }
    }

    /// Tighten trust immediately: from now on only TDX-attested on-chain
    /// delivery can commit, execute, settle or prove.
    pub fn enable_attested(&mut self) {
        self.assert_manager();
        self.mode = Mode::Attested;
        self.pending_relayed = None;
        emit("mode", serde_json::json!({"mode": "attested"}));
    }

    /// Loosening trust back to a relayer is timelocked like a code change.
    pub fn propose_relayed(&mut self) {
        self.assert_manager();
        require!(self.mode == Mode::Attested, "already relayed");
        let p = self.proposal("relayed".into(), String::new());
        emit("mode_proposed", serde_json::json!({"mode": "relayed", "effective_at_ms": p.effective_at_ms}));
        self.pending_relayed = Some(p);
    }

    pub fn apply_relayed(&mut self) {
        let p = self.pending_relayed.clone().unwrap_or_else(|| env::panic_str("nothing pending"));
        self.check_due(&p);
        self.mode = Mode::Relayed;
        self.pending_relayed = None;
        emit("mode", serde_json::json!({"mode": "relayed"}));
    }

    /// Timelocked self-upgrade. With the account's full-access key deleted,
    /// this is the only way the contract code can change: the sha256 of the
    /// new code is public for 48h before anyone can deploy it.
    pub fn propose_code(&mut self, code_sha256: String) {
        self.assert_manager();
        require!(code_sha256.len() == 64 && hex::decode(&code_sha256).is_ok(), "code_sha256 must be 32 bytes hex");
        let p = self.proposal(code_sha256, String::new());
        emit("code_proposed", serde_json::json!({"sha256": p.hash, "effective_at_ms": p.effective_at_ms}));
        self.pending_code = Some(p);
    }

    /// Anyone, after the timelock: send the proposed code as the raw call
    /// input. Deploys it and calls its `migrate`.
    pub fn apply_code(&mut self) -> Promise {
        let p = self.pending_code.clone().unwrap_or_else(|| env::panic_str("nothing pending"));
        self.check_due(&p);
        let code = env::input().unwrap_or_else(|| env::panic_str("send the contract code as raw input"));
        require!(hex::encode(env::sha256(&code)) == p.hash, "code does not match the proposed sha256");
        self.pending_code = None;
        emit("code_upgraded", serde_json::json!({"sha256": p.hash}));
        Promise::new(env::current_account_id())
            .deploy_contract(code)
            .function_call("migrate".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(80))
    }

    pub fn cancel_proposals(&mut self) {
        self.assert_manager();
        self.pending_strategy = None;
        self.pending_relayed = None;
        self.pending_code = None;
        emit("proposals_cancelled", serde_json::json!({}));
    }

    // ==================================================================
    // Fees
    // ==================================================================

    /// Mint the manager's fee as shares for gain above the high-water mark.
    /// Only on REALIZED gains: while any value sits cross-chain (priced by a
    /// report rather than held as NEAR) no fee can crystallize. Never in
    /// emergency.
    pub fn crystallize_fees(&mut self) -> U128 {
        if self.emergency || self.total_shares == 0 || self.perf_fee_bps == 0 || self.ext_value > 0 {
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
            "pending_strategy": self.pending_strategy,
            "mode": self.mode,
            "pending_relayed": self.pending_relayed,
            "pending_code": self.pending_code,
            "exit": self.get_exit(),
            "deposits_open": !self.emergency && !self.deposits_paused && self.ext_value == 0,
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
    fn assert_relayed(&self) {
        require!(self.mode == Mode::Relayed, "attested mode: only enclave-delivered output may commit, settle or prove");
    }
    fn proposal(&self, hash: String, url: String) -> Proposal {
        Proposal { hash, url, effective_at_ms: env::block_timestamp_ms() + ROTATION_DELAY_MS, proposer: env::predecessor_account_id() }
    }
    fn check_due(&self, p: &Proposal) {
        require!(env::block_timestamp_ms() >= p.effective_at_ms, "timelock not elapsed");
        require!(self.manager_account() == p.proposer, "proposer no longer holds the strategy NFT");
    }
    fn storage_cost(&self, before: u64) -> u128 {
        u128::from(env::storage_usage().saturating_sub(before)) * env::storage_byte_cost().as_yoctonear()
    }
    /// The caller pays for every byte it adds (excess refunded), so relayed
    /// writes can never eat into shareholders' NEAR.
    fn charge_storage(&self, before: u64) {
        let cost = self.storage_cost(before);
        let att = env::attached_deposit().as_yoctonear();
        require!(att >= cost, format!("attach at least {cost} yoctoNEAR for storage"));
        if att > cost {
            let _ = Promise::new(env::predecessor_account_id()).transfer(NearToken::from_yoctonear(att - cost));
        }
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
// Migration v4 -> v5. Deployed atomically with the code. Future versions
// arrive via the timelocked `apply_code`, which calls `migrate`.
// ---------------------------------------------------------------------------
#[near(serializers = [borsh])]
pub struct VaultV4 {
    owner: AccountId,
    executor: AccountId,
    approved_wasm_hash: String,
    guardrails: Guardrails,
    total_shares: u128,
    shares: LookupMap<AccountId, u128>,
    pool: u128,
    ext_value: u128,
    last_settlement_ref: Option<String>,
    last_settlement_ms: u64,
    strategy_wasm_url: String,
    commitments: Vector<TradeCommitment>,
    risk_proofs: Vector<StoredRiskProof>,
    nav_log: Vector<NavPoint>,
    nav_log_head: u32,
    perf_fee_bps: u64,
    hwm_nav_per_share: u128,
    integrity_alert: Option<String>,
    deposits_paused: bool,
    nft: NonFungibleToken,
    nft_metadata: LazyOption<NFTContractMetadata>,
    last_attested_ms: u64,
    liveness_timeout_ms: u64,
    emergency: bool,
    ext_units_total: u128,
    vault_ext_units: u128,
    ext_units: LookupMap<AccountId, u128>,
    ext_debt: LookupMap<AccountId, u128>,
    ext_pending: LookupMap<AccountId, u128>,
    ext_acc: u128,
    repatriated_total: u128,
    ext_paid_total: u128,
    mpc_path: String,
    evm_address: String,
    pending_wallet: Option<(String, String, u64)>,
    pending_strategy: Option<(String, String, u64)>,
    signed_legs: LookupMap<String, bool>,
    failed_payouts: LookupMap<AccountId, u128>,
}

#[near]
impl Vault {
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let o: VaultV4 = env::state_read().expect("no v4 state to migrate");
        require!(o.pending_wallet.is_none(), "a wallet rotation is pending: v5 removes rotation, cancel it first");
        let manager = o.nft.owner_by_id.get(&STRATEGY_TOKEN_ID.to_string()).unwrap_or_else(|| o.owner.clone());
        let pending_strategy = o.pending_strategy.map(|(hash, url, at)| Proposal { hash, url, effective_at_ms: at, proposer: manager });
        emit("migrated", serde_json::json!({"to": "v5"}));
        Self {
            owner: o.owner,
            executor: o.executor,
            approved_wasm_hash: o.approved_wasm_hash,
            guardrails: o.guardrails,
            total_shares: o.total_shares,
            shares: o.shares,
            pool: o.pool,
            ext_value: o.ext_value,
            last_settlement_ref: o.last_settlement_ref,
            last_settlement_ms: o.last_settlement_ms,
            strategy_wasm_url: o.strategy_wasm_url,
            commitments: o.commitments,
            risk_proofs: o.risk_proofs,
            nav_log: o.nav_log,
            nav_log_head: o.nav_log_head,
            perf_fee_bps: o.perf_fee_bps,
            hwm_nav_per_share: o.hwm_nav_per_share,
            integrity_alert: o.integrity_alert,
            deposits_paused: o.deposits_paused,
            nft: o.nft,
            nft_metadata: o.nft_metadata,
            last_attested_ms: o.last_attested_ms,
            liveness_timeout_ms: o.liveness_timeout_ms,
            emergency: o.emergency,
            ext_units_total: o.ext_units_total,
            vault_ext_units: o.vault_ext_units,
            ext_units: o.ext_units,
            ext_debt: o.ext_debt,
            ext_pending: o.ext_pending,
            ext_acc: o.ext_acc,
            repatriated_total: o.repatriated_total,
            ext_paid_total: o.ext_paid_total,
            mpc_path: o.mpc_path,
            evm_address: o.evm_address,
            pending_strategy,
            signed_legs: o.signed_legs,
            failed_payouts: o.failed_payouts,
            mode: Mode::Relayed,
            pending_relayed: None,
            pending_code: None,
            attested_ids: LookupMap::new(StorageKey::AttestedIds),
            expected_bundles: LookupMap::new(StorageKey::ExpectedBundles),
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
