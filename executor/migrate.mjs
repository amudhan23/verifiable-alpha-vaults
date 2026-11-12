#!/usr/bin/env node
// Atomic upgrade: deploy new code + call migrate() in ONE transaction.
// If migrate() panics, the deploy reverts too — the vault can't be bricked.
import { readFileSync } from "node:fs";
import { connect, keyStores, KeyPair, transactions } from "near-api-js";

const masterId = process.env.NEAR_ACCOUNT_ID;
const contractId = process.env.VAULT_CONTRACT_ID ?? `vault.${masterId}`;
const wasmUrl = process.argv[2];
if (!wasmUrl) { console.error("usage: node migrate.mjs <strategy_wasm_url>"); process.exit(1); }

const keyStore = new keyStores.InMemoryKeyStore();
const kp = KeyPair.fromString(process.env.NEAR_PRIVATE_KEY);
await keyStore.setKey("testnet", contractId, kp);
const near = await connect({ networkId: "testnet", nodeUrl: process.env.NEAR_RPC ?? "https://rpc.testnet.near.org", keyStore });
const account = await near.account(contractId);
const code = readFileSync(new URL("../target/wasm32-unknown-unknown/release/vault_contract.wasm", import.meta.url));

const res = await account.signAndSendTransaction({
  receiverId: contractId,
  actions: [
    transactions.deployContract(code),
    transactions.functionCall("migrate", { strategy_wasm_url: wasmUrl }, 100_000_000_000_000n, 0n),
  ],
});
console.log("upgrade tx:", res.transaction.hash);
const summary = await account.viewFunction({ contractId, methodName: "get_summary" });
console.log("commitments:", summary.commitments, "| nav:", summary.nav_yocto, "| wasm url set:", !!summary.strategy_wasm_url);
