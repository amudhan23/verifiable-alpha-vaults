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

// Uniswap v3 on Ethereum Sepolia — each address verified on-chain (code
// present; WETH/USDC pools at 0.05/0.3/1% with liquidity).
export const UNI = {
  router: "0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E", // SwapRouter02
  quoter: "0xEd1f6473345F45b75F8179591dd5bA1888cf2FB3", // QuoterV2
  weth: "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14",
  usdc: "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
  fees: [500, 3000, 10000],
};
const SLIPPAGE_BPS = 200n; // 2%
// Refuse to trade into a pool whose price strays from the oracle by more than
// this — protects against manipulated / thin pools. Sepolia pools are wildly
// mispriced vs. real markets, so testnet runs need ALLOW_POOL_DEVIATION=1;
// the deviation is then still measured and recorded with the trade.
const MAX_POOL_DEVIATION = 0.05;


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
      this.pub = publicClient;
      this.viem = await import("viem");
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

  /** Sign (MPC) + broadcast one tx; wait for the receipt. Never throws on
   * revert — returns status so the vault can record the truth. */
  async sendTx({ to, data = "0x", value = 0n }) {
    await this.init();
    const from = await this.address();
    const { transaction, hashesToSign } = await this.evm.prepareTransactionForSigning({ from, to, value, data });
    const payloadHex = Array.from(hashesToSign[0], (b) => b.toString(16).padStart(2, "0")).join("");
    const res = await this.nearAccount.functionCall({
      contractId: MPC_TESTNET_CONTRACT,
      methodName: "sign",
      args: { request: { payload_v2: { Ecdsa: payloadHex }, path: this.cfg.derivationPath, domain_id: 0 } },
      gas: 300_000_000_000_000n,
      attachedDeposit: "1",
    });
    const raw = JSON.parse(Buffer.from(res.status.SuccessValue, "base64").toString());
    const signedTx = this.evm.finalizeTransactionSigning({ transaction, rsvSignatures: [this.toRSV(raw)] });
    const sent = await this.evm.broadcastTx(signedTx);
    const hash = typeof sent === "string" ? sent : sent.hash;
    try {
      const rcpt = await this.pub.waitForTransactionReceipt({ hash, timeout: 120_000 });
      return { hash, status: rcpt.status, block: rcpt.blockNumber };
    } catch {
      return { hash, status: "pending-timeout" };
    }
  }

  async balances() {
    await this.init();
    const { parseAbi } = this.viem;
    const erc20 = parseAbi(["function balanceOf(address) view returns (uint256)"]);
    const who = await this.address();
    const [eth, weth, usdc, block] = await Promise.all([
      this.pub.getBalance({ address: who }),
      this.pub.readContract({ address: UNI.weth, abi: erc20, functionName: "balanceOf", args: [who] }),
      this.pub.readContract({ address: UNI.usdc, abi: erc20, functionName: "balanceOf", args: [who] }),
      this.pub.getBlockNumber(),
    ]);
    return { eth, weth, usdc, block };
  }

  /** Best quote across fee tiers via QuoterV2 (eth_call). */
  async bestQuote(tokenIn, tokenOut, amountIn) {
    const { parseAbi } = this.viem;
    const q = parseAbi([
      "function quoteExactInputSingle((address tokenIn,address tokenOut,uint256 amountIn,uint24 fee,uint160 sqrtPriceLimitX96)) view returns (uint256 amountOut,uint160,uint32,uint256)",
    ]);
    let best = null;
    for (const fee of UNI.fees) {
      try {
        const [out] = await this.pub.readContract({
          address: UNI.quoter, abi: q, functionName: "quoteExactInputSingle",
          args: [{ tokenIn, tokenOut, amountIn, fee, sqrtPriceLimitX96: 0n }],
        });
        if (!best || out > best.out) best = { fee, out };
      } catch { /* tier without depth */ }
    }
    if (!best) throw new Error("no Uniswap pool can quote this trade");
    return best;
  }

  /**
   * Execute a decision as a REAL Uniswap v3 swap on Sepolia.
   *   sell ETH/USDC → WETH→USDC ;  buy ETH/USDC → USDC→WETH
   * The 32-byte NEAR commitment is appended after the ABI-encoded swap args
   * (Solidity ignores trailing calldata), so every swap tx is verifiably tied
   * to the commitment that preceded it.
   * @returns {{ref:string, txs:object[], filled:boolean, detail:string}}
   */
  async executeTrade({ commitment, decision, ethUsd, tradeEthWei = 2_000_000_000_000_000n }) {
    await this.init();
    const { parseAbi, encodeFunctionData } = this.viem;
    const who = await this.address();
    const erc20 = parseAbi([
      "function approve(address,uint256) returns (bool)",
      "function allowance(address,address) view returns (uint256)",
      "function deposit() payable",
    ]);
    const routerAbi = parseAbi([
      "function exactInputSingle((address tokenIn,address tokenOut,uint24 fee,address recipient,uint256 amountIn,uint256 amountOutMinimum,uint160 sqrtPriceLimitX96)) payable returns (uint256)",
    ]);
    const txs = [];
    const bal = await this.balances();
    let tokenIn, tokenOut, amountIn;

    if (decision.action === "sell") {
      tokenIn = UNI.weth; tokenOut = UNI.usdc; amountIn = tradeEthWei;
      if (bal.weth < amountIn) {
        // wrap just what's needed (keep native ETH for gas)
        const need = amountIn - bal.weth;
        txs.push({ step: "wrap", ...(await this.sendTx({ to: UNI.weth, value: need, data: encodeFunctionData({ abi: erc20, functionName: "deposit" }) })) });
      }
    } else if (decision.action === "buy") {
      tokenIn = UNI.usdc; tokenOut = UNI.weth;
      amountIn = bal.usdc / 2n; // deploy half the USDC per buy
      if (amountIn === 0n) return { ref: null, txs, filled: false, detail: "buy skipped: no USDC to deploy (already fully long ETH)" };
    } else {
      return { ref: null, txs, filled: false, detail: "hold" };
    }

    const allowance = await this.pub.readContract({ address: tokenIn, abi: erc20, functionName: "allowance", args: [who, UNI.router] });
    if (allowance < amountIn) {
      txs.push({ step: "approve", ...(await this.sendTx({ to: tokenIn, data: encodeFunctionData({ abi: erc20, functionName: "approve", args: [UNI.router, 2n ** 255n] }) })) });
    }

    const q = await this.bestQuote(tokenIn, tokenOut, amountIn);
    // implied ETH price from the pool vs. the oracle the enclave saw
    const poolEthUsd = tokenIn === UNI.weth
      ? (Number(q.out) / 1e6) / (Number(amountIn) / 1e18)
      : (Number(amountIn) / 1e6) / (Number(q.out) / 1e18);
    const deviation = ethUsd ? Math.abs(poolEthUsd / ethUsd - 1) : 0;
    const devNote = `pool ETH $${poolEthUsd.toFixed(0)} vs oracle $${ethUsd?.toFixed(0)} (${(deviation * 100).toFixed(1)}% off)`;
    if (deviation > MAX_POOL_DEVIATION && process.env.ALLOW_POOL_DEVIATION !== "1") {
      return { ref: `refused:${devNote}`, txs, filled: false, detail: `REFUSED: ${devNote} exceeds ${MAX_POOL_DEVIATION * 100}% guard` };
    }
    const minOut = (q.out * (10_000n - SLIPPAGE_BPS)) / 10_000n;
    const swapData =
      encodeFunctionData({
        abi: routerAbi, functionName: "exactInputSingle",
        args: [{ tokenIn, tokenOut, fee: q.fee, recipient: who, amountIn, amountOutMinimum: minOut, sqrtPriceLimitX96: 0n }],
      }) + commitment; // trailing 32B commitment
    const swap = await this.sendTx({ to: UNI.router, data: swapData });
    txs.push({ step: "swap", fee: q.fee, amountIn: amountIn.toString(), minOut: minOut.toString(), ...swap });

    const filled = swap.status === "success";
    return {
      ref: filled ? swap.hash : `reverted:${swap.hash}`,
      txs, filled,
      detail: `${decision.action} via Uniswap v3 ${q.fee / 10000}% pool, minOut ${minOut} (2% slippage guard) · ${devNote}${deviation > MAX_POOL_DEVIATION ? " [testnet override]" : ""}`,
    };
  }
}

// ---------------------------------------------------------------------------
// v4: the VAULT signs. The contract builds each tx (recipient locked to its
// own address, fixed targets, gas caps) and calls the MPC signer itself; the
// executor rebuilds the same tx locally, checks the hash matches what the
// vault signed, attaches the signature, and broadcasts.
// ---------------------------------------------------------------------------
// per-leg caps, mirrored from the v5 contract
const GAS = { wrap: 250_000n, approve: 250_000n, swap: 400_000n, exit: 120_000n };
const MAX_FEE_CAP = 100_000_000_000n;

/** The exact swap calldata the v5 contract builds: multicall(deadline, [exactInputSingle ‖ commitment]). */
export function swapCalldata(viem, { tokenIn, tokenOut, fee, recipient, amountIn, minOut, deadline, commitment }) {
  const routerAbi = viem.parseAbi([
    "function exactInputSingle((address tokenIn,address tokenOut,uint24 fee,address recipient,uint256 amountIn,uint256 amountOutMinimum,uint160 sqrtPriceLimitX96)) payable returns (uint256)",
    "function multicall(uint256 deadline, bytes[] data) payable returns (bytes[])",
  ]);
  const inner = viem.encodeFunctionData({ abi: routerAbi, functionName: "exactInputSingle",
    args: [{ tokenIn, tokenOut, fee, recipient, amountIn, amountOutMinimum: minOut, sqrtPriceLimitX96: 0n }] }) + commitment;
  return viem.encodeFunctionData({ abi: routerAbi, functionName: "multicall", args: [deadline, [inner]] });
}

export class VaultSignedExecutor {
  /** @param {{vault: import('./vault.mjs').NearVault, vaultId: string, mpcPath?: string}} cfg */
  constructor({ vault, vaultId, mpcPath = "alpha-vault-v1", rpcUrl = "https://ethereum-sepolia-rpc.publicnode.com" }) {
    this.vault = vault;
    this.vaultId = vaultId;
    this.mpcPath = mpcPath;
    this.rpcUrl = rpcUrl;
  }

  async init() {
    if (this.pub) return;
    this.viem = await import("viem");
    const { sepolia } = await import("viem/chains");
    this.pub = this.viem.createPublicClient({ chain: sepolia, transport: this.viem.http(this.rpcUrl) });
    const s = await this.vault.view("get_summary");
    this.address = s.evm_address;
  }

  async balances() {
    await this.init();
    const erc20 = this.viem.parseAbi(["function balanceOf(address) view returns (uint256)"]);
    const [eth, weth, usdc, block] = await Promise.all([
      this.pub.getBalance({ address: this.address }),
      this.pub.readContract({ address: UNI.weth, abi: erc20, functionName: "balanceOf", args: [this.address] }),
      this.pub.readContract({ address: UNI.usdc, abi: erc20, functionName: "balanceOf", args: [this.address] }),
      this.pub.getBlockNumber(),
    ]);
    return { eth, weth, usdc, block };
  }

  async bestQuote(tokenIn, tokenOut, amountIn) {
    return ChainSigExecutor.prototype.bestQuote.call(this, tokenIn, tokenOut, amountIn);
  }

  /** Ask the vault to sign one leg; verify; broadcast; wait. */
  async leg(id, leg, { to, value = 0n, data, deadline = 0n, method = "sign_evm_tx" }) {
    const { serializeTransaction, keccak256 } = this.viem;
    const nonce = await this.pub.getTransactionCount({ address: this.address, blockTag: "pending" });
    const fees = await this.pub.estimateFeesPerGas();
    let maxFeePerGas = fees.maxFeePerGas * 2n;
    if (maxFeePerGas > MAX_FEE_CAP) maxFeePerGas = MAX_FEE_CAP;
    let maxPriorityFeePerGas = fees.maxPriorityFeePerGas ?? 1_000_000_000n;
    if (maxPriorityFeePerGas > maxFeePerGas) maxPriorityFeePerGas = maxFeePerGas;
    // estimate per tx (+30%), fallback to defaults; capped at the vault's per-leg cap
    let gas = GAS[leg];
    try { gas = (await this.pub.estimateGas({ account: this.address, to, value, data })) * 13n / 10n; } catch { /* e.g. swap before approve is mined */ }
    if (gas > GAS[leg]) gas = GAS[leg];
    const tx = { type: "eip1559", chainId: 11155111, nonce, maxFeePerGas, maxPriorityFeePerGas, gas, to, value, data };
    const localHash = keccak256(serializeTransaction(tx)).slice(2);

    const fee = { nonce, max_fee_per_gas: maxFeePerGas.toString(), max_priority_fee_per_gas: maxPriorityFeePerGas.toString(), gas_limit: Number(gas) };
    const res = await this.vault.callRaw(method, method === "sign_evm_tx" ? { id, leg, ...fee, deadline_s: Number(deadline) } : fee, 240_000_000_000_000n, "5000000000000000000000"); // 0.005 NEAR: leg storage + MPC fee
    const logs = res.receipts_outcome.flatMap((r) => r.outcome.logs);
    const req = logs.map((l) => l.match(/"event":"evm_sign_requested".*"payload":"([0-9a-f]{64})"/)).find(Boolean);
    if (!req) throw new Error("vault did not log a sign request");
    if (req[1] !== localHash) throw new Error(`vault signed ${req[1]} but local tx hashes to ${localHash}: refusing to broadcast`);
    const sv = res.status?.SuccessValue;
    const sig = sv ? JSON.parse(Buffer.from(sv, "base64").toString()) : null;
    if (!sig) throw new Error("MPC signature failed (leg can be retried)");
    const bigR = sig.big_r.affine_point ?? sig.big_r;
    const r = `0x${bigR.slice(2)}`;
    const s = `0x${sig.s.scalar ?? sig.s}`;
    const raw = serializeTransaction(tx, { r, s, yParity: sig.recovery_id });
    const hash = await this.pub.sendRawTransaction({ serializedTransaction: raw });
    try {
      const rcpt = await this.pub.waitForTransactionReceipt({ hash, timeout: 120_000 });
      return { step: leg, hash, status: rcpt.status };
    } catch {
      return { step: leg, hash, status: "pending-timeout" };
    }
  }

  /**
   * Execute an executed (revealed + guardrail-passed) decision. v5: the size,
   * slippage floor and fee tier are the ENCLAVE's (inside the committed
   * decision); the executor only checks the pool can meet the floor before
   * spending gas, and broadcasts what the vault signs.
   */
  async executeTrade({ id, commitment, decision }) {
    await this.init();
    const { encodeFunctionData, parseAbi } = this.viem;
    const erc20 = parseAbi(["function approve(address,uint256) returns (bool)", "function allowance(address,address) view returns (uint256)", "function deposit() payable"]);
    const txs = [];
    if (decision.action !== "buy" && decision.action !== "sell") return { ref: null, txs, filled: false, detail: "hold" };
    const amountIn = BigInt(decision.amount_in || 0);
    const minOut = BigInt(decision.min_out || 0);
    const fee = Number(decision.fee_tier || 0);
    if (amountIn === 0n || minOut === 0n) return { ref: null, txs, filled: false, detail: "decision carries no trade size (pre-v2 strategy): nothing to sign" };
    const bal = await this.balances();
    const [tokenIn, tokenOut] = decision.action === "sell" ? [UNI.weth, UNI.usdc] : [UNI.usdc, UNI.weth];
    // the pool must be able to meet the enclave's floor, or the swap would revert and burn gas
    const quote = await this.quoteExact(tokenIn, tokenOut, amountIn, fee);
    if (quote < minOut) {
      return { ref: `refused:pool quote ${quote} < enclave floor ${minOut}`, txs, filled: false,
        detail: `REFUSED: pool would return ${quote}, below the enclave's slippage floor ${minOut} (Sepolia pools are mispriced vs the oracle)` };
    }
    if (decision.action === "sell" && bal.weth < amountIn) {
      txs.push(await this.leg(id, "wrap", { to: UNI.weth, value: amountIn, data: "0xd0e30db0" }));
    }
    const allowance = await this.pub.readContract({ address: tokenIn, abi: erc20, functionName: "allowance", args: [this.address, UNI.router] });
    if (allowance < amountIn) {
      txs.push(await this.leg(id, "approve", { to: tokenIn, data: encodeFunctionData({ abi: erc20, functionName: "approve", args: [UNI.router, 2n ** 256n - 1n] }) }));
    }
    const deadline = BigInt(Math.floor(Date.now() / 1000) + 1200);
    const data = swapCalldata(this.viem, { tokenIn, tokenOut, fee, recipient: this.address, amountIn, minOut, deadline, commitment });
    const swap = await this.leg(id, "swap", { to: UNI.router, data, deadline });
    txs.push(swap);
    const filled = swap.status === "success";
    return { ref: filled ? swap.hash : `reverted:${swap.hash}`, txs, filled,
      detail: `${decision.action} ${amountIn} via Uniswap v3 ${fee / 10000}% · enclave-sized, floor ${minOut} · vault-signed` };
  }

  async quoteExact(tokenIn, tokenOut, amountIn, fee) {
    const quoterAbi = this.viem.parseAbi(["function quoteExactInputSingle((address tokenIn,address tokenOut,uint256 amountIn,uint24 fee,uint160 sqrtPriceLimitX96)) returns (uint256 amountOut,uint160,uint32,uint256)"]);
    try {
      const { result } = await this.pub.simulateContract({ address: UNI.quoter, abi: quoterAbi, functionName: "quoteExactInputSingle",
        args: [{ tokenIn, tokenOut, amountIn, fee, sqrtPriceLimitX96: 0n }] });
      return result[0];
    } catch { return 0n; }
  }
}
