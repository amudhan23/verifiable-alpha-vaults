// Live mode: read a deployed vault straight from NEAR JSON-RPC — no SDK, no
// build step, no server. Open index.html?vault=<contract-id>[&rpc=<url>].

export const isTestnet = (id) => id.endsWith(".testnet");
/** Same rule as the contract: a .testnet vault trades on Sepolia, a mainnet vault on Base. */
export const netOf = (id) => (isTestnet(id)
  ? { near: "testnet", rpc: "https://rpc.testnet.near.org", evm: "Sepolia", evmRpc: "https://ethereum-sepolia-rpc.publicnode.com", explorer: "https://sepolia.etherscan.io", usdc: "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238", intentsUsdc: "nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near" }
  : { near: "mainnet", rpc: "https://rpc.mainnet.near.org", evm: "Base", evmRpc: "https://base-rpc.publicnode.com", explorer: "https://basescan.org", usdc: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", intentsUsdc: "nep141:base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near" });

export class LiveVault {
  constructor(contractId, rpcUrl) {
    rpcUrl = rpcUrl ?? netOf(contractId).rpc;
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
    const [commitments, proofs, navLog] = await Promise.all([
      this.view("get_commitments", { from: 0, limit: 500 }),
      this.view("get_risk_proofs", { from: 0, limit: 200 }),
      this.view("get_nav_history", { from: 0, limit: 1000 }).catch(() => []),
    ]);
    const toNear = (yocto) => Number(BigInt(yocto) / 10n ** 20n) / 10_000;
    const navHistory = navLog.map((p) => ({ t: p.t, nav: toNear(p.pool_yocto) }));
    // Return from NAV per SHARE (first deposit mints 1:1, so NAV/share starts
    // at 1.0). Seed capital and deposits move NAV but never the share price.
    const nav = BigInt(s.nav_yocto ?? s.pool_yocto);
    const shares = BigInt(s.total_shares);
    const returnPct = shares > 0n ? (Number((nav * 1_000_000n) / shares) / 1_000_000 - 1) * 100 : null;
    return {
      id: this.contractId,
      name: this.contractId,
      manager: s.manager ?? s.owner,
      live: true,
      approved_wasm_hash: s.approved_wasm_hash,
      guardrails: s.guardrails,
      pool_near: toNear(s.nav_yocto ?? s.pool_yocto),
      local_near: toNear(s.pool_yocto),
      external_near: toNear(s.external_value_yocto ?? "0"),
      evm_address: s.evm_address,
      exit: s.exit ?? null,
      version: s.version,
      last_settlement_ref: s.last_settlement_ref,
      last_attested_ms: s.last_attested_ms,
      liveness_timeout_ms: s.liveness_timeout_ms,
      emergency: s.emergency,
      total_shares: s.total_shares,
      nav_history: navHistory,
      return_pct: returnPct,
      integrity_alert: s.integrity_alert,
      deposits_paused: s.deposits_paused,
      mode: s.mode ?? null,
      deposits_open: s.deposits_open ?? !s.deposits_paused,
      pending_strategy: s.pending_strategy ?? null,
      executor: s.executor,
      commitments,
      risk_proofs: proofs.map((p) => ({ ...p, bundle: JSON.parse(p.bundle_json) })),
    };
  }
}
