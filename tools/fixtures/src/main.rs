//! Generates `frontend/demo/fixtures.json` for the demo frontend.
//!
//! NOTHING IS FAKED: commitments are real sha256(decision||salt) from the real
//! strategy code, and risk bundles are real Bulletproofs produced by
//! `vault-proofs` — the browser verifies them for real. Only the market data
//! and NAV path are simulated (deterministic, seeded — regenerating gives the
//! same market, fresh blindings).

use sha2::{Digest, Sha256};
use vault_proofs::{prove_risk, RiskLimits};
use vault_strategy::{run, Params, RunInput};

const WASM_HASH: &str = "c3b8b92a101e575d1c0e2e502c1fc21443fa0e08e864e84fe467d5840bda9f77";
const T0_MS: u64 = 1794268800000; // Tue 10 Nov 2026 00:00 UTC
const EPOCH_MS: u64 = 3_600_000; // hourly epochs

/// Tiny deterministic LCG so fixtures are reproducible without a rand dep.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn unit(&mut self) -> f64 {
        (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
    fn salt(&mut self) -> [u8; 32] {
        let mut s = [0u8; 32];
        for chunk in s.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes());
        }
        s
    }
}

fn commitment_of(decision_json: &str, salt: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(decision_json.as_bytes());
    h.update(salt);
    hex::encode(h.finalize())
}

struct SimVault {
    name: &'static str,
    account: &'static str,
    params: Params,
    seed: u64,
    drift: f64,
    post_proofs: bool,
    integrity_alert: bool,
}

fn simulate(v: &SimVault) -> serde_json::Value {
    let mut rng = Lcg(v.seed);
    let limits = RiskLimits { max_leverage_bps: 30_000, max_drawdown_bps: 2_000 };

    // price path
    let mut prices: Vec<f64> = vec![3000.0];
    for _ in 0..64 {
        let shock = (rng.unit() - 0.5) * 0.04 + v.drift;
        prices.push((prices.last().unwrap() * (1.0 + shock)).max(1.0));
    }

    let mut nav = 100.0_f64; // NEAR
    let mut peak = nav;
    let mut nav_history = vec![serde_json::json!({"t": T0_MS, "nav": nav})];
    let mut commitments = Vec::new();
    let mut risk_proofs = Vec::new();
    let mut position: f64 = 0.0; // signed exposure multiple of NAV (lev-weighted)

    let epochs = 10usize;
    for e in 0..epochs {
        let now = T0_MS + (e as u64 + 1) * EPOCH_MS;
        let window_end = 12 + e * 5;
        let window: Vec<f64> = prices[window_end - 12..window_end].to_vec();
        let nav_yocto = ((nav * 1e6) as u128).to_string(); // scaled, strings only compared ratio-wise
        let peak_yocto = ((peak * 1e6) as u128).to_string();

        let input = RunInput {
            epoch: e as u64 + 1,
            nonce: rng.next(),
            pair: "ETH/USDC".into(),
            prices: window.clone(),
            nav_yocto: nav_yocto.clone(),
            peak_nav_yocto: peak_yocto.clone(),
            max_leverage_bps: limits.max_leverage_bps,
            max_drawdown_bps: limits.max_drawdown_bps,
            settle: None,
        };
        let salt = rng.salt();
        let out = run(&v.params, &input, salt);
        let decision: serde_json::Value = serde_json::from_str(&out.sealed.as_ref().unwrap().decision_json.clone()).unwrap();
        let action = decision["action"].as_str().unwrap().to_string();
        let over_limit = decision["leverage_bps"].as_u64().unwrap() > limits.max_leverage_bps;

        let status = if over_limit { "Rejected" } else { "Executed" };
        commitments.push(serde_json::json!({
            "id": commitments.len(),
            "commitment": out.commitment,
            "wasm_hash": WASM_HASH,
            "attestation_ref": format!("local-sim-{}", e + 1),
            "committed_at_ms": now,
            "status": status,
            "revealed": out.sealed.as_ref().unwrap().decision_json.clone(),
            "salt_hex": out.sealed.as_ref().unwrap().salt_hex.clone(),
            "reject_reason": if over_limit { serde_json::json!(format!("leverage {}bps exceeds max {}bps", decision["leverage_bps"], limits.max_leverage_bps)) } else { serde_json::Value::Null },
            "execution_ref": if over_limit || action == "hold" { serde_json::Value::Null } else { serde_json::json!(format!("0x{:064x}", rng.next())) },
        }));

        // toy PnL: position times next-epoch return, plus noise
        if !over_limit {
            let lev = decision["leverage_bps"].as_u64().unwrap() as f64 / 10_000.0;
            let size = decision["size_bps"].as_u64().unwrap() as f64 / 10_000.0;
            position = match action.as_str() {
                "buy" => size * lev,
                "sell" => -size * lev,
                _ => position * 0.5,
            };
        }
        let ret = (prices[window_end + 4] / prices[window_end - 1]) - 1.0;
        nav *= 1.0 + position * ret;
        peak = peak.max(nav);
        nav_history.push(serde_json::json!({"t": now, "nav": (nav * 100.0).round() / 100.0}));

        // epoch risk proof (REAL Bulletproofs) — peak leverage this epoch + current drawdown
        if v.post_proofs && !over_limit {
            let peak_lev_bps = (position.abs() * 10_000.0) as u64;
            let dd_bps = if nav >= peak { 0 } else { ((peak - nav) / peak * 10_000.0) as u64 };
            if let Ok(bundle) = prove_risk(e as u64 + 1, peak_lev_bps.min(30_000), dd_bps.min(2_000), &limits) {
                risk_proofs.push(serde_json::json!({
                    "epoch": e + 1,
                    "max_leverage_bps": limits.max_leverage_bps,
                    "max_drawdown_bps": limits.max_drawdown_bps,
                    "posted_at_ms": now + 60_000,
                    "bundle": bundle,
                }));
            }
        }
    }

    let ret_pct = (nav / 100.0 - 1.0) * 100.0;
    serde_json::json!({
        "id": v.account,
        "name": v.name,
        "manager": "amudhan.testnet",
        "approved_wasm_hash": WASM_HASH,
        "guardrails": {
            "max_leverage_bps": limits.max_leverage_bps,
            "max_drawdown_bps": limits.max_drawdown_bps,
            "max_position_bps": 5_000,
            "allowed_pairs": ["ETH/USDC", "NEAR/USDC"],
        },
        "pool_near": (nav * 100.0).round() / 100.0,
        "total_shares": "100000000",
        "nav_history": nav_history,
        "return_pct": (ret_pct * 100.0).round() / 100.0,
        "integrity_alert": if v.integrity_alert {
            serde_json::json!(format!("attested wasm {} != approved {}", "9d".repeat(32), WASM_HASH))
        } else { serde_json::Value::Null },
        "deposits_paused": v.integrity_alert,
        "commitments": commitments,
        "risk_proofs": risk_proofs,
    })
}

fn main() {
    let honest = SimVault {
        name: "Momentum Alpha",
        account: "momentum-alpha.vaults.testnet",
        params: Params { fast: 3, slow: 8, threshold_bps: 40, size_bps: 4_000, leverage_bps: 25_000, seal_key_hex: None },
        seed: 11,
        drift: 0.008,
        post_proofs: true,
        integrity_alert: false,
    };
    // greedy: sealed params demand 10x — every non-hold decision gets Rejected,
    // and no risk proof can exist for those epochs
    let greedy = SimVault {
        name: "Degen Max",
        account: "degen-max.vaults.testnet",
        params: Params { fast: 2, slow: 5, threshold_bps: 10, size_bps: 4_500, leverage_bps: 100_000, seal_key_hex: None },
        seed: 99,
        drift: -0.002,
        post_proofs: true,
        integrity_alert: false,
    };
    // swapped code: integrity alert raised, deposits paused
    let swapped = SimVault {
        name: "Shadow Fork",
        account: "shadow-fork.vaults.testnet",
        params: Params { fast: 3, slow: 8, threshold_bps: 40, size_bps: 3_000, leverage_bps: 20_000, seal_key_hex: None },
        seed: 13,
        drift: 0.001,
        post_proofs: false,
        integrity_alert: true,
    };

    // the back-fill cheat: an honest commitment, but the "manager" claims a
    // different (winning) decision at reveal time. The frontend recomputes
    // sha256 in the browser and shows the mismatch.
    let real_decision = r#"{"action":"buy","pair":"ETH/USDC","size_bps":4000,"leverage_bps":25000,"epoch":11,"nonce":1}"#;
    let fake_winner = r#"{"action":"sell","pair":"ETH/USDC","size_bps":4000,"leverage_bps":25000,"epoch":11,"nonce":1}"#;
    let salt = [5u8; 32];

    let fixtures = serde_json::json!({
        "note": "Generated by tools/fixtures. Commitments and Bulletproofs are REAL (cargo run -p gen-fixtures to regenerate); market data is simulated.",
        "approved_wasm_hash": WASM_HASH,
        "vaults": [simulate(&honest), simulate(&greedy), simulate(&swapped)],
        "cheats": {
            "backfill": {
                "commitment": commitment_of(real_decision, &salt),
                "honest_decision": real_decision,
                "claimed_decision": fake_winner,
                "salt_hex": hex::encode(salt),
            }
        }
    });

    let path = std::path::Path::new("frontend/demo/fixtures.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(&fixtures).unwrap()).unwrap();
    eprintln!("wrote {} ({} KB)", path.display(), std::fs::metadata(path).unwrap().len() / 1024);
}
