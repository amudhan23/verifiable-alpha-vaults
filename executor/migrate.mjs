#!/usr/bin/env node
// Atomic upgrade: deploy new code + call migrate() in ONE transaction.
// If migrate() panics, the deploy reverts too — the vault can't be bricked.
// Only possible while the vault account still has a full-access key; after
// it is deleted, upgrades go through propose_code / apply_code (48h).
import { readFileSync } from "node:fs";
import { connect, keyStores, KeyPair, transactions } from "near-api-js";

const masterId = process.env.NEAR_ACCOUNT_ID;
const contractId = process.env.VAULT_CONTRACT_ID ?? `vault.${masterId}`;

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
    transactions.functionCall("migrate", {}, 100_000_000_000_000n, 0n),
  ],
});
console.log("upgrade tx:", res.transaction.hash);
const summary = await account.viewFunction({ contractId, methodName: "get_summary" });
console.log("version:", summary.version, "| mode:", summary.mode, "| commitments:", summary.commitments, "| nav:", summary.nav_yocto,
  "| pending strategy:", summary.pending_strategy?.hash?.slice(0, 12), "| deposits open:", summary.deposits_open);
