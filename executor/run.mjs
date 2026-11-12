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
import { ChainSigExecutor } from "./evm.mjs";
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
const evm = arg("execute", "sim") === "evm"
  ? new ChainSigExecutor({ accountId: need("NEAR_ACCOUNT_ID"), privateKey: need("NEAR_PRIVATE_KEY") })
  : null;

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
  const market = flag("fixed") || !live ? fixedMarket() : await fetchMarket();
  console.log(`→ oracle: ${market.oracle.spot_source} · ETH $${market.oracle.eth_usd} · NEAR $${market.oracle.near_usd}`);

  let settle = null;
  if (evm) {
    const b = await evm.balances();
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
  };

  console.log(`→ enclave run (epoch ${epoch})`);
  const { output, attestationRef, callId, timeMs } = await enclave.run(input);
  console.log(`  done in ${timeMs}ms · call ${callId}`);
  const sealed = await unseal(output);

  if (output.settlement) {
    console.log(`→ settle cross-chain value into NAV: ${(Number(output.settlement.external_value_yocto) / 1e24).toFixed(4)} NEAR @ evm block ${output.settlement.evm_block}`);
    await vault.settlePnl(output.settlement.external_value_yocto, output.settlement.evm_block, attestationRef);
  }

  console.log("→ commit on vault (before execution) + publish the exact enclave input");
  const id = await vault.commitDecision(output.commitment, APPROVED_WASM_HASH, attestationRef, JSON.stringify(input));
  console.log(`  committed as #${id} · input sha256 ${crypto.createHash("sha256").update(JSON.stringify(input)).digest("hex").slice(0, 16)}…`);

  const decision = JSON.parse(sealed.decision_json);
  let execRef = null;
  if (evm && decision.action !== "hold") {
    console.log(`→ execute ${decision.action} as a real Uniswap v3 swap via Chain Signatures`);
    const r = await evm.executeTrade({ commitment: output.commitment, decision, ethUsd: market.oracle.eth_usd });
    for (const t of r.txs) console.log(`  ${t.step}: ${t.status} ${t.hash}`);
    console.log(`  ${r.detail}`);
    execRef = r.ref;
  } else {
    console.log(`→ ${decision.action}${evm ? "" : " [simulated fill — pass --execute evm for a real swap]"}`);
  }

  console.log("→ reveal on vault (guardrails enforced by the contract)");
  await vault.executeDecision(id, sealed.decision_json, sealed.salt_hex);
  if (execRef) await vault.recordExecutionRef(id, execRef);

  if (output.risk_bundle) {
    await vault.postRiskProof(epoch, JSON.stringify(output.risk_bundle));
    console.log("→ Bulletproof risk bundle anchored");
  }
  const after = onChain ? await vault.view("get_summary") : await vault.view();
  console.log("\ncycle complete:", JSON.stringify({ commitments: after.commitments, risk_proofs: after.risk_proofs, nav_yocto: after.nav_yocto }, null, 0));
}

const everyMin = Number(arg("every", "0"));
do {
  try { await cycle(); } catch (e) { console.error("cycle failed:", e.message); if (!everyMin) process.exit(1); }
  if (everyMin > 0) {
    console.log(`\n(sleeping ${everyMin} min — ctrl-c to stop)\n`);
    await new Promise((r) => setTimeout(r, everyMin * 60_000));
  }
} while (everyMin > 0);
