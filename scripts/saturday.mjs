#!/usr/bin/env node
// Saturday go-live steps, one command each (run from executor/ so deps resolve):
//
//   node ../scripts/saturday.mjs status       what is pending, mode, keys on the vault account
//   node ../scripts/saturday.mjs apply        apply_strategy (anyone; fails before the timelock)
//   node run.mjs --enclave onchain --vault near --execute evm     one attested cycle with a trade
//   node ../scripts/saturday.mjs attest       enable_attested (manager; instant; going back takes 48h)
//   node ../scripts/saturday.mjs lock --yes-delete-vault-keys
//                                             delete every access key on the vault account.
//                                             IRREVERSIBLE: from then on code changes only via
//                                             propose_code -> 48h -> apply_code.
//
// env: NEAR_ACCOUNT_ID (manager/owner), NEAR_PRIVATE_KEY, VAULT_CONTRACT_ID, NEAR_RPC (optional)
import { connect, keyStores, KeyPair, transactions, utils } from "near-api-js";

const cmd = process.argv[2] ?? "status";
const owner = process.env.NEAR_ACCOUNT_ID;
const vaultId = process.env.VAULT_CONTRACT_ID ?? `vault.${owner}`;
const nodeUrl = process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com";
const kp = KeyPair.fromString(process.env.NEAR_PRIVATE_KEY);
const ks = new keyStores.InMemoryKeyStore();
await ks.setKey("testnet", owner, kp);
await ks.setKey("testnet", vaultId, kp);
const near = await connect({ networkId: "testnet", nodeUrl, keyStore: ks });
const ownerAcct = await near.account(owner);
const vaultAcct = await near.account(vaultId);
const view = (m, a = {}) => ownerAcct.viewFunction({ contractId: vaultId, methodName: m, args: a });
const call = (m, a = {}) => ownerAcct.functionCall({ contractId: vaultId, methodName: m, args: a, gas: 100_000_000_000_000n });

async function status() {
  const s = await view("get_summary");
  const keys = await vaultAcct.getAccessKeys();
  const now = Date.now();
  console.log(`vault ${vaultId} v${s.version} · mode ${s.mode} · approved ${s.approved_wasm_hash.slice(0, 12)}…`);
  if (s.pending_strategy) {
    const at = s.pending_strategy.effective_at_ms;
    console.log(`pending strategy ${s.pending_strategy.hash.slice(0, 12)}… · ${at <= now ? "APPLICABLE NOW" : `applicable in ${((at - now) / 3.6e6).toFixed(1)}h (${new Date(at).toISOString()})`}`);
  }
  if (s.pending_code) console.log(`pending code ${s.pending_code.hash.slice(0, 12)}… at ${new Date(s.pending_code.effective_at_ms).toISOString()}`);
  console.log(`commitments ${s.commitments} · proofs ${s.risk_proofs} · deposits ${s.deposits_open ? "open" : "closed"} · emergency ${s.emergency}`);
  console.log(`access keys on ${vaultId}: ${keys.length}${keys.length ? " (" + keys.map((k) => `${k.public_key.slice(0, 16)}… ${k.access_key.permission === "FullAccess" ? "FULL ACCESS" : "function-call"}`).join(", ") + ")" : " (locked: upgrades only via propose_code/apply_code)"}`);
  const bal = await ownerAcct.getAccountBalance();
  console.log(`${owner} balance ${utils.format.formatNearAmount(bal.available, 3)} NEAR`);
}

if (cmd === "status") await status();
else if (cmd === "apply") {
  const r = await call("apply_strategy");
  console.log("apply_strategy tx", r.transaction.hash);
  await status();
} else if (cmd === "attest") {
  const r = await call("enable_attested");
  console.log("enable_attested tx", r.transaction.hash);
  await status();
} else if (cmd === "lock") {
  if (!process.argv.includes("--yes-delete-vault-keys")) {
    console.error("refusing: this permanently removes every key on the vault account. Re-run with --yes-delete-vault-keys");
    process.exit(1);
  }
  const s = await view("get_summary");
  if (s.version !== "5.0.0") throw new Error(`unexpected contract version ${s.version}`);
  const keys = await vaultAcct.getAccessKeys();
  // delete other keys first, the signing key last (in the same transaction)
  const mine = kp.getPublicKey().toString();
  const order = [...keys.filter((k) => k.public_key !== mine), ...keys.filter((k) => k.public_key === mine)];
  const r = await vaultAcct.signAndSendTransaction({
    receiverId: vaultId,
    actions: order.map((k) => transactions.deleteKey(utils.PublicKey.from(k.public_key))),
  });
  console.log("deleted", order.length, "key(s) · tx", r.transaction.hash);
  await status();
} else {
  console.error(`unknown command ${cmd}`);
  process.exit(1);
}
