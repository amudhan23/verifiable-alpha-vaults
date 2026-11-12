// Live mode: read a deployed vault straight from NEAR JSON-RPC — no SDK, no
// build step, no server. Open index.html?vault=<contract-id>[&rpc=<url>].

const DEFAULT_RPC = "https://rpc.testnet.fastnear.com";

export class LiveVault {
  constructor(contractId, rpcUrl = DEFAULT_RPC) {
    this.contractId = contractId;
    this.rpc = rpcUrl;
  }

  async view(method, args = {}) {
    const res = await fetch(this.rpc, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "query",
        params: {
          request_type: "call_function",
          finality: "final",
          account_id: this.contractId,
          method_name: method,
          args_base64: btoa(JSON.stringify(args)),
        },
      }),
    });
    const body = await res.json();
    if (body.error) throw new Error(body.error.data ?? body.error.message);
    if (body.result.error) throw new Error(body.result.error);
    return JSON.parse(new TextDecoder().decode(new Uint8Array(body.result.result)));
  }

  /** Normalize on-chain state into the same shape the demo fixtures use. */
  async load() {
    const s = await this.view("get_summary");
    const [commitments, proofs] = await Promise.all([
      this.view("get_commitments", { from: 0, limit: 500 }),
      this.view("get_risk_proofs", { from: 0, limit: 200 }),
    ]);
    return {
      id: this.contractId,
      name: this.contractId.split(".")[0],
      manager: s.owner,
      live: true,
      approved_wasm_hash: s.approved_wasm_hash,
      guardrails: s.guardrails,
      pool_near: Number(BigInt(s.pool_yocto) / 10n ** 20n) / 10_000, // 4dp NEAR
      total_shares: s.total_shares,
      nav_history: [], // charted once epochs accrue; see render fallback
      return_pct: null,
      integrity_alert: s.integrity_alert,
      deposits_paused: s.deposits_paused,
      commitments,
      risk_proofs: proofs.map((p) => ({ ...p, bundle: JSON.parse(p.bundle_json) })),
    };
  }
}
