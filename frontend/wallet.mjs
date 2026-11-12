// Wallet connection + vault actions. The 2 MB wallet-selector bundle loads
// lazily: only when a live vault page opens or someone clicks "Connect".
//
// Every action is an ordinary transaction to the vault contract, signed in
// the user's own wallet. The frontend never holds keys.

const RPC = "https://rpc.testnet.near.org";
const YOCTO = 10n ** 24n;
const GAS = "100000000000000"; // 100 Tgas

let state = { selector: null, modal: null, accountId: null, contractId: null };
const listeners = new Set();
export const onWalletChange = (fn) => listeners.add(fn);
const notify = () => listeners.forEach((fn) => fn(state.accountId));

function loadCss() {
  if (document.getElementById("wallet-css")) return;
  const l = document.createElement("link");
  l.id = "wallet-css"; l.rel = "stylesheet"; l.href = "./vendor/wallet-modal.css";
  document.head.appendChild(l);
}

export async function ensureWallet(contractId) {
  if (state.selector && state.contractId === contractId) return state;
  loadCss();
  const { initWallet } = await import("./vendor/wallet.js");
  const { selector, modal } = await initWallet(contractId);
  state = { selector, modal, contractId, accountId: null };
  const sync = () => {
    const acc = selector.store.getState().accounts.find((a) => a.active);
    const id = acc?.accountId ?? null;
    if (id !== state.accountId) { state.accountId = id; notify(); }
  };
  selector.store.observable.subscribe(sync);
  sync();
  return state;
}

export const currentAccount = () => state.accountId;

export async function connect(contractId) {
  const s = await ensureWallet(contractId);
  s.modal.show();
}

export async function disconnect() {
  if (!state.selector) return;
  const w = await state.selector.wallet();
  await w.signOut();
  state.accountId = null;
  notify();
}

async function call(methodName, args, depositYocto = "0") {
  const w = await state.selector.wallet();
  return w.signAndSendTransaction({
    receiverId: state.contractId,
    actions: [{ type: "FunctionCall", params: { methodName, args, gas: GAS, deposit: depositYocto } }],
  });
}

/** "1.5" NEAR -> yocto string, exact (no floats). */
export function nearToYocto(s) {
  const [w, f = ""] = String(s).trim().split(".");
  if (!/^\d+$/.test(w || "0") || !/^\d*$/.test(f)) throw new Error("enter a number like 1.5");
  return (BigInt(w || "0") * YOCTO + BigInt((f + "0".repeat(24)).slice(0, 24))).toString();
}
export const yoctoToNear = (y, dp = 4) => (Number((BigInt(y) * 10n ** BigInt(dp)) / YOCTO) / 10 ** dp).toFixed(dp);

export const deposit = (near) => call("deposit", {}, nearToYocto(near));
export const withdrawShares = (shares) => call("withdraw", { shares: String(shares) });
export const triggerEmergency = () => call("trigger_emergency", {});
export const claimExternal = () => call("claim_external", {});
export const retryPayout = () => call("retry_payout", {});

async function view(contractId, method, args) {
  const res = await fetch(RPC, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "query", params: {
      request_type: "call_function", finality: "optimistic", account_id: contractId,
      method_name: method, args_base64: btoa(JSON.stringify(args)) } }),
  });
  const b = await res.json();
  if (b.error || b.result?.error) throw new Error(b.error?.data ?? b.result.error);
  return JSON.parse(new TextDecoder().decode(new Uint8Array(b.result.result)));
}

/** The connected user's position, read straight from the contract. */
export async function position(contractId, accountId) {
  const [shares, s, claim] = await Promise.all([
    view(contractId, "get_shares", { account: accountId }),
    view(contractId, "get_summary", {}),
    view(contractId, "get_external_position", { account: accountId }).catch(() => null),
  ]);
  const total = BigInt(s.total_shares || "0");
  const nav = BigInt(s.nav_yocto ?? s.pool_yocto);
  const mine = BigInt(shares);
  const value = total > 0n ? (nav * mine) / total : 0n;
  // v4: a withdrawal pays exactly your slice of the NEAR pool now; the rest
  // of your value moves to you as external units (claimable as repatriated)
  const localSlice = total > 0n ? (BigInt(s.pool_yocto) * mine) / total : 0n;
  return { shares: mine, value, localSlice, summary: s, ext: claim };
}
