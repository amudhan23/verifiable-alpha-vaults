// Smoke-test the wasm-bindgen verifier the way the browser uses it: load
// frontend/pkg, verify every REAL bundle in the demo fixtures, and confirm a
// tampered bundle fails. Run by CI after the wasm build; also works locally.
import { readFileSync } from "node:fs";

const pkg = await import("../frontend/pkg/vault_proofs.js");
const wasmBytes = readFileSync(new URL("../frontend/pkg/vault_proofs_bg.wasm", import.meta.url));
await pkg.default({ module_or_path: wasmBytes });

const fixtures = JSON.parse(readFileSync(new URL("../frontend/demo/fixtures.json", import.meta.url)));
let verified = 0;
for (const v of fixtures.vaults) {
  for (const p of v.risk_proofs) {
    const res = pkg.verify_risk_bundle_json(
      JSON.stringify(p.bundle),
      BigInt(v.guardrails.max_leverage_bps),
      BigInt(v.guardrails.max_drawdown_bps)
    );
    if (res !== "ok") {
      console.error(`FAIL: ${v.name} epoch ${p.epoch}: ${res}`);
      process.exit(1);
    }
    verified++;
  }
}
if (verified === 0) {
  console.error("FAIL: no bundles found to verify");
  process.exit(1);
}

// tampered bundle must fail
const v = fixtures.vaults.find((x) => x.risk_proofs.length > 0);
const bad = structuredClone(v.risk_proofs[0].bundle);
bad.leverage.commitment = bad.leverage.commitment.replace(/^../, (h) => (h === "00" ? "01" : "00"));
const res = pkg.verify_risk_bundle_json(
  JSON.stringify(bad),
  BigInt(v.guardrails.max_leverage_bps),
  BigInt(v.guardrails.max_drawdown_bps)
);
if (res === "ok") {
  console.error("FAIL: tampered bundle verified");
  process.exit(1);
}

console.log(`OK: ${verified} real Bulletproof bundles verified via wasm; tampered bundle rejected ("${res.slice(0, 60)}…")`);
