// Offline tests for the executor: ordering + hash discipline in DryRunVault,
// and a full local-enclave cycle (skipped if cargo isn't available).
import assert from "node:assert/strict";
import { test } from "node:test";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { DryRunVault } from "./vault.mjs";
import { LocalEnclave } from "./enclave.mjs";

const HASH = "c3b8b92a101e575d1c0e2e502c1fc21443fa0e08e864e84fe467d5840bda9f77";
const commitmentOf = (dec, saltHex) =>
  crypto.createHash("sha256").update(Buffer.concat([Buffer.from(dec), Buffer.from(saltHex, "hex")])).digest("hex");

test("reveal must match the prior commitment", async () => {
  const v = new DryRunVault({ approvedWasmHash: HASH });
  const dec = `{"action":"buy"}`;
  const salt = "ab".repeat(32);
  const id = await v.commitDecision(commitmentOf(dec, salt), HASH, "x");
  await assert.rejects(
    () => v.executeDecision(id, `{"action":"sell"}`, salt),
    /was not committed when claimed/
  );
  await v.executeDecision(id, dec, salt); // honest reveal passes
});

test("execution without commitment is impossible", async () => {
  const v = new DryRunVault({ approvedWasmHash: HASH });
  await assert.rejects(() => v.executeDecision(0, "{}", "00"), /never committed/);
});

test("wrong attested code hash is refused", async () => {
  const v = new DryRunVault({ approvedWasmHash: HASH });
  await assert.rejects(
    () => v.commitDecision("a".repeat(64), "9d".repeat(32), "x"),
    /does not match approved strategy/
  );
});

const hasCargo = spawnSync("cargo", ["--version"]).status === 0;
test("full local-enclave cycle", { skip: !hasCargo }, async () => {
  const enclave = new LocalEnclave({
    strategyParams: { fast: 3, slow: 8, threshold_bps: 40, size_bps: 4000, leverage_bps: 25000 },
  });
  const vault = new DryRunVault({ approvedWasmHash: HASH });
  const { output } = await enclave.run({
    epoch: 1, nonce: 1, pair: "ETH/USDC",
    prices: [100, 100, 100, 100, 100, 102, 105, 109, 114, 120],
    nav_yocto: "990", peak_nav_yocto: "1000",
    max_leverage_bps: 30000, max_drawdown_bps: 2000,
  });
  assert.equal(output.commitment, commitmentOf(output.sealed.decision_json, output.sealed.salt_hex));
  assert.ok(output.risk_bundle, "honest params produce a risk proof");
  const id = await vault.commitDecision(output.commitment, HASH, "local");
  await vault.executeDecision(id, output.sealed.decision_json, output.sealed.salt_hex);
  assert.equal(vault.commitments[id].status, "Executed");
});
