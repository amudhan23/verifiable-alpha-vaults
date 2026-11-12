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

/** Badge 1 (quick, sync): on-chain state only, for leaderboard chips.
 * The contract records each decision's attested wasm hash and refuses (and
 * pauses deposits) if it differs from the hash approved AT THAT MOMENT, so
 * the on-chain signal of a swap is the integrity alert / a code rejection.
 * A strategy rotation by the NFT holder is legitimate and logged. */
export function checkCodeAttested(vault) {
  if (vault.integrity_alert) return { state: "bad", detail: vault.integrity_alert };
  const swapped = vault.commitments.filter((c) => c.reject_reason?.includes("code hash"));
  if (swapped.length > 0)
    return { state: "bad", detail: `${swapped.length} decisions arrived attested to unapproved code` };
  if (vault.commitments.length === 0) return { state: "wait", detail: "no attested decisions yet" };
  const builds = [...new Set(vault.commitments.map((c) => c.wasm_hash))];
  const rot = builds.length > 1 ? ` across ${builds.length} approved builds (${builds.length - 1} logged rotation${builds.length > 2 ? "s" : ""})` : "";
  return { state: "ok", detail: `${vault.commitments.length} decisions attested${rot}; current build ${vault.approved_wasm_hash.slice(0, 12)}…` };
}

/** Badge 1 (deep): fetch each TDX attestation from OutLayer's public API and
 * check the code that ACTUALLY executed equals the hash the contract recorded
 * (and enforced) for that decision. Real third-party verification. */
export async function checkCodeAttestedDeep(vault, onStep = () => {}) {
  const quick = checkCodeAttested(vault);
  if (quick.state !== "ok") return quick;
  const fetchable = vault.commitments.filter((c) => c.attestation_ref?.startsWith("https://")).slice(-6);
  if (fetchable.length === 0)
    return { ...quick, detail: quick.detail + " (no fetchable attestations — demo data)" };
  let verified = 0;
  for (const c of fetchable) {
    onStep(c.id);
    try {
      const att = await (await fetch(c.attestation_ref)).json();
      const executed = att.executed_wasm_sha256 ?? att.wasm_hash;
      if (executed !== c.wasm_hash)
        return { state: "bad", detail: `#${c.id}: TDX says ${String(executed).slice(0, 12)}… ran, vault recorded ${c.wasm_hash.slice(0, 12)}…` };
      if (!att.tdx_quote) return { state: "bad", detail: `attestation for #${c.id} has no TDX quote` };
      verified++;
    } catch {
      return { state: "wait", detail: `attestation for #${c.id} not reachable right now` };
    }
  }
  const builds = new Set(vault.commitments.map((c) => c.wasm_hash)).size;
  // commitments the vault created in its own callback from OutLayer's
  // on-chain request_execution: only DCAP-verified TDX workers can resolve it,
  // and the run was pinned to the approved build by the contract itself
  const onchain = vault.commitments.filter((c) => c.attestation_ref?.startsWith("onchain")).length;
  return {
    state: "ok",
    detail: `${verified} TDX attestations fetched: executed code = recorded code on all${builds > 1 ? ` (${builds - 1} logged strategy rotation)` : ""}${onchain ? `; ${onchain} delivered on-chain by OutLayer's TDX workers straight to the vault` : ""}`,
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
  const onPositions = vault.commitments.filter((c) => c.input_json?.includes('"vault_state"')).length;
  return {
    state: "ok",
    detail: `${vault.risk_proofs.length} Bulletproof bundles verified in this browser (leverage ≤ ${vault.guardrails.max_leverage_bps / 10000}x, drawdown ≤ ${vault.guardrails.max_drawdown_bps / 100}%)${onPositions ? `; latest ${onPositions} computed from actual holdings, inputs checked by the contract` : ""}`,
  };
}

/** Check 4: the strategy saw REAL prices. For every decision that published
 * its enclave input: sha256(input_json) must equal the TDX attestation's
 * input_hash, and the oracle price inside must match CoinGecko's public
 * history at that timestamp (±2%). */
export async function checkPricesVerified(vault, onStep = () => {}) {
  const withInput = vault.commitments.filter((c) => c.input_json && c.attestation_ref?.startsWith("https://")).slice(-4);
  if (withInput.length === 0) return { state: "wait", detail: "no decisions with published inputs yet" };
  let hashed = 0, crossChecked = 0;
  for (const c of withInput) {
    onStep(c.id);
    const att = await (await fetch(c.attestation_ref)).json().catch(() => null);
    if (!att) return { state: "wait", detail: `attestation for #${c.id} not reachable` };
    const h = await sha256Hex(new TextEncoder().encode(c.input_json));
    if (h !== att.input_hash)
      return { state: "bad", detail: `#${c.id}: published input does not hash to the attested input_hash` };
    hashed++;
    const o = JSON.parse(c.input_json).oracle;
    if (o?.as_of && o.eth_usd) {
      try {
        const r = await fetch(`https://api.coingecko.com/api/v3/coins/ethereum/market_chart/range?vs_currency=usd&from=${o.as_of - 900}&to=${o.as_of + 900}`);
        const pts = (await r.json()).prices ?? [];
        if (pts.length) {
          const near = pts.reduce((a, b) => (Math.abs(b[0] / 1000 - o.as_of) < Math.abs(a[0] / 1000 - o.as_of) ? b : a));
          if (Math.abs(near[1] / o.eth_usd - 1) > 0.02)
            return { state: "bad", detail: `#${c.id}: enclave saw ETH $${o.eth_usd}, public history says $${near[1].toFixed(2)}` };
          crossChecked++;
        }
      } catch { /* public history unreachable: hash check still holds */ }
    }
  }
  return {
    state: "ok",
    detail: `${hashed} enclave inputs match their attested input_hash${crossChecked ? `; ${crossChecked} oracle prices match public history (±2%)` : ""}`,
  };
}

export async function runAllChecks(vault, hooks = {}) {
  const code = await checkCodeAttestedDeep(vault, hooks.onCodeStep);
  hooks.onCode?.(code);
  const trades = await checkTradesCommitted(vault, hooks.onTradeStep);
  hooks.onTrades?.(trades);
  const risk = await checkRiskProven(vault, hooks.onRiskStep);
  hooks.onRisk?.(risk);
  const prices = await checkPricesVerified(vault, hooks.onPriceStep);
  hooks.onPrices?.(prices);
  return { code, trades, risk, prices };
}
