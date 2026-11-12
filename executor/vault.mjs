// Vault providers: where commitments land.
//
// NearVault — the real contract on NEAR testnet (near-api-js).
// DryRunVault — in-memory stand-in that enforces the same ordering and hash
//   check, for offline demos and CI. Guardrail logic is NOT mirrored here:
//   the contract is the single source of truth for that (see its test suite).

import crypto from "node:crypto";

export class NearVault {
  /**
   * @param {{networkId?:string, nodeUrl?:string, contractId:string, accountId:string, privateKey:string}} cfg
   */
  constructor(cfg) {
    this.cfg = {
      networkId: "testnet",
      nodeUrl: "https://rpc.testnet.near.org",
      ...cfg,
    };
    this._account = null;
  }

  async account() {
    if (this._account) return this._account;
    const { connect, keyStores, KeyPair } = await import("near-api-js");
    const keyStore = new keyStores.InMemoryKeyStore();
    await keyStore.setKey(this.cfg.networkId, this.cfg.accountId, KeyPair.fromString(this.cfg.privateKey));
    const near = await connect({ ...this.cfg, keyStore });
    this._account = await near.account(this.cfg.accountId);
    return this._account;
  }

  async call(methodName, args, gas = 100_000_000_000_000n) {
    const account = await this.account();
    const res = await account.functionCall({
      contractId: this.cfg.contractId,
      methodName,
      args,
      gas,
    });
    return res;
  }

  async commitDecision(commitment, wasmHash, attestationRef, inputJson = null) {
    const res = await this.call("commit_decision", {
      commitment,
      wasm_hash: wasmHash,
      attestation_ref: attestationRef,
      input_json: inputJson,
    });
    const status = res.status?.SuccessValue;
    return status ? Number(JSON.parse(Buffer.from(status, "base64").toString())) : 0;
  }

  async executeDecision(id, decisionJson, saltHex) {
    await this.call("execute_decision", { id, decision_json: decisionJson, salt_hex: saltHex });
  }

  async postRiskProof(epoch, bundleJson) {
    await this.call("post_risk_proof", { epoch, bundle_json: bundleJson });
  }

  async settlePnl(externalValueYocto, evmBlock, attestationRef) {
    await this.call("settle_pnl", {
      external_value: externalValueYocto,
      evm_block: evmBlock,
      attestation_ref: attestationRef,
    });
  }

  async recordExecutionRef(id, executionRef) {
    await this.call("record_execution_ref", { id, execution_ref: executionRef });
  }

  async view(methodName, args = {}) {
    const account = await this.account();
    return account.viewFunction({ contractId: this.cfg.contractId, methodName, args });
  }
}

export class DryRunVault {
  constructor({ approvedWasmHash }) {
    this.approved = approvedWasmHash;
    this.commitments = [];
    this.proofs = [];
  }

  async commitDecision(commitment, wasmHash, attestationRef, inputJson = null) {
    if (wasmHash !== this.approved) {
      throw new Error("attested code hash does not match approved strategy; deposits paused");
    }
    const id = this.commitments.length;
    this.commitments.push({ id, commitment, wasmHash, attestationRef, status: "Committed", committedAt: Date.now() });
    return id;
  }

  async executeDecision(id, decisionJson, saltHex) {
    const c = this.commitments[id];
    if (!c) throw new Error("no such commitment: trade was never committed");
    if (c.status !== "Committed") throw new Error("commitment already settled");
    const digest = crypto
      .createHash("sha256")
      .update(Buffer.concat([Buffer.from(decisionJson), Buffer.from(saltHex, "hex")]))
      .digest("hex");
    if (digest !== c.commitment) {
      throw new Error("reveal does not match commitment: this trade was not committed when claimed");
    }
    c.status = "Executed";
    c.revealed = decisionJson;
    c.saltHex = saltHex;
  }

  async postRiskProof(epoch, bundleJson) {
    this.proofs.push({ epoch, bundleJson });
  }

  async settlePnl(externalValueYocto, evmBlock) {
    this.external = { externalValueYocto, evmBlock };
  }

  async recordExecutionRef(id, executionRef) {
    this.commitments[id].executionRef = executionRef;
  }

  async view() {
    return { commitments: this.commitments.length, proofs: this.proofs.length };
  }
}
