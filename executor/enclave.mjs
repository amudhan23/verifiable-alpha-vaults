import { netFor } from "./net.mjs";
// Enclave providers: where the sealed strategy actually runs.
//
// OutLayerEnclave — the real thing: OutLayer testnet HTTPS API, Intel TDX,
//   secrets decrypted only inside the enclave, attestation per run.
// LocalEnclave   — the same WASM logic run natively for offline demos/tests.
//   Identical stdin/stdout contract; attestation_ref marks it as simulated.

import { spawn } from "node:child_process";

/** @typedef {{commitment:string, risk_bundle:object|null, risk_error:string|null, sealed:{decision_json:string, salt_hex:string}}} RunOutput */

export class OutLayerEnclave {
  /**
   * @param {{baseUrl?:string, owner:string, project:string, paymentKey:string, secretsProfile?:string}} cfg
   */
  constructor(cfg) {
    this.base = cfg.baseUrl ?? netFor(cfg.owner ?? cfg.accountId ?? "x.testnet").outlayerApi;
    this.owner = cfg.owner;
    this.project = cfg.project;
    this.key = cfg.paymentKey;
    this.profile = cfg.secretsProfile ?? "default";
    // Pin the exact build the vault approves (OutLayer `version_key`), so a
    // project-level "active version" change cannot swap the code under us.
    this.versionKey = cfg.versionKey ?? null;
  }

  /** @param {object} input @returns {Promise<{output: RunOutput, attestationRef: string, callId: string, timeMs: number}>} */
  async run(input) {
    const res = await fetch(`${this.base}/call/${this.owner}/${this.project}`, {
      method: "POST",
      headers: { "X-Payment-Key": this.key, "Content-Type": "application/json" },
      body: JSON.stringify({
        input,
        secrets_ref: { profile: this.profile, account_id: this.owner },
        async: false,
        ...(this.versionKey ? { version_key: this.versionKey } : {}),
      }),
    });
    const body = await res.json();
    if (body.status !== "completed") {
      throw new Error(`OutLayer call ${body.call_id ?? "?"} ${body.status}: ${body.error ?? res.status}`);
    }
    // The code hash the vault records comes from the ATTESTATION (what the
    // TDX worker actually ran), never from executor config.
    let executedWasm = null;
    for (let i = 0; i < 6 && !executedWasm; i++) {
      try {
        const att = await this.attestation(body.call_id);
        executedWasm = att.executed_wasm_sha256 ?? att.wasm_hash ?? null;
      } catch { await new Promise((r) => setTimeout(r, 1500)); }
    }
    if (!executedWasm) throw new Error(`no attestation for call ${body.call_id}: refusing to commit without an attested code hash`);
    return {
      output: typeof body.output === "string" ? JSON.parse(body.output) : body.output,
      attestationRef: `${this.base}${body.attestation_url}`,
      callId: body.call_id,
      timeMs: body.time_ms,
      executedWasm,
    };
  }

  /** Fetch the TDX attestation for a finished call. */
  async attestation(callId) {
    const res = await fetch(`${this.base}/attestations/by-call/${callId}`, {
      headers: { "X-Payment-Key": this.key },
    });
    if (!res.ok) throw new Error(`attestation not ready (${res.status})`);
    return res.json();
  }
}

export class LocalEnclave {
  /** @param {{strategyParams: object, cwd?: string}} cfg */
  constructor(cfg) {
    this.params = JSON.stringify(cfg.strategyParams);
    this.cwd = cfg.cwd ?? new URL("..", import.meta.url).pathname;
  }

  /** @param {object} input @returns {Promise<{output: RunOutput, attestationRef: string, callId: string, timeMs: number}>} */
  async run(input) {
    const t0 = Date.now();
    const out = await new Promise((resolve, reject) => {
      const p = spawn("cargo", ["run", "-q", "-p", "vault-strategy"], {
        cwd: this.cwd,
        env: { ...process.env, STRATEGY_PARAMS: this.params },
        stdio: ["pipe", "pipe", "inherit"],
      });
      let stdout = "";
      p.stdout.on("data", (d) => (stdout += d));
      p.on("error", reject);
      p.on("close", (code) =>
        code === 0 ? resolve(stdout) : reject(new Error(`strategy exited ${code}`))
      );
      p.stdin.end(JSON.stringify(input));
    });
    return {
      output: JSON.parse(out),
      attestationRef: "local-sim (NOT attested — demo mode)",
      callId: `local-${Date.now()}`,
      timeMs: Date.now() - t0,
    };
  }

  async attestation() {
    return { note: "local simulation: no TDX quote. Use OutLayerEnclave for real attestations." };
  }
}
