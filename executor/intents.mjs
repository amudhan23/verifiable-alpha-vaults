#!/usr/bin/env node
// The way back: the vault's cross-chain value returns as NEAR via NEAR Intents (1Click).
//
//   node intents.mjs quote [--amount <usdc 6dp>] [--recipient <mainnet account>]
//        Live dry-run quote: the vault wallet's USDC on Ethereum -> NEAR, delivered to the vault.
//   node intents.mjs propose            (mainnet vaults) real quote -> vault.propose_exit (public 24h)
//   node intents.mjs send               (mainnet vaults) after 24h: the vault signs ONE USDC transfer
//                                        to the quote's deposit address; we verify and broadcast it
//   node intents.mjs status             1Click status of the pending exit's deposit address
//   node intents.mjs claim              anyone: vault.unwrap_and_repatriate (wNEAR -> NEAR -> holders)
//   node intents.mjs testnet-delivery <near>
//        TESTNET STAND-IN for the Intents delivery (NEAR Intents has no testnet): wrap NEAR on
//        wrap.testnet and ft_transfer it to the vault, exactly as 1Click delivers wNEAR on mainnet.
//
// env: NEAR_ACCOUNT_ID, NEAR_PRIVATE_KEY, VAULT_CONTRACT_ID, NEAR_RPC, ONECLICK_JWT (optional; avoids the fee)
import { NearVault } from "./vault.mjs";
import { VaultSignedExecutor } from "./evm.mjs";

const ONECLICK = process.env.ONECLICK_URL ?? "https://1click.chaindefuser.com";
// Ethereum mainnet USDC and NEAR (wNEAR) as 1Click asset ids
const ETH_USDC = "nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near";
const WNEAR = "nep141:wrap.near";

const cmd = process.argv[2] ?? "quote";
const arg = (k, d) => { const i = process.argv.indexOf(`--${k}`); return i > 0 ? process.argv[i + 1] : d; };
const vaultId = process.env.VAULT_CONTRACT_ID;
const mainnet = vaultId?.endsWith(".near");
const vault = new NearVault({ contractId: vaultId, accountId: process.env.NEAR_ACCOUNT_ID, privateKey: process.env.NEAR_PRIVATE_KEY,
  nodeUrl: process.env.NEAR_RPC ?? (mainnet ? "https://rpc.mainnet.fastnear.com" : "https://test.rpc.fastnear.com"), networkId: mainnet ? "mainnet" : "testnet" });

/** POST /v0/quote. `recipient` must be an existing NEAR mainnet account. */
export async function quote({ amountUsdc6, recipient, refundTo, dry = true }) {
  const res = await fetch(`${ONECLICK}/v0/quote`, {
    method: "POST",
    headers: { "content-type": "application/json", ...(process.env.ONECLICK_JWT ? { authorization: `Bearer ${process.env.ONECLICK_JWT}` } : {}) },
    body: JSON.stringify({
      dry, swapType: "EXACT_INPUT", slippageTolerance: 100,
      originAsset: ETH_USDC, depositType: "ORIGIN_CHAIN",
      destinationAsset: WNEAR, amount: String(amountUsdc6),
      refundTo, refundType: "ORIGIN_CHAIN", // a failed swap refunds to the vault's own EVM wallet
      recipient, recipientType: "DESTINATION_CHAIN",
      deadline: new Date(Date.now() + (dry ? 3600e3 : 26 * 3600e3)).toISOString(), // real quotes outlive the 24h exit delay
    }),
  });
  const j = await res.json();
  if (!res.ok) throw new Error(`1Click ${res.status}: ${j.message ?? JSON.stringify(j)}`);
  return j;
}

async function main() {
  const s = await vault.view("get_summary");
  const evm = new VaultSignedExecutor({ vault, vaultId });
  if (cmd === "quote") {
    let amount = arg("amount");
    if (!amount) { const b = await evm.balances(); amount = b.usdc.toString(); }
    // on testnet the vault has no mainnet account to receive; price the same route to a stand-in recipient
    const recipient = arg("recipient", mainnet ? vaultId : "wrap.near");
    const q = await quote({ amountUsdc6: amount, recipient, refundTo: s.evm_address });
    console.log(`NEAR Intents (1Click), live dry-run quote:`);
    console.log(`  ${q.quote.amountInFormatted} USDC on Ethereum  ->  ${Number(q.quote.amountOutFormatted).toFixed(4)} NEAR (min ${(Number(q.quote.minAmountOut) / 1e24).toFixed(4)}) to ${recipient}`);
    console.log(`  about ${q.quote.timeEstimate}s; refunds to the vault's own EVM wallet ${s.evm_address}`);
    if (!mainnet) console.log(`  (testnet vault: priced with Ethereum-mainnet USDC to a stand-in recipient, since NEAR Intents has no testnet)`);
  } else if (cmd === "propose") {
    if (!mainnet) throw new Error("NEAR Intents is mainnet-only: propose_exit with a real deposit address needs a mainnet vault. Use `quote` and `testnet-delivery` on testnet.");
    const b = await evm.balances();
    const q = await quote({ amountUsdc6: b.usdc.toString(), recipient: vaultId, refundTo: s.evm_address, dry: false });
    if (q.quoteRequest.recipient !== vaultId) throw new Error("quote recipient is not this vault");
    const r = await vault.callRaw("propose_exit", { deposit_address: q.quote.depositAddress, amount_usdc: b.usdc.toString(), quote_ref: `1click:${q.correlationId ?? q.quote.depositAddress}` },
      100_000_000_000_000n, "10000000000000000000000");
    console.log(`exit proposed (tx ${r.transaction.hash}): ${q.quote.amountInFormatted} USDC -> ~${q.quote.amountOutFormatted} NEAR; signable in 24h.`);
    console.log(`anyone can check: ${ONECLICK}/v0/status?depositAddress=${q.quote.depositAddress}`);
  } else if (cmd === "send") {
    const e = s.exit;
    if (!e) throw new Error("no exit proposed");
    await evm.init();
    const erc20 = evm.viem.parseAbi(["function transfer(address,uint256) returns (bool)"]);
    const data = evm.viem.encodeFunctionData({ abi: erc20, functionName: "transfer", args: [e.deposit_address, BigInt(e.amount_usdc)] });
    const usdc = mainnet ? "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48" : "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238";
    const t = await evm.leg(0, "exit", { to: usdc, data, method: "sign_exit_tx" });
    console.log(`exit transfer ${t.status}: ${t.hash}. Track: node intents.mjs status`);
  } else if (cmd === "status") {
    if (!s.exit) throw new Error("no exit proposed");
    const j = await (await fetch(`${ONECLICK}/v0/status?depositAddress=${s.exit.deposit_address}`)).json();
    console.log(JSON.stringify({ status: j.status, recipient: j.quoteResponse?.quoteRequest?.recipient, details: j.swapDetails }, null, 2));
    if (j.status === "SUCCESS") console.log("delivered: run `node intents.mjs claim` to unwrap and credit holders");
  } else if (cmd === "claim") {
    const r = await vault.callRaw("unwrap_and_repatriate", {}, 120_000_000_000_000n, "0");
    const logs = r.receipts_outcome.flatMap((o) => o.outcome.logs).filter((l) => /repatriated|unwrap_/.test(l));
    console.log(`tx ${r.transaction.hash}`); logs.forEach((l) => console.log("  " + l));
  } else if (cmd === "testnet-delivery") {
    if (mainnet) throw new Error("testnet only");
    const near = arg("near", /^[0-9.]+$/.test(process.argv[3] ?? "") ? process.argv[3] : "0.05");
    const yocto = BigInt(Math.round(Number(near) * 1e6)) * 10n ** 18n;
    const acct = await vault.account();
    const wrap = "wrap.testnet";
    await acct.functionCall({ contractId: wrap, methodName: "storage_deposit", args: { account_id: vaultId, registration_only: true }, gas: 30_000_000_000_000n, attachedDeposit: 1_250_000_000_000_000_000_000n });
    if (!process.argv.includes("--resend")) await acct.functionCall({ contractId: wrap, methodName: "near_deposit", args: {}, gas: 30_000_000_000_000n, attachedDeposit: yocto });
    // wrap.testnet keeps a storage fee on first deposit: send what we actually hold
    const held = await acct.viewFunction({ contractId: wrap, methodName: "ft_balance_of", args: { account_id: process.env.NEAR_ACCOUNT_ID } });
    const r = await acct.functionCall({ contractId: wrap, methodName: "ft_transfer", args: { receiver_id: vaultId, amount: held, memo: "testnet stand-in for a NEAR Intents delivery" }, gas: 30_000_000_000_000n, attachedDeposit: 1n });
    console.log(`delivered ${(Number(BigInt(held) / 10n ** 18n) / 1e6).toFixed(4)} wNEAR to ${vaultId} (tx ${r.transaction.hash}), as 1Click would on mainnet. Now: node intents.mjs claim`);
  } else throw new Error(`unknown command ${cmd}`);
}
main().catch((e) => { console.error(e.message); process.exit(1); });
