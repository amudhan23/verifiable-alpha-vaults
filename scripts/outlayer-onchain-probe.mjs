#!/usr/bin/env node
// Direct on-chain OutLayer request_execution (yield/resume) against the strategy project.
// Usage: node scripts/outlayer-onchain-probe.mjs <input.json>   (run from executor/ for deps)
// Lesson: Bulletproof generation needs ~2.3e9 instructions; the default budget (1e9) aborts it.
import { connect, keyStores, KeyPair } from "near-api-js";
import fs from "fs";
const c = JSON.parse(fs.readFileSync(process.env.CREDS ?? `${process.env.HOME}/.outlayer/testnet/credentials.json`));
const ks = new keyStores.InMemoryKeyStore();
await ks.setKey("testnet", c.account_id, KeyPair.fromString(c.private_key));
const near = await connect({ networkId: "testnet", nodeUrl: "https://test.rpc.fastnear.com", keyStore: ks });
const acct = await near.account(c.account_id);
const input = JSON.parse(fs.readFileSync(process.argv[2]));
const args = {
  source: { Project: { project_id: "alpha-vaults-23.testnet/alpha-vault-strategy", version_key: "f64b653707a92c9b80a80e32eb2ba2ecd1bc939fc6749ccd4069cce3863631cd" } },
  resource_limits: { max_instructions: Number(process.env.MI ?? 5_000_000_000), max_memory_mb: 256, max_execution_seconds: 60 },
  input_data: JSON.stringify(input),
  secrets_ref: { profile: "default", account_id: "alpha-vaults-23.testnet" },
  response_format: "Json",
};
const t0 = Date.now();
const r = await acct.functionCall({ contractId: "outlayer.testnet", methodName: "request_execution", args, gas: 300_000_000_000_000n, attachedDeposit: BigInt(process.env.DEP ?? "7100000000000000000000") });
console.log("tx", r.transaction.hash, "secs", (Date.now() - t0) / 1000);
const logs = r.receipts_outcome.flatMap(o => o.outcome.logs);
for (const l of logs) if (/execution_completed|Execution failed|charged/.test(l)) console.log(l.slice(0, 700));
const sv = r.status?.SuccessValue; console.log("result:", sv ? Buffer.from(sv, "base64").toString().slice(0, 900) : JSON.stringify(r.status).slice(0, 300));
