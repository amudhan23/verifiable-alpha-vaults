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
        accountId: this.cfg.accountId,
        keypair: KeyPair.fromString(this.cfg.privateKey),
      });
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
    const signature = await this.contract.sign({
      payload: hashesToSign[0].payload,
      path: this.cfg.derivationPath,
      key_version: 0,
    });
    const signedTx = this.evm.finalizeTransactionSigning({
      transaction,
      rsvSignatures: [signature],
    });
    const txHash = await this.evm.broadcastTx(signedTx);
    return typeof txHash === "string" ? txHash : txHash.hash ?? JSON.stringify(txHash);
  }

  /** calldata = 0x | commitment (32B) — upgrade point for a real DEX call. */
  buildCalldata(commitment) {
    return `0x${commitment}`;
  }
}
