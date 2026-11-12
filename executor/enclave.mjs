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
    this.base = cfg.baseUrl ?? "https://testnet-api.outlayer.ai";
    this.owner = cfg.owner;
    this.project = cfg.project;
    this.key = cfg.paymentKey;
    this.profile = cfg.secretsProfile ?? "default";
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
      }),
    });
    const body = await res.json();
    if (body.status !== "completed") {
      throw new Error(`OutLayer call ${body.call_id ?? "?"} ${body.status}: ${body.error ?? res.status}`);
    }
    return {
      output: JSON.parse(body.output),
      attestationRef: `${this.base}${body.attestation_url}`,
      callId: body.call_id,
      timeMs: body.time_ms,
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
