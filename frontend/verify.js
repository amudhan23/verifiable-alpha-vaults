// Verification engine. Everything here actually verifies — no decorative green.
//
// 1. Trades committed  — recompute sha256(decision_json || salt) in WebCrypto
//                        and compare against the on-chain commitment. Real, offline.
// 2. Code attested     — every commitment's attested wasm hash must equal the
//                        vault's approved hash, and no integrity alert is set.
//                        (Live mode additionally fetches the TDX quote from the
//                        OutLayer attestation API by attestation_ref.)
// 3. Risk proven       — Bulletproofs verified by vault-proofs compiled to
//                        WASM. If the wasm bundle isn't built yet, the badge
//                        stays amber "unverified" — never green.

export async function sha256Hex(bytes) {
  const d = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function hexToBytes(hex) {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Recompute one commitment. Returns {ok, recomputed}. */
export async function checkCommitment(decisionJson, saltHex, claimedCommitment) {
  const dec = new TextEncoder().encode(decisionJson);
  const salt = hexToBytes(saltHex);
  const pre = new Uint8Array(dec.length + salt.length);
  pre.set(dec); pre.set(salt, dec.length);
  const recomputed = await sha256Hex(pre);
  return { ok: recomputed === claimedCommitment, recomputed };
}

/** Badge 2: all revealed trades recompute. */
export async function checkTradesCommitted(vault, onStep = () => {}) {
  const revealed = vault.commitments.filter((c) => c.revealed && c.salt_hex);
  if (revealed.length === 0) return { state: "wait", detail: "no revealed trades yet" };
  let okCount = 0;
  for (const c of revealed) {
    const { ok } = await checkCommitment(c.revealed, c.salt_hex, c.commitment);
    onStep(c.id, ok);
    if (!ok) return { state: "bad", detail: `commitment #${c.id} does not recompute — back-fill detected` };
    okCount++;
    await new Promise((r) => setTimeout(r, 60)); // let the viewer see it run
  }
  return { state: "ok", detail: `${okCount}/${revealed.length} commitments recomputed in this browser` };
}

/** Badge 1: code hashes match the approved strategy. */
export function checkCodeAttested(vault) {
  if (vault.integrity_alert) return { state: "bad", detail: vault.integrity_alert };
  const mismatched = vault.commitments.filter((c) => c.wasm_hash !== vault.approved_wasm_hash);
  if (mismatched.length > 0)
    return { state: "bad", detail: `${mismatched.length} decisions attested to different code` };
  if (vault.commitments.length === 0) return { state: "wait", detail: "no attested decisions yet" };
  return {
    state: "ok",
    detail: `${vault.commitments.length} decisions attested to ${vault.approved_wasm_hash.slice(0, 12)}…`,
  };
}

// --- Bulletproofs via wasm ---------------------------------------------------
let wasmVerifier = null; // { verify_risk_bundle_json }
let wasmTried = false;

export async function loadProofWasm() {
  if (wasmTried) return wasmVerifier;
  wasmTried = true;
  try {
    const mod = await import("./pkg/vault_proofs.js");
    await mod.default();
    wasmVerifier = mod;
  } catch {
    wasmVerifier = null; // not built yet — badge stays amber
  }
  return wasmVerifier;
}

/** Badge 3: verify every posted risk bundle with real Bulletproofs. */
export async function checkRiskProven(vault, onStep = () => {}) {
  const expected = vault.commitments.filter((c) => c.status === "Executed").length;
  if (vault.risk_proofs.length === 0) {
    const rejected = vault.commitments.filter((c) => c.status === "Rejected").length;
    return {
      state: rejected > 0 ? "bad" : "wait",
      detail:
        rejected > 0
          ? `no proofs posted — ${rejected} decisions breached limits (no proof can exist for a breach)`
          : "no risk proofs posted yet",
    };
  }
  const w = await loadProofWasm();
  if (!w) {
    return {
      state: "wait",
      detail: `${vault.risk_proofs.length} proof bundles posted — build the verifier wasm to check them here (scripts/build-wasm.sh)`,
    };
  }
  for (const p of vault.risk_proofs) {
    const res = w.verify_risk_bundle_json(
      JSON.stringify(p.bundle),
      BigInt(vault.guardrails.max_leverage_bps),
      BigInt(vault.guardrails.max_drawdown_bps)
    );
    onStep(p.epoch, res === "ok");
    if (res !== "ok") return { state: "bad", detail: `epoch ${p.epoch}: ${res}` };
    await new Promise((r) => setTimeout(r, 40));
  }
  return {
    state: "ok",
    detail: `${vault.risk_proofs.length} Bulletproof bundles verified in this browser (leverage ≤ ${vault.guardrails.max_leverage_bps / 10000}x, drawdown ≤ ${vault.guardrails.max_drawdown_bps / 100}%)`,
  };
}

export async function runAllChecks(vault, hooks = {}) {
  const code = checkCodeAttested(vault);
  hooks.onCode?.(code);
  const trades = await checkTradesCommitted(vault, hooks.onTradeStep);
  hooks.onTrades?.(trades);
  const risk = await checkRiskProven(vault, hooks.onRiskStep);
  hooks.onRisk?.(risk);
  return { code, trades, risk };
}
