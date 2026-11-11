//! Momentum strategy with **sealed parameters**.
//!
//! The code is public (this file — anyone can audit what *kind* of strategy it
//! is and that it can't exfiltrate anything). The *parameters* — lookback
//! windows, entry threshold, sizing, leverage — arrive as an OutLayer secret
//! that only this WASM, running inside the TDX enclave, can read. That is the
//! protected asset: the edge is in the tuning, and the tuning never leaves
//! the enclave.
//!
//! Every run outputs:
//! * `commitment` — sha256(decision_json || salt): posted on NEAR *before*
//!   execution, so trades can't be back-filled;
//! * `risk_bundle` — Bulletproofs that the run's risk metrics are within the
//!   vault's public guardrails, without revealing the metrics;
//! * `sealed` — the decision + salt, for the executor to reveal at execution.
//!
//! The enclave can *decide* whatever its parameters tell it to — including a
//! breach. Two independent layers catch that: the risk proof fails to exist
//! (see `breach_params_cannot_produce_risk_proof`), and the vault contract
//! refuses the reveal before anything is signed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vault_proofs::{prove_risk, RiskLimits, RiskProofBundle};

/// Sealed parameters (OutLayer secret). THIS IS THE EDGE.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Params {
    /// Fast SMA lookback (bars).
    pub fast: usize,
    /// Slow SMA lookback (bars).
    pub slow: usize,
    /// Entry threshold on fast/slow divergence, in bps.
    pub threshold_bps: u64,
    /// Position size per trade, bps of NAV.
    pub size_bps: u64,
    /// Leverage per trade, bps (10_000 = 1x).
    pub leverage_bps: u64,
}

/// Public per-run input, assembled by the executor from on-chain + oracle data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunInput {
    pub epoch: u64,
    pub nonce: u64,
    pub pair: String,
    /// Recent closes, oldest first (from the price oracle).
    pub prices: Vec<f64>,
    /// Current NAV and the high-water NAV, yoctoNEAR as strings.
    pub nav_yocto: String,
    pub peak_nav_yocto: String,
    /// Vault guardrails (public; must match the contract).
    pub max_leverage_bps: u64,
    pub max_drawdown_bps: u64,
}

/// Decision field order is FIXED: the contract reveals the exact JSON string,
/// so prover and verifier always hash identical bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub action: String, // "buy" | "sell" | "hold"
    pub pair: String,
    pub size_bps: u64,
    pub leverage_bps: u64,
    pub epoch: u64,
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sealed {
    pub decision_json: String,
    pub salt_hex: String,
}

/// Full run output (stdout of the enclave).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunOutput {
    /// sha256(decision_json || salt), hex — goes on NEAR immediately.
    pub commitment: String,
    /// Bulletproofs for this run's risk metrics, or null + reason if the
    /// metrics breach the limits (in which case no proof *can* exist).
    pub risk_bundle: Option<RiskProofBundle>,
    pub risk_error: Option<String>,
    /// Decision + salt for the executor to reveal at execution time.
    pub sealed: Sealed,
}

fn sma(prices: &[f64], n: usize) -> Option<f64> {
    if n == 0 || prices.len() < n {
        return None;
    }
    Some(prices[prices.len() - n..].iter().sum::<f64>() / n as f64)
}

/// Pure decision logic — deterministic given params + input.
pub fn decide(params: &Params, input: &RunInput) -> Decision {
    let hold = Decision {
        action: "hold".into(),
        pair: input.pair.clone(),
        size_bps: 0,
        leverage_bps: 0,
        epoch: input.epoch,
        nonce: input.nonce,
    };
    let (Some(fast), Some(slow)) = (sma(&input.prices, params.fast), sma(&input.prices, params.slow))
    else {
        return hold; // not enough history
    };
    if slow <= 0.0 {
        return hold;
    }
    let momentum_bps = ((fast - slow) / slow * 10_000.0) as i64;
    let action = if momentum_bps >= params.threshold_bps as i64 {
        "buy"
    } else if momentum_bps <= -(params.threshold_bps as i64) {
        "sell"
    } else {
        return hold;
    };
    Decision {
        action: action.into(),
        pair: input.pair.clone(),
        size_bps: params.size_bps,
        leverage_bps: params.leverage_bps,
        epoch: input.epoch,
        nonce: input.nonce,
    }
}

/// Drawdown vs high-water mark, in bps (0 if at/above the peak).
pub fn drawdown_bps(nav_yocto: &str, peak_nav_yocto: &str) -> u64 {
    let nav: u128 = nav_yocto.parse().unwrap_or(0);
    let peak: u128 = peak_nav_yocto.parse().unwrap_or(0);
    if peak == 0 || nav >= peak {
        return 0;
    }
    ((peak - nav).saturating_mul(10_000) / peak) as u64
}

/// One full enclave run: decide, commit, prove.
/// `salt` must come from the TEE's entropy (random_get); injected here so the
/// logic is testable.
pub fn run(params: &Params, input: &RunInput, salt: [u8; 32]) -> RunOutput {
    let decision = decide(params, input);
    let decision_json = serde_json::to_string(&decision).expect("decision serializes");

    let mut hasher = Sha256::new();
    hasher.update(decision_json.as_bytes());
    hasher.update(salt);
    let commitment = hex::encode(hasher.finalize());

    // Risk metrics for this run: leverage the vault would be at if the
    // decision executes, and current drawdown vs the high-water mark.
    let run_leverage = if decision.action == "hold" { 0 } else { decision.leverage_bps };
    let dd = drawdown_bps(&input.nav_yocto, &input.peak_nav_yocto);
    let limits = RiskLimits {
        max_leverage_bps: input.max_leverage_bps,
        max_drawdown_bps: input.max_drawdown_bps,
    };
    let (risk_bundle, risk_error) = match prove_risk(input.epoch, run_leverage, dd, &limits) {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e.to_string())),
    };

    RunOutput {
        commitment,
        risk_bundle,
        risk_error,
        sealed: Sealed {
            decision_json,
            salt_hex: hex::encode(salt),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_proofs::verify_risk;

    fn params() -> Params {
        Params { fast: 3, slow: 8, threshold_bps: 50, size_bps: 4_000, leverage_bps: 25_000 }
    }

    fn input(prices: Vec<f64>) -> RunInput {
        RunInput {
            epoch: 1,
            nonce: 7,
            pair: "ETH/USDC".into(),
            prices,
            nav_yocto: "950".into(),
            peak_nav_yocto: "1000".into(), // 5% drawdown
            max_leverage_bps: 30_000,
            max_drawdown_bps: 2_000,
        }
    }

    fn rising() -> Vec<f64> {
        vec![100., 100., 100., 100., 100., 102., 105., 109., 114., 120.]
    }
    fn falling() -> Vec<f64> {
        vec![120., 120., 120., 120., 120., 117., 112., 106., 100., 95.]
    }
    fn flat() -> Vec<f64> {
        vec![100.; 10]
    }

    #[test]
    fn momentum_signals_are_deterministic_and_directional() {
        let p = params();
        assert_eq!(decide(&p, &input(rising())).action, "buy");
        assert_eq!(decide(&p, &input(falling())).action, "sell");
        assert_eq!(decide(&p, &input(flat())).action, "hold");
        // same inputs, same decision — byte-for-byte
        let a = serde_json::to_string(&decide(&p, &input(rising()))).unwrap();
        let b = serde_json::to_string(&decide(&p, &input(rising()))).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parameters_change_the_decision_proving_they_matter() {
        // Same market, different sealed params -> different behavior.
        // (This is exactly what the vault hides.)
        let cautious = Params { threshold_bps: 2_000, ..params() };
        assert_eq!(decide(&params(), &input(rising())).action, "buy");
        assert_eq!(decide(&cautious, &input(rising())).action, "hold");
    }

    #[test]
    fn commitment_binds_decision_and_salt() {
        let out = run(&params(), &input(rising()), [7u8; 32]);
        // recompute like the contract does
        let mut pre = out.sealed.decision_json.as_bytes().to_vec();
        pre.extend_from_slice(&hex::decode(&out.sealed.salt_hex).unwrap());
        let digest = hex::encode(sha2::Sha256::digest(&pre));
        assert_eq!(digest, out.commitment);
        // a different salt yields a different commitment
        let out2 = run(&params(), &input(rising()), [8u8; 32]);
        assert_ne!(out.commitment, out2.commitment);
    }

    #[test]
    fn honest_run_produces_verifiable_risk_bundle() {
        let out = run(&params(), &input(rising()), [1u8; 32]);
        let bundle = out.risk_bundle.expect("honest run proves its risk");
        verify_risk(
            &bundle,
            &RiskLimits { max_leverage_bps: 30_000, max_drawdown_bps: 2_000 },
        )
        .unwrap();
        assert!(out.risk_error.is_none());
    }

    #[test]
    fn breach_params_cannot_produce_risk_proof() {
        // Malicious sealed params: 10x leverage. The decision is still
        // emitted (the enclave is not the enforcement point), but no risk
        // proof can exist — and the contract will refuse the reveal.
        let greedy = Params { leverage_bps: 100_000, ..params() };
        let out = run(&greedy, &input(rising()), [1u8; 32]);
        assert!(out.risk_bundle.is_none());
        assert!(out.risk_error.unwrap().contains("exceeds limit"));
        let d: Decision = serde_json::from_str(&out.sealed.decision_json).unwrap();
        assert_eq!(d.leverage_bps, 100_000);
    }

    #[test]
    fn drawdown_math() {
        assert_eq!(drawdown_bps("950", "1000"), 500); // 5%
        assert_eq!(drawdown_bps("1000", "1000"), 0);
        assert_eq!(drawdown_bps("1100", "1000"), 0); // above peak
        assert_eq!(drawdown_bps("0", "1000"), 10_000);
        assert_eq!(drawdown_bps("500", "0"), 0); // no peak yet
    }

    #[test]
    fn deep_drawdown_cannot_be_proven() {
        // 25% drawdown under a 20% cap: decision fine, proof impossible.
        let mut inp = input(flat());
        inp.nav_yocto = "750".into();
        inp.peak_nav_yocto = "1000".into();
        let out = run(&params(), &inp, [1u8; 32]);
        assert!(out.risk_bundle.is_none());
    }
}
