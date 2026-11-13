#!/usr/bin/env node
// One strategy cycle, end to end:
//
//   oracle (Pyth→CoinGecko) + cross-chain balances
//     -> enclave (sealed params; TDX-attested)
//     -> SETTLE previous cross-chain value into NAV (attested, fraud-provable)
//     -> COMMIT decision hash + publish exact enclave input on the vault
//     -> EXECUTE as a real Uniswap v3 swap via Chain Signatures (or simulate)
//     -> REVEAL decision+salt (guardrails enforced by the contract)
//     -> anchor Bulletproof risk bundle
//
//   node run.mjs                                            # offline demo
//   node run.mjs --enclave outlayer --vault near            # testnet, simulated fill
//   node run.mjs --enclave outlayer --vault near --execute evm   # + real swap
//   add --every 30 to loop every 30 min · --fixed for deterministic prices

import crypto from "node:crypto";
import { LocalEnclave, OutLayerEnclave } from "./enclave.mjs";
import { NearVault, DryRunVault } from "./vault.mjs";
import { VaultSignedExecutor } from "./evm.mjs";
import { fetchMarket, fixedMarket } from "./oracle.mjs";

const arg = (name, dflt) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > -1 ? process.argv[i + 1] ?? true : dflt;
};
const flag = (name) => process.argv.includes(`--${name}`);
const need = (k) => {
  const v = process.env[k];
  if (!v) { console.error(`missing env ${k}`); process.exit(1); }
  return v;
};
const APPROVED_WASM_HASH =
  process.env.APPROVED_WASM_HASH ?? "c3b8b92a101e575d1c0e2e502c1fc21443fa0e08e864e84fe467d5840bda9f77";

// --enclave onchain: the VAULT requests the run from OutLayer on-chain
// (request_execution, resolved only by DCAP-verified TDX workers); the output
// lands in the vault's own callback, never passing through this process.
const onchainEnclave = arg("enclave", "local") === "onchain";
const live = arg("enclave", "local") === "outlayer";
const enclave = live
  ? new OutLayerEnclave({
      owner: need("NEAR_ACCOUNT_ID"),
      project: process.env.OUTLAYER_PROJECT ?? "alpha-vault-strategy",
      paymentKey: need("OUTLAYER_PAYMENT_KEY"),
    })
  : new LocalEnclave({
      // Demo-only params. On OutLayer these live in an encrypted secret.
      strategyParams: { fast: 3, slow: 8, threshold_bps: 40, size_bps: 4000, leverage_bps: 25000 },
    });
const onChain = arg("vault", "dry") === "near";
const vault = onChain
  ? new NearVault({ contractId: need("VAULT_CONTRACT_ID"), accountId: need("NEAR_ACCOUNT_ID"), privateKey: need("NEAR_PRIVATE_KEY") })
  : new DryRunVault({ approvedWasmHash: APPROVED_WASM_HASH });
// balances read on every live cycle (risk + settlement use real holdings);
// swaps only with --execute evm — and the VAULT signs them (v4)
const chainsig = onChain ? new VaultSignedExecutor({ vault, vaultId: need("VAULT_CONTRACT_ID") }) : null;
const evm = arg("execute", "sim") === "evm" ? chainsig : null;

/** Decrypt the enclave's sealed payload (ChaCha20-Poly1305, key = SEAL_KEY_HEX). */
async function unseal(output) {
  if (output.sealed) return output.sealed;
  const key = Buffer.from(need("SEAL_KEY_HEX"), "hex");
  const blob = Buffer.from(output.sealed_enc, "hex");
  const { chacha20poly1305 } = await import("@noble/ciphers/chacha.js");
  const pt = chacha20poly1305(key, blob.subarray(0, 12)).decrypt(blob.subarray(12));
  return JSON.parse(Buffer.from(pt).toString());
}

async function cycle() {
  const epoch = Number(arg("epoch", `${Math.floor(Date.now() / 3_600_000)}`));
  const market = flag("fixed") || !(live || onchainEnclave) ? fixedMarket() : await fetchMarket();
  console.log(`→ oracle: ${market.oracle.spot_source} · ETH $${market.oracle.eth_usd} · NEAR $${market.oracle.near_usd}`);

  let settle = null;
  if (chainsig) {
    const b = await chainsig.balances();
    settle = {
      eth_balance_wei: (b.eth + b.weth).toString(),
      usdc_balance_6dp: b.usdc.toString(),
      eth_usd: market.oracle.eth_usd,
      near_usd: market.oracle.near_usd,
      evm_block: Number(b.block),
    };
    console.log(`  cross-chain holdings @ block ${b.block}: ${(Number(b.eth + b.weth) / 1e18).toFixed(5)} ETH, ${(Number(b.usdc) / 1e6).toFixed(2)} USDC`);
  }

  const summary = onChain ? await vault.view("get_summary") : null;
  const nav = summary?.nav_yocto ?? "100000000";
  const input = {
    epoch, nonce: Date.now(), pair: "ETH/USDC",
    prices: market.prices,
    nav_yocto: nav, peak_nav_yocto: nav,
    max_leverage_bps: 30000, max_drawdown_bps: 2000,
    oracle: market.oracle,
    ...(settle ? { settle } : {}),
    // checked against the contract's own state when the commitment lands
    ...(summary ? { vault_state: {
      pool_yocto: summary.pool_yocto,
      total_shares: summary.total_shares,
      hwm_nps: summary.hwm_nav_per_share,
      external_value_yocto: summary.external_value_yocto,
      max_leverage_bps: summary.guardrails.max_leverage_bps,
      max_drawdown_bps: summary.guardrails.max_drawdown_bps,
    } } : {}),
  };

  if (onchainEnclave) return onchainCycle({ input, epoch, market });
  if (summary?.approved_wasm_hash && "versionKey" in enclave) enclave.versionKey = summary.approved_wasm_hash;
  console.log(`→ enclave run (epoch ${epoch})`);
  const { output, attestationRef, callId, timeMs, executedWasm } = await enclave.run(input);
  const wasmForVault = executedWasm ?? APPROVED_WASM_HASH; // local sim has no attestation
  console.log(`  done in ${timeMs}ms · call ${callId}${executedWasm ? ` · TDX ran ${executedWasm.slice(0, 12)}…` : ""}`);
  const sealed = await unseal(output);

  console.log("→ commit on vault (before execution) + publish the exact enclave input");
  const id = await vault.commitDecision(output.commitment, wasmForVault, attestationRef, JSON.stringify(input));
  console.log(`  committed as #${id} · input sha256 ${crypto.createHash("sha256").update(JSON.stringify(input)).digest("hex").slice(0, 16)}…`);

  const decision = JSON.parse(sealed.decision_json);

  // v4 order: REVEAL first (guardrails checked on-chain), then the VAULT
  // signs the swap. Brief public window before the swap lands: bounded by
  // the 2% slippage guard; on mainnet, submit via a private mempool.
  console.log("→ reveal on vault (guardrails enforced by the contract)");
  await vault.executeDecision(id, sealed.decision_json, sealed.salt_hex);
  const status = onChain ? (await vault.view("get_commitment", { id })).status : "Executed";
  if (status !== "Executed") {
    console.log(`  decision ${status} by guardrails — nothing will be signed`);
  } else if (evm && decision.action !== "hold") {
    console.log(`→ execute ${decision.action}: the vault signs each leg via MPC; executor verifies + broadcasts`);
    const r = await evm.executeTrade({ id, commitment: output.commitment, decision, ethUsd: market.oracle.eth_usd });
    for (const t of r.txs) console.log(`  ${t.step}: ${t.status} ${t.hash}`);
    console.log(`  ${r.detail}`);
    if (r.ref) await vault.recordExecutionRef(id, r.ref);
  } else {
    console.log(`→ ${decision.action}${evm ? "" : " [simulated fill — pass --execute evm for a real swap]"}`);
  }

  if (output.risk_bundle) {
    await vault.postRiskProof(epoch, JSON.stringify(output.risk_bundle));
    console.log(`→ Bulletproof risk bundle anchored (basis: ${output.risk_basis || "decision"})`);
  } else if (output.risk_error) {
    console.log(`→ NO risk proof possible: ${output.risk_error}`);
  }

  // settle last (state checked at commit must not move mid-cycle). v4 bounds:
  // >= 1h apart, holders' NAV may move <= 10% per settlement; a larger true
  // move converges over several bounded settlements.
  if (output.settlement && onChain) {
    const s = await vault.view("get_summary");
    const since = Date.now() - Number(s.last_settlement_ms || 0);
    if (Number(s.last_settlement_ms) > 0 && since < 3_600_000) {
      console.log(`→ settlement skipped: last one ${Math.round(since / 60000)} min ago (1h minimum)`);
    } else {
      const target = BigInt(output.settlement.external_value_yocto);
      const units = BigInt(s.ext_units_total), vunits = BigInt(s.vault_ext_units);
      const pool = BigInt(s.pool_yocto), nav = BigInt(s.nav_yocto);
      let wallet = target;
      if (units > 0n && vunits > 0n) {
        const step = (nav * 99n) / 1000n; // 9.9%
        let holders = (target * vunits) / units;
        const lo = nav - step - pool, hi = nav + step - pool;
        if (holders > hi) holders = hi;
        if (holders < (lo > 0n ? lo : 0n)) holders = lo > 0n ? lo : 0n;
        wallet = (holders * units) / vunits;
      }
      const clamped = wallet !== target;
      console.log(`→ settle cross-chain wallet: ${(Number(wallet) / 1e24).toFixed(4)} NEAR @ evm block ${output.settlement.evm_block}${clamped ? ` (bounded step toward ${(Number(target) / 1e24).toFixed(4)})` : ""}`);
      await vault.settlePnl(wallet.toString(), output.settlement.evm_block, attestationRef);
    }
  } else if (output.settlement) {
    await vault.settlePnl(output.settlement.external_value_yocto, output.settlement.evm_block, attestationRef);
  }
  const after = onChain ? await vault.view("get_summary") : await vault.view();
  console.log("\ncycle complete:", JSON.stringify({ commitments: after.commitments, risk_proofs: after.risk_proofs, nav_yocto: after.nav_yocto }, null, 0));
}

async function onchainCycle({ input, epoch, market }) {
  console.log(`→ vault.request_decision (epoch ${epoch}): OutLayer runs the approved build in TDX and calls the vault back`);
  const t0 = Date.now();
  const res = await vault.callRaw("request_decision", { input_json: JSON.stringify(input) },
    240_000_000_000_000n, (100_000_000_000_000_000_000_000n + 7_500_000_000_000_000_000_000n).toString()); // 0.1 storage reserve + OutLayer fee
  const logs = res.receipts_outcome.flatMap((o) => o.outcome.logs);
  const ev = (name) => logs.filter((l) => l.includes(`"event":"${name}"`)).map((l) => JSON.parse(l.slice(11)).data[0]);
  const failed = ev("decision_request_failed")[0];
  const commit = ev("commit")[0];
  console.log(`  tx ${res.transaction.hash} · ${((Date.now() - t0) / 1000).toFixed(1)}s`);
  if (failed || !commit) {
    for (const l of logs) if (/Execution failed|decision_request_failed/.test(l)) console.log("  " + l.slice(0, 300));
    throw new Error("on-chain request did not deliver a decision");
  }
  const id = commit.id;
  console.log(`  committed as #${id} by the vault's own callback (attested path)`);
  if (ev("risk_proof").length) console.log("  Bulletproof bundle anchored from the enclave's delivery");
  for (const e of ev("attested_settlement_skipped")) console.log(`  settlement skipped: ${e.reason}`);
  if (ev("settlement").length) console.log("  settlement applied from the enclave's delivery");
  let c = await vault.view("get_commitment", { id });
  if (c.status === "Committed") {
    // the decision is sealed to the relayer's key; its hash is already fixed
    // on-chain, so revealing is all the relayer can do with it
    const sealed = await unseal({ sealed_enc: c.sealed_enc });
    console.log("→ reveal (hash fixed by the enclave's on-chain commitment)");
    await vault.executeDecision(id, sealed.decision_json, sealed.salt_hex);
    c = await vault.view("get_commitment", { id });
  }
  const decision = JSON.parse(c.revealed);
  if (c.status !== "Executed") console.log(`  decision ${c.status}: nothing will be signed`);
  else if (evm && decision.action !== "hold") {
    console.log(`→ execute ${decision.action} ${decision.amount_in}: the vault signs each leg via MPC`);
    const r = await evm.executeTrade({ id, commitment: c.commitment, decision, ethUsd: market.oracle.eth_usd });
    for (const t of r.txs) console.log(`  ${t.step}: ${t.status} ${t.hash}`);
    console.log(`  ${r.detail}`);
    if (r.ref) await vault.recordExecutionRef(id, r.ref);
  } else console.log(`→ ${decision.action}`);
  const after = await vault.view("get_summary");
  console.log("\ncycle complete:", JSON.stringify({ mode: after.mode, commitments: after.commitments, risk_proofs: after.risk_proofs, nav_yocto: after.nav_yocto }));
}

const everyMin = Number(arg("every", "0"));
do {
  try { await cycle(); } catch (e) { console.error("cycle failed:", e.message); if (!everyMin) process.exit(1); }
  if (everyMin > 0) {
    console.log(`\n(sleeping ${everyMin} min — ctrl-c to stop)\n`);
    await new Promise((r) => setTimeout(r, everyMin * 60_000));
  }
} while (everyMin > 0);
