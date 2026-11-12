#!/usr/bin/env node
// One strategy cycle, end to end:
//
//   oracle prices -> enclave (sealed params) -> COMMIT hash on vault
//     -> execute trade (chain-sig leg; simulated fill in demo)
//     -> REVEAL decision+salt on vault (guardrails enforced there)
//     -> post Bulletproof risk bundle
//
// Modes:
//   node run.mjs --enclave local    --vault dry    # fully offline demo
//   node run.mjs --enclave outlayer --vault near   # testnet (needs env, see below)
//
// Env for testnet mode:
//   NEAR_ACCOUNT_ID, NEAR_PRIVATE_KEY, VAULT_CONTRACT_ID,
//   OUTLAYER_PAYMENT_KEY, OUTLAYER_PROJECT (default alpha-vault-strategy),
//   APPROVED_WASM_HASH

import { LocalEnclave, OutLayerEnclave } from "./enclave.mjs";
import { NearVault, DryRunVault } from "./vault.mjs";
import { ChainSigExecutor } from "./evm.mjs";

const arg = (name, dflt) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > -1 ? process.argv[i + 1] : dflt;
};
const APPROVED_WASM_HASH =
  process.env.APPROVED_WASM_HASH ?? "c3b8b92a101e575d1c0e2e502c1fc21443fa0e08e864e84fe467d5840bda9f77";

// --- oracle -----------------------------------------------------------------
// Demo: deterministic recent closes. Friday: live prices (Pyth/CoinGecko).
async function fetchPrices() {
  return [3000, 3002, 2998, 3004, 3010, 3025, 3042, 3060, 3081, 3105];
}

// --- wire up providers --------------------------------------------------------
const enclave =
  arg("enclave", "local") === "outlayer"
    ? new OutLayerEnclave({
        owner: need("NEAR_ACCOUNT_ID"),
        project: process.env.OUTLAYER_PROJECT ?? "alpha-vault-strategy",
        paymentKey: need("OUTLAYER_PAYMENT_KEY"),
      })
    : new LocalEnclave({
        // Demo-only params. On OutLayer these live as an encrypted secret the
        // enclave alone can read — they never appear in any config or log.
        strategyParams: { fast: 3, slow: 8, threshold_bps: 40, size_bps: 4000, leverage_bps: 25000 },
      });

const vault =
  arg("vault", "dry") === "near"
    ? new NearVault({
        contractId: need("VAULT_CONTRACT_ID"),
        accountId: need("NEAR_ACCOUNT_ID"),
        privateKey: need("NEAR_PRIVATE_KEY"),
      })
    : new DryRunVault({ approvedWasmHash: APPROVED_WASM_HASH });

function need(k) {
  const v = process.env[k];
  if (!v) { console.error(`missing env ${k}`); process.exit(1); }
  return v;
}

// --- cycle loop -----------------------------------------------------------------
const everyMin = Number(arg("every", "0"));
do {
  await cycle();
  if (everyMin > 0) {
    console.log(`\n(sleeping ${everyMin} min — ctrl-c to stop)\n`);
    await new Promise((r) => setTimeout(r, everyMin * 60_000));
  }
} while (everyMin > 0);

async function cycle() {
const epoch = Number(arg("epoch", `${Math.floor(Date.now() / 3_600_000)}`));
const input = {
  epoch,
  nonce: Date.now(),
  pair: "ETH/USDC",
  prices: await fetchPrices(),
  nav_yocto: "100000000",
  peak_nav_yocto: "100000000",
  max_leverage_bps: 30000,
  max_drawdown_bps: 2000,
};

console.log(`→ enclave run (epoch ${epoch})`);
const { output, attestationRef, callId, timeMs } = await enclave.run(input);
console.log(`  done in ${timeMs}ms · call ${callId}`);
console.log(`  commitment ${output.commitment.slice(0, 16)}… · risk proof: ${output.risk_bundle ? "yes" : `NO (${output.risk_error})`}`);

console.log("→ commit on vault (before execution — this is what kills back-filling)");
const id = await vault.commitDecision(output.commitment, APPROVED_WASM_HASH, attestationRef);
console.log(`  committed as #${id}`);

const decision = JSON.parse(output.sealed.decision_json);
let executionRef = null;
if (decision.action === "hold") {
  console.log("→ decision is hold; revealing without execution");
} else if (arg("execute", "sim") === "evm") {
  // Chain Signatures: the vault's NEAR account signs via the MPC network and
  // the trade lands on Sepolia with the commitment hash in calldata.
  const signer = new ChainSigExecutor({
    accountId: need("NEAR_ACCOUNT_ID"),
    privateKey: need("NEAR_PRIVATE_KEY"),
  });
  console.log(`→ execute via Chain Signatures from ${await signer.address()}`);
  executionRef = await signer.executeTrade({ commitment: output.commitment, decision });
  console.log(`  broadcast ✓ ${executionRef}`);
} else {
  console.log(`→ execute: ${decision.action} ${decision.pair} (${decision.leverage_bps / 10000}x, ${decision.size_bps / 100}% NAV) [simulated fill — pass --execute evm for Chain Signatures]`);
}

console.log("→ reveal on vault (guardrails enforced by the contract now)");
await vault.executeDecision(id, output.sealed.decision_json, output.sealed.salt_hex);
console.log("  revealed ✓ (hash matched, guardrails passed)");
if (executionRef) {
  await vault.recordExecutionRef(id, executionRef);
  console.log("  target-chain tx recorded on the vault");
}

if (output.risk_bundle) {
  console.log("→ post Bulletproof risk bundle");
  await vault.postRiskProof(epoch, JSON.stringify(output.risk_bundle));
  console.log("  posted ✓");
}

console.log("\ncycle complete:", JSON.stringify(await vault.view(), null, 2));
}
