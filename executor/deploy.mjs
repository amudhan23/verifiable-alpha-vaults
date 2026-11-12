#!/usr/bin/env node
// Deploy the vault to NEAR testnet in one command:
//   creates <prefix>.<master> as a subaccount, deploys vault_contract.wasm,
//   calls new() with the guardrails, prints the explorer + frontend links.
//
//   NEAR_ACCOUNT_ID=you.testnet NEAR_PRIVATE_KEY=ed25519:... \
//   node deploy.mjs [--prefix vault] [--wasm-hash <approved sha256>] [--fund 8]
//
// The same key is reused for the subaccount; owner = executor = master
// (fine for the demo; split roles for anything real).

import { readFileSync } from "node:fs";
import { connect, keyStores, KeyPair, utils } from "near-api-js";

const arg = (name, dflt) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > -1 ? process.argv[i + 1] : dflt;
};
const need = (k) => {
  const v = process.env[k];
  if (!v) { console.error(`missing env ${k}`); process.exit(1); }
  return v;
};

const masterId = need("NEAR_ACCOUNT_ID");
const privateKey = need("NEAR_PRIVATE_KEY");
const prefix = arg("prefix", "vault");
const contractId = `${prefix}.${masterId}`;
const fundNear = arg("fund", "8");
// placeholder until the OutLayer project exists; rotate later with
// approve_new_wasm_hash (the rotation is logged on-chain by design)
const wasmHash = arg("wasm-hash", process.env.APPROVED_WASM_HASH ?? readLocalHash());

function readLocalHash() {
  try {
    return readFileSync(new URL("../strategy.wasm.sha256", import.meta.url), "utf8").split(" ")[0].trim();
  } catch {
    console.error("no --wasm-hash, no APPROVED_WASM_HASH, no strategy.wasm.sha256 — build first or pass one");
    process.exit(1);
  }
}

const wasm = readFileSync(new URL("../target/wasm32-unknown-unknown/release/vault_contract.wasm", import.meta.url));

const keyStore = new keyStores.InMemoryKeyStore();
const keyPair = KeyPair.fromString(privateKey);
await keyStore.setKey("testnet", masterId, keyPair);
await keyStore.setKey("testnet", contractId, keyPair);
const near = await connect({
  networkId: "testnet",
  nodeUrl: process.env.NEAR_RPC ?? "https://rpc.testnet.near.org",
  keyStore,
});
const master = await near.account(masterId);

// 1. subaccount (skip if it exists)
let fresh = false;
try {
  await near.connection.provider.query({ request_type: "view_account", finality: "final", account_id: contractId });
  console.log(`✓ ${contractId} already exists — redeploying code`);
} catch {
  console.log(`→ creating ${contractId} with ${fundNear} NEAR`);
  await master.createAccount(contractId, keyPair.getPublicKey(), utils.format.parseNearAmount(fundNear));
  fresh = true;
}

// 2. deploy
const contract = await near.account(contractId);
console.log(`→ deploying vault_contract.wasm (${(wasm.length / 1024).toFixed(0)} KB)`);
await contract.deployContract(wasm);

// 3. init (only on a fresh account)
if (fresh) {
  console.log(`→ init: owner/executor=${masterId}, approved wasm ${wasmHash.slice(0, 12)}…`);
  await contract.functionCall({
    contractId,
    methodName: "new",
    args: {
      owner: masterId,
      executor: masterId,
      approved_wasm_hash: wasmHash,
      guardrails: {
        max_leverage_bps: 30_000,
        max_drawdown_bps: 2_000,
        max_position_bps: 5_000,
        allowed_pairs: ["ETH/USDC", "NEAR/USDC"],
      },
    },
    gas: 100_000_000_000_000n,
  });
}

const summary = await contract.viewFunction({ contractId, methodName: "get_summary" });
console.log("\nvault deployed ✓");
console.log(JSON.stringify(summary, null, 2));
console.log(`\nexplorer:  https://testnet.nearblocks.io/address/${contractId}`);
console.log(`frontend:  ?vault=${contractId}  (e.g. http://localhost:8080/?vault=${contractId})`);
console.log(`\nnext: VAULT_CONTRACT_ID=${contractId} node run.mjs --enclave outlayer --vault near`);
