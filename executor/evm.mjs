// Chain Signatures execution leg: the vault's NEAR account controls a derived
// EVM address via the MPC network (v1.signer-prod.testnet); trades execute on
// an EVM testnet and the tx hash is recorded back on the vault.
//
// Flow (chainsig.js v1.x):
//   deriveAddressAndPublicKey -> prepareTransactionForSigning
//     -> contract.sign (MPC) -> finalizeTransactionSigning -> broadcastTx
//
// MVP execution primitive: a transfer-with-calldata that anchors the decision
// commitment into the target-chain tx (provable link trade <-> commitment).
// A Uniswap v3 swap drops into `buildCalldata` without changing the flow.
// NEAR-native DEX swap remains the fallback if the EVM leg misbehaves live.

export const MPC_TESTNET_CONTRACT = "v1.signer-prod.testnet";

export class ChainSigExecutor {
  /**
   * @param {{accountId:string, privateKey:string, derivationPath?:string,
   *          networkId?:string, rpcUrl?:string}} cfg
   */
  constructor(cfg) {
    this.cfg = {
      networkId: "testnet",
      derivationPath: "alpha-vault-v1",
      rpcUrl: "https://ethereum-sepolia-rpc.publicnode.com",
      ...cfg,
    };
    this._ready = null;
  }

  async init() {
    if (this._ready) return this._ready;
    this._ready = (async () => {
      const [{ chainAdapters, contracts }, { KeyPair }, { createPublicClient, http }, { sepolia }] =
        await Promise.all([
          import("chainsig.js"),
          import("@near-js/crypto"),
          import("viem"),
          import("viem/chains"),
        ]);
      // docs show contracts.near.ChainSignatureContract; v1.1.16 exports it flat
      const ChainSignatureContract =
        contracts.near?.ChainSignatureContract ?? contracts.ChainSignatureContract;
      this.contract = new ChainSignatureContract({
        networkId: this.cfg.networkId,
        contractId: MPC_TESTNET_CONTRACT,
        fallbackRpcUrls: ["https://rpc.testnet.near.org"],
      });
      this.toRSV = (sig) =>
        "big_r" in sig
          ? { r: (sig.big_r.affine_point ?? sig.big_r).substring(2), s: sig.s.scalar ?? sig.s, v: sig.recovery_id + 27 }
          : sig;
      // NEAR account that pays for + authorizes the MPC sign call
      const { connect, keyStores } = await import("near-api-js");
      const keyStore = new keyStores.InMemoryKeyStore();
      await keyStore.setKey(this.cfg.networkId, this.cfg.accountId, KeyPair.fromString(this.cfg.privateKey));
      const near = await connect({ networkId: this.cfg.networkId, nodeUrl: "https://rpc.testnet.near.org", keyStore });
      this.nearAccount = await near.account(this.cfg.accountId);
      const publicClient = createPublicClient({ chain: sepolia, transport: http(this.cfg.rpcUrl) });
      this.evm = new chainAdapters.evm.EVM({ publicClient, contract: this.contract });
    })();
    return this._ready;
  }

  /** The vault's derived EVM address (fund it once with Sepolia ETH). */
  async address() {
    await this.init();
    const { address } = await this.evm.deriveAddressAndPublicKey(
      this.cfg.accountId,
      this.cfg.derivationPath
    );
    return address;
  }

  /**
   * Execute one decision on the EVM testnet. The commitment hash rides in
   * calldata so the on-chain trade is cryptographically tied to the NEAR
   * commitment that preceded it.
   * @param {{commitment: string, decision: object}} trade
   * @returns {Promise<string>} target-chain tx hash
   */
  async executeTrade({ commitment, decision }) {
    await this.init();
    const from = await this.address();
    const { transaction, hashesToSign } = await this.evm.prepareTransactionForSigning({
      from,
      to: from, // MVP: self-send; swap-router address once the DEX call lands
      value: 0n,
      data: this.buildCalldata(commitment, decision),
    });
    // request the MPC signature directly from v1.signer-prod.testnet
    const payloadHex = Array.from(hashesToSign[0], (b) => b.toString(16).padStart(2, "0")).join("");
    const res = await this.nearAccount.functionCall({
      contractId: MPC_TESTNET_CONTRACT,
      methodName: "sign",
      args: { request: { payload_v2: { Ecdsa: payloadHex }, path: this.cfg.derivationPath, domain_id: 0 } },
      gas: 300_000_000_000_000n,
      attachedDeposit: "1",
    });
    const raw = JSON.parse(Buffer.from(res.status.SuccessValue, "base64").toString());
    const signedTx = this.evm.finalizeTransactionSigning({
      transaction,
      rsvSignatures: [this.toRSV(raw)],
    });
    const txHash = await this.evm.broadcastTx(signedTx);
    return typeof txHash === "string" ? txHash : txHash.hash ?? JSON.stringify(txHash);
  }

  /** calldata = 0x | commitment (32B) — upgrade point for a real DEX call. */
  buildCalldata(commitment) {
    return `0x${commitment}`;
  }
}
