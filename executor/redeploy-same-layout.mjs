#!/usr/bin/env node
// Code-only redeploy for a fix that keeps the state layout (no migrate call).
// Only possible while the vault still has a full-access key.
import { readFileSync } from "node:fs";
import { connect, keyStores, KeyPair, transactions } from "near-api-js";
const contractId = process.env.VAULT_CONTRACT_ID ?? `vault.${process.env.NEAR_ACCOUNT_ID}`;
const keyStore = new keyStores.InMemoryKeyStore();
await keyStore.setKey("testnet", contractId, KeyPair.fromString(process.env.NEAR_PRIVATE_KEY));
const near = await connect({ networkId: "testnet", nodeUrl: process.env.NEAR_RPC ?? "https://rpc.testnet.near.org", keyStore });
const account = await near.account(contractId);
const code = readFileSync(new URL("../target/wasm32-unknown-unknown/release/vault_contract.wasm", import.meta.url));
const res = await account.signAndSendTransaction({ receiverId: contractId, actions: [transactions.deployContract(code)] });
console.log("redeploy tx:", res.transaction.hash);
const s = await account.viewFunction({ contractId, methodName: "get_summary" });
console.log("version", s.version, "| commitments", s.commitments, "| nav", s.nav_yocto);
