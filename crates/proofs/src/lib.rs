//! # vault-proofs
//!
//! Pedersen commitments + Bulletproofs range proofs that a vault's **private**
//! risk metrics stayed within **public** limits, without revealing the metrics.
//!
//! ## The statement being proven
//!
//! Given a Pedersen commitment `C = v*B + r*B_blinding` to a secret value `v`
//! (e.g. intra-epoch peak leverage in basis points) and a public limit `L`
//! (e.g. 30_000 = 3x), we prove:
//!
//! ```text
//!     0 <= v < 2^16   AND   0 <= L - v < 2^16      =>      v <= L
//! ```
//!
//! The verifier never learns `v`. The complement commitment is *derived by the
//! verifier* from public data — `C_comp = Com(L, 0) - C` — so the prover cannot
//! lie about the relationship between the two committed values. Both range
//! proofs are aggregated into a single Bulletproof (2 values, 16 bits).
//!
//! A prover holding `v > L` **cannot produce a proof at all**: `L - v`
//! underflows, and there is no 16-bit opening of `Com(L,0) - C`.
//!
//! Used by:
//! * the strategy enclave (prover) — compiled to WASM, runs inside the TEE,
//! * the browser (verifier) — compiled to WASM via `--features wasm`,
//! * the test suite / CLI (native).

use bulletproofs::{BulletproofGens, PedersenGens, RangeProof};
use curve25519_dalek_ng::ristretto::CompressedRistretto;
use curve25519_dalek_ng::scalar::Scalar;
use merlin::Transcript;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

/// Bits in each range proof. 16 bits covers values up to 65_535 bps (6.5535x
/// leverage / 655% drawdown), comfortably above any sane public limit.
pub const BITSIZE: usize = 16;
/// Values per aggregated proof: the metric and its complement to the limit.
const PARTIES: usize = 2;
/// Domain separation label for the Merlin transcript.
const DOMAIN: &[u8] = b"verifiable-alpha-vaults/risk-proof/v1";

/// 1x leverage = 10_000 bps. Default vault limit: 3x.
pub const DEFAULT_MAX_LEVERAGE_BPS: u64 = 30_000;
/// 100% drawdown = 10_000 bps. Default vault limit: 20%.
pub const DEFAULT_MAX_DRAWDOWN_BPS: u64 = 2_000;

#[derive(Debug, thiserror::Error)]
pub enum ProofsError {
    #[error("value {value} exceeds limit {limit}: no proof exists (this is the point)")]
    ValueExceedsLimit { value: u64, limit: u64 },
    #[error("limit {0} does not fit in {BITSIZE} bits")]
    LimitTooLarge(u64),
    #[error("bulletproofs error: {0:?}")]
    Bulletproofs(bulletproofs::ProofError),
    #[error("malformed input: {0}")]
    Malformed(String),
}

impl From<bulletproofs::ProofError> for ProofsError {
    fn from(e: bulletproofs::ProofError) -> Self {
        ProofsError::Bulletproofs(e)
    }
}

fn gens() -> (PedersenGens, BulletproofGens) {
    (PedersenGens::default(), BulletproofGens::new(64, PARTIES))
}

/// Transcript must be built identically by prover and verifier.
fn transcript(label: &str, limit: u64, epoch: u64) -> Transcript {
    let mut t = Transcript::new(DOMAIN);
    t.append_message(b"metric-label", label.as_bytes());
    t.append_u64(b"limit", limit);
    t.append_u64(b"epoch", epoch);
    t
}

/// A proof that a committed secret value is `<= limit`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LimitProof {
    /// Which metric this is ("leverage_bps", "drawdown_bps", ...). Public.
    pub label: String,
    /// The public limit the secret value is proven not to exceed.
    pub limit: u64,
    /// Epoch / round this proof covers (binds the transcript).
    pub epoch: u64,
    /// Hex-encoded compressed Ristretto Pedersen commitment to the secret value.
    pub commitment: String,
    /// Hex-encoded aggregated Bulletproof over [value, limit - value].
    pub proof: String,
}

/// Prover output: the proof plus the secrets needed to open the commitment
/// later if ever required (kept inside the TEE; never published).
#[derive(Debug)]
pub struct ProverArtifacts {
    pub public: LimitProof,
    /// Blinding factor for the value commitment (secret).
    pub blinding: Scalar,
}

/// Prove `value <= limit` without revealing `value`.
pub fn prove_leq(
    label: &str,
    value: u64,
    limit: u64,
    epoch: u64,
) -> Result<ProverArtifacts, ProofsError> {
    if limit >= (1u64 << BITSIZE) {
        return Err(ProofsError::LimitTooLarge(limit));
    }
    let complement = limit
        .checked_sub(value)
        .ok_or(ProofsError::ValueExceedsLimit { value, limit })?;

    let (pc_gens, bp_gens) = gens();
    let mut rng = OsRng;
    let blinding = Scalar::random(&mut rng);
    // The complement's blinding is -r so the verifier can derive its
    // commitment homomorphically: Com(L - v, -r) = Com(L, 0) - Com(v, r).
    let blindings = [blinding, -blinding];
    let values = [value, complement];

    let mut t = transcript(label, limit, epoch);
    let (proof, commitments) =
        RangeProof::prove_multiple(&bp_gens, &pc_gens, &mut t, &values, &blindings, BITSIZE)?;

    Ok(ProverArtifacts {
        public: LimitProof {
            label: label.to_string(),
            limit,
            epoch,
            commitment: hex::encode(commitments[0].as_bytes()),
            proof: hex::encode(proof.to_bytes()),
        },
        blinding,
    })
}

/// Verify a [`LimitProof`]. The verifier reconstructs the complement
/// commitment itself from the public limit, so the prover cannot substitute an
/// unrelated second commitment.
pub fn verify_leq(lp: &LimitProof) -> Result<(), ProofsError> {
    if lp.limit >= (1u64 << BITSIZE) {
        return Err(ProofsError::LimitTooLarge(lp.limit));
    }
    let (pc_gens, bp_gens) = gens();

    let c_bytes: [u8; 32] = hex::decode(&lp.commitment)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| ProofsError::Malformed("commitment must be 32 bytes hex".into()))?;
    let c_value = CompressedRistretto(c_bytes);
    let c_value_pt = c_value
        .decompress()
        .ok_or_else(|| ProofsError::Malformed("commitment is not a valid Ristretto point".into()))?;

    // C_comp = Com(limit, 0) - C_value
    let c_comp = (pc_gens.commit(Scalar::from(lp.limit), Scalar::zero()) - c_value_pt).compress();

    let proof_bytes =
        hex::decode(&lp.proof).map_err(|_| ProofsError::Malformed("proof hex".into()))?;
    let proof = RangeProof::from_bytes(&proof_bytes)
        .map_err(|_| ProofsError::Malformed("proof bytes".into()))?;

    let mut t = transcript(&lp.label, lp.limit, lp.epoch);
    proof.verify_multiple(&bp_gens, &pc_gens, &mut t, &[c_value, c_comp], BITSIZE)?;
    Ok(())
}

/// The bundle a vault publishes every epoch: proofs that peak leverage and
/// max drawdown stayed within the vault's public guardrails.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskProofBundle {
    pub epoch: u64,
    pub leverage: LimitProof,
    pub drawdown: LimitProof,
}

pub struct RiskLimits {
    pub max_leverage_bps: u64,
    pub max_drawdown_bps: u64,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_leverage_bps: DEFAULT_MAX_LEVERAGE_BPS,
            max_drawdown_bps: DEFAULT_MAX_DRAWDOWN_BPS,
        }
    }
}

/// Prove both epoch risk metrics at once. Fails (by design) if either secret
/// metric exceeds its public limit.
pub fn prove_risk(
    epoch: u64,
    peak_leverage_bps: u64,
    max_drawdown_bps: u64,
    limits: &RiskLimits,
) -> Result<RiskProofBundle, ProofsError> {
    let lev = prove_leq("leverage_bps", peak_leverage_bps, limits.max_leverage_bps, epoch)?;
    let dd = prove_leq("drawdown_bps", max_drawdown_bps, limits.max_drawdown_bps, epoch)?;
    Ok(RiskProofBundle {
        epoch,
        leverage: lev.public,
        drawdown: dd.public,
    })
}

/// Verify a full epoch bundle against the vault's public limits.
pub fn verify_risk(bundle: &RiskProofBundle, limits: &RiskLimits) -> Result<(), ProofsError> {
    if bundle.leverage.limit != limits.max_leverage_bps {
        return Err(ProofsError::Malformed(format!(
            "leverage proof limit {} != vault guardrail {}",
            bundle.leverage.limit, limits.max_leverage_bps
        )));
    }
    if bundle.drawdown.limit != limits.max_drawdown_bps {
        return Err(ProofsError::Malformed(format!(
            "drawdown proof limit {} != vault guardrail {}",
            bundle.drawdown.limit, limits.max_drawdown_bps
        )));
    }
    if bundle.leverage.epoch != bundle.epoch || bundle.drawdown.epoch != bundle.epoch {
        return Err(ProofsError::Malformed("epoch mismatch inside bundle".into()));
    }
    verify_leq(&bundle.leverage)?;
    verify_leq(&bundle.drawdown)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Browser bindings (compiled with --features wasm, target wasm32-unknown-unknown)
// ---------------------------------------------------------------------------
#[cfg(feature = "wasm")]
mod wasm_api {
    use super::*;
    use wasm_bindgen::prelude::*;

    /// Verify a serialized `RiskProofBundle` (JSON) against the vault's public
    /// guardrails. Returns "ok" or an error description — the frontend shows a
    /// green badge only on "ok".
    #[wasm_bindgen]
    pub fn verify_risk_bundle_json(
        bundle_json: &str,
        max_leverage_bps: u64,
        max_drawdown_bps: u64,
    ) -> String {
        let bundle: RiskProofBundle = match serde_json::from_str(bundle_json) {
            Ok(b) => b,
            Err(e) => return format!("malformed bundle: {e}"),
        };
        let limits = RiskLimits {
            max_leverage_bps,
            max_drawdown_bps,
        };
        match verify_risk(&bundle, &limits) {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("INVALID: {e}"),
        }
    }

    /// Verify one LimitProof (JSON).
    #[wasm_bindgen]
    pub fn verify_limit_proof_json(proof_json: &str) -> String {
        let lp: LimitProof = match serde_json::from_str(proof_json) {
            Ok(p) => p,
            Err(e) => return format!("malformed proof: {e}"),
        };
        match verify_leq(&lp) {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("INVALID: {e}"),
        }
    }
}

#[cfg(feature = "wasm")]
pub use wasm_api::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honest_leverage_proof_verifies() {
        // 2.5x leverage under a 3x cap.
        let art = prove_leq("leverage_bps", 25_000, 30_000, 7).unwrap();
        verify_leq(&art.public).unwrap();
    }

    #[test]
    fn boundary_values_verify() {
        // exactly at the cap, and at zero
        verify_leq(&prove_leq("leverage_bps", 30_000, 30_000, 1).unwrap().public).unwrap();
        verify_leq(&prove_leq("drawdown_bps", 0, 2_000, 1).unwrap().public).unwrap();
    }

    #[test]
    fn ten_x_leverage_cannot_be_proven() {
        // THE DEMO: 10x leverage under a 3x cap -> no proof exists.
        let err = prove_leq("leverage_bps", 100_000, 30_000, 7).unwrap_err();
        assert!(matches!(err, ProofsError::ValueExceedsLimit { .. }));
    }

    #[test]
    fn proof_does_not_verify_against_a_looser_or_tighter_limit() {
        // Proof made for limit 30_000 must not pass if someone claims the
        // limit was 20_000 (tighter) or 60_000 (looser): the verifier derives
        // the complement commitment from the claimed limit, so it breaks.
        let art = prove_leq("leverage_bps", 25_000, 30_000, 7).unwrap();
        for claimed in [20_000u64, 60_000] {
            let mut forged = art.public.clone();
            forged.limit = claimed;
            assert!(verify_leq(&forged).is_err(), "limit {claimed} should fail");
        }
    }

    #[test]
    fn tampered_commitment_fails() {
        let art = prove_leq("drawdown_bps", 1_500, 2_000, 3).unwrap();
        let mut forged = art.public.clone();
        // flip a nibble in the commitment
        let mut bytes = hex::decode(&forged.commitment).unwrap();
        bytes[0] ^= 0x01;
        forged.commitment = hex::encode(bytes);
        assert!(verify_leq(&forged).is_err());
    }

    #[test]
    fn proof_is_bound_to_label_and_epoch() {
        let art = prove_leq("leverage_bps", 25_000, 30_000, 7).unwrap();
        let mut wrong_label = art.public.clone();
        wrong_label.label = "drawdown_bps".into();
        assert!(verify_leq(&wrong_label).is_err());

        let art2 = prove_leq("leverage_bps", 25_000, 30_000, 7).unwrap();
        let mut wrong_epoch = art2.public.clone();
        wrong_epoch.epoch = 8; // replaying an old proof for a new epoch
        assert!(verify_leq(&wrong_epoch).is_err());
    }

    #[test]
    fn full_bundle_roundtrip_including_json() {
        let limits = RiskLimits::default();
        let bundle = prove_risk(42, 28_000, 1_900, &limits).unwrap();
        verify_risk(&bundle, &limits).unwrap();

        // survives JSON serialization (what gets posted on-chain / fetched by browser)
        let json = serde_json::to_string(&bundle).unwrap();
        let back: RiskProofBundle = serde_json::from_str(&json).unwrap();
        verify_risk(&back, &limits).unwrap();

        // bundle for different guardrails is rejected
        let other = RiskLimits { max_leverage_bps: 20_000, max_drawdown_bps: 2_000 };
        assert!(verify_risk(&back, &other).is_err());
    }

    #[test]
    fn breaching_drawdown_cannot_be_proven() {
        let limits = RiskLimits::default();
        // 25% drawdown under a 20% cap
        assert!(prove_risk(1, 10_000, 2_500, &limits).is_err());
    }

    #[test]
    fn commitment_hides_the_value() {
        // Two proofs over the same value produce different commitments
        // (fresh blinding), so observers can't fingerprint repeated values.
        let a = prove_leq("leverage_bps", 25_000, 30_000, 7).unwrap();
        let b = prove_leq("leverage_bps", 25_000, 30_000, 7).unwrap();
        assert_ne!(a.public.commitment, b.public.commitment);
    }
}
