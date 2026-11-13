// UI: leaderboard + vault page + red-team console. Hash routing, no framework.
//
// Modes:
//   default                      — demo fixtures (real proofs, simulated market)
//   ?vault=<contract>[&rpc=url]  — live NEAR testnet state, pinned above demo rows
import { runAllChecks, checkCodeAttested, checkCommitment } from "./verify.js";
import { LiveVault, netOf } from "./near-rpc.mjs";
import * as W from "./wallet.mjs";

const $ = (s, el = document) => el.querySelector(s);
const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

const qs = new URLSearchParams(location.search);
// Live testnet vaults shown by default, alongside the demo vaults.
const DEFAULT_LIVE_VAULTS = ["vault.alpha-vaults-23.testnet"];
const LIVE_CONTRACTS = qs.get("vault") ? [qs.get("vault")] : DEFAULT_LIVE_VAULTS;
const RPC_URL = qs.get("rpc") ?? undefined;

let DATA = null;
let CURRENT_VAULT = null; // vault object shown on the open vault page

async function loadData() {
  if (DATA) return DATA;
  let fixtures = { vaults: [], cheats: null };
  try {
    const res = await fetch("./demo/fixtures.json");
    if (res.ok) fixtures = await res.json();
  } catch {
    /* file:// or missing — handled below */
  }
  const data = { vaults: [...fixtures.vaults], cheats: fixtures.cheats, liveError: null };
  for (const id of LIVE_CONTRACTS) {
    try {
      data.vaults.unshift(await new LiveVault(id, RPC_URL).load());
      const mode = document.getElementById("mode");
      if (mode) mode.textContent = "live + demo records";
    } catch (e) {
      data.liveError = `couldn't reach ${id}: ${e.message}`;
    }
  }
  DATA = data;
  return DATA;
}

const fmtPct = (p) => (p == null ? "—" : Math.abs(p) < 0.005 ? "0.00%" : `${p > 0 ? "+" : ""}${p.toFixed(2)}%`);
const short = (h, n = 10) => (h ? `${h.slice(0, n)}…${h.slice(-4)}` : "—");
const when = (ms) => {
  const d = Date.now() - ms;
  if (d < 90_000) return "just now";
  if (d < 5_400_000) return `${Math.round(d / 60_000)} min ago`;
  if (d < 129_600_000) return `${Math.round(d / 3_600_000)} h ago`;
  return new Date(ms).toISOString().slice(0, 10);
};

function chip(state, label) {
  const cls = { ok: "ok", bad: "bad", wait: "wait" }[state] || "wait";
  return `<span class="chip ${cls}"><span class="dot"></span>${esc(label)}</span>`;
}
let stampSeq = 0;
function stamp(state, word) {
  const color = { ok: "var(--verified)", bad: "var(--void)", wait: "var(--wax)" }[state];
  const id = "ring" + stampSeq++;
  const micro = "· VERIFIABLE · ALPHA · VAULTS · NEAR ".repeat(2);
  return `<svg class="stamp slam" viewBox="0 0 100 100" role="img" aria-label="${esc(word)}">
    <circle cx="50" cy="50" r="47" fill="none" stroke="${color}" stroke-width="2.6"/>
    <circle cx="50" cy="50" r="34" fill="none" stroke="${color}" stroke-width="0.9"/>
    <defs><path id="${id}" d="M50,50 m-40.5,0 a40.5,40.5 0 1,1 81,0 a40.5,40.5 0 1,1 -81,0"/></defs>
    <text font-size="5.4" letter-spacing="1.25" fill="${color}" font-family="var(--mono)"><textPath href="#${id}">${micro}</textPath></text>
    <text x="50" y="54.5" text-anchor="middle" font-family="Fraunces, serif" font-weight="700" font-size="${word.length > 8 ? 10.5 : 13}" letter-spacing="0.06em" fill="${color}">${esc(word)}</text>
  </svg>`;
}

const copyBtn = (value) =>
  `<button class="copy" data-copy="${esc(value)}" title="Copy" aria-label="Copy to clipboard">copy</button>`;

function sparkline(hist, w = 220, h = 36) {
  if (!hist || hist.length < 2) return `<span class="muted" style="font-size:12px">history accrues on-chain</span>`;
  const navs = hist.map((p) => p.nav);
  const min = Math.min(...navs), max = Math.max(...navs);
  const span = max - min || 1;
  const pts = navs
    .map((v, i) => `${((i / (navs.length - 1)) * (w - 4) + 2).toFixed(1)},${(h - 3 - ((v - min) / span) * (h - 6)).toFixed(1)}`)
    .join(" ");
  const up = navs[navs.length - 1] >= navs[0];
  return `<svg class="spark" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" role="img" aria-label="NAV history">
    <polyline points="${pts}" fill="none" stroke="${up ? "var(--verified)" : "var(--void)"}" stroke-width="1.6"/></svg>`;
}

function navChart(v, w = 560, h = 190) {
  const hist = v.nav_history;
  if (!hist || hist.length < 2)
    return `<p class="muted" style="margin:24px 0">NAV history builds up as epochs settle on-chain. Pool right now: <span class="num">${v.pool_near} NEAR</span>.</p>`;
  const navs = hist.map((p) => p.nav);
  const min = Math.min(...navs), max = Math.max(...navs);
  const span = max - min || 1;
  const px = (i) => (i / (navs.length - 1)) * (w - 64) + 8;
  const py = (val) => h - 26 - ((val - min) / span) * (h - 48);
  const pts = navs.map((val, i) => `${px(i).toFixed(1)},${py(val).toFixed(1)}`).join(" ");
  const t = (i) => new Date(hist[i].t).toISOString().slice(11, 16);
  return `<svg width="100%" viewBox="0 0 ${w} ${h}" role="img" aria-label="NAV over time">
    ${[min, (min + max) / 2, max]
      .map((val) => `<line x1="8" x2="${w - 56}" y1="${py(val)}" y2="${py(val)}" stroke="var(--rule)"/>
        <text x="${w - 50}" y="${py(val) + 4}" fill="var(--ink-2)" font-size="11" font-family="var(--mono)">${val.toFixed(1)}</text>`)
      .join("")}
    <text x="8" y="${h - 8}" fill="var(--ink-2)" font-size="11" font-family="var(--mono)">${t(0)} UTC</text>
    <text x="${w - 110}" y="${h - 8}" fill="var(--ink-2)" font-size="11" font-family="var(--mono)">${t(hist.length - 1)} UTC</text>
    <polyline points="${pts}" fill="none" stroke="var(--ink)" stroke-width="1.6"/>
  </svg>`;
}

// ---------------------------------------------------------------- leaderboard
async function renderLeaderboard() {
  document.title = "Verifiable Alpha Vaults";
  const data = await loadData();
  if (data.vaults.length === 0) {
    $("#view").innerHTML = `<div class="panel"><p><b>No vault data.</b> The demo fixtures couldn't load — browsers block <code>fetch</code> on <code>file://</code> pages.</p>
      <p class="muted">Serve the folder instead:</p><div class="reveal-box">cd frontend &amp;&amp; python3 -m http.server 8080</div>
      <p class="muted">then open <code>http://localhost:8080</code>.</p></div>`;
    return;
  }
  const vaults = [...data.vaults].sort((a, b) => (b.return_pct ?? -1e9) - (a.return_pct ?? -1e9));
  const rows = vaults
    .map((v) => {
      const code = checkCodeAttested(v);
      const revealed = v.commitments.filter((c) => c.revealed).length;
      const proofs = v.risk_proofs.length;
      const rejected = v.commitments.filter((c) => c.status === "Rejected").length;
      const codeLabel = { ok: "code attested", bad: "code mismatch", wait: "code unchecked" }[code.state];
      return `<a class="ledger-row" href="#/vault/${encodeURIComponent(v.id)}">
        <span class="vault-name">${esc(v.name)}${v.live ? ` <span class="chip ok" style="margin-left:6px"><span class="dot"></span>live</span>` : ""}${v.name === v.id ? "" : `<small>${esc(v.id)}</small>`}</span>
        <span class="spark-cell">${sparkline(v.nav_history)}</span>
        <span class="ret ${v.return_pct == null || Math.abs(v.return_pct) < 0.005 ? "muted" : v.return_pct >= 0 ? "v-ok" : "v-bad"}">${fmtPct(v.return_pct)}</span>
        <span class="badges">
          ${chip(code.state, codeLabel)}
          ${chip(revealed ? "ok" : "wait", `${revealed} trades committed`)}
          ${chip(proofs ? "ok" : rejected ? "bad" : "wait", proofs ? `${proofs} risk proofs` : rejected ? `${rejected} breaches blocked` : "no proofs")}
        </span>
      </a>`;
    })
    .join("");

  $("#view").innerHTML = `
    ${data.liveError ? `<div class="alert" style="border-left-color:var(--pending); border-color:var(--pending-dim)">Live mode: ${esc(data.liveError)} — showing demo vaults.</div>` : ""}
    <h2 id="page-h" tabindex="-1">The Registry</h2>
    <div class="ledger">
      <div class="ledger-head"><span>Vault</span><span>NAV</span><span style="text-align:right">Return</span><span style="text-align:right">Verification</span></div>
      ${rows}
    </div>
    <p class="muted" style="margin-top:12px; font-style:italic">Seals summarize on-chain state. Open a vault and examine it yourself — the stamps are earned in your browser, never issued by a server.</p>`;
}

// ----------------------------------------------------------------- vault page
async function renderVault(id) {
  const data = await loadData();
  const v = data.vaults.find((x) => x.id === id);
  if (!v) {
    $("#view").innerHTML = `<p>No vault <code>${esc(id)}</code>. <a href="#/">Back to the leaderboard</a>.</p>`;
    return;
  }
  CURRENT_VAULT = v;
  document.title = `${v.name} — Verifiable Alpha Vaults`;

  const g = v.guardrails;
  const commitRows = v.commitments
    .map((c) => {
      const d = c.revealed ? JSON.parse(c.revealed) : null;
      const stateChip =
        c.status === "Executed" ? chip("ok", "executed")
        : c.status === "Rejected" ? chip("bad", "rejected")
        : chip("wait", "sealed");
      return `<tr class="expandable" data-id="${c.id}" tabindex="0" role="button" aria-expanded="false">
        <td class="num">#${c.id}</td>
        <td><span class="hash">${short(c.commitment)}</span></td>
        <td>${d ? (d.action === "hold" ? `<span class="muted">hold</span>` : `${esc(d.action)} ${esc(d.pair)} <span class="muted">(${(d.leverage_bps / 10000).toFixed(1)}x, ${(d.size_bps / 100).toFixed(0)}% NAV)</span>`) : `<span class="muted">sealed until execution</span>`}</td>
        <td>${stateChip}${c.attestation_ref?.startsWith("onchain") ? ` <span class="annot" title="Created by the vault's own callback from OutLayer's on-chain request: no relayer in between">on-chain</span>` : ""}</td>
        <td class="num muted">${when(c.committed_at_ms)}</td>
      </tr>
      <tr class="detail-row" data-for="${c.id}" hidden><td colspan="5">
        <div class="reveal-box">
          <div><span class="lbl">commitment (on NEAR, pre-execution)</span>&nbsp; ${c.commitment} ${copyBtn(c.commitment)}</div>
          ${c.revealed ? `<div style="margin-top:6px"><span class="lbl">revealed decision</span>&nbsp; ${esc(c.revealed)} ${copyBtn(c.revealed)}</div>
          <div><span class="lbl">salt</span>&nbsp; ${c.salt_hex} ${copyBtn(c.salt_hex)}</div>` : ""}
          ${c.reject_reason ? `<div class="v-bad" style="margin-top:6px">refused by guardrails: ${esc(c.reject_reason)}</div>` : ""}
          ${c.attestation_ref ? `<div style="margin-top:6px"><span class="lbl">attestation</span>&nbsp; ${c.attestation_ref.startsWith("https://") ? `<a href="${esc(c.attestation_ref)}" target="_blank" rel="noopener">${esc(c.attestation_ref)}</a>` : esc(c.attestation_ref)} <span class="lbl">· wasm</span> ${short(c.wasm_hash)}</div>` : ""}
          ${c.execution_ref ? `<div><span class="lbl">target-chain tx</span>&nbsp; ${c.execution_ref.startsWith("0x") ? `<a href="${netOf(v.id).explorer}/tx/${esc(c.execution_ref)}" target="_blank" rel="noopener">${esc(c.execution_ref)}</a> <span class="lbl">(Uniswap v3 on ${netOf(v.id).evm}; the last 32 bytes of its calldata are this commitment)</span>` : esc(c.execution_ref)}</div>` : ""}
          ${c.input_json ? (() => { const o = JSON.parse(c.input_json).oracle; return o ? `<div><span class="lbl">prices the enclave saw</span>&nbsp; ETH $${o.eth_usd} · NEAR $${o.near_usd} · ${esc(o.spot_source)} @ ${new Date(o.as_of * 1000).toISOString().slice(0, 16).replace("T", " ")} UTC</div>` : ""; })() : ""}
        </div>
        ${c.revealed ? `<button data-recompute="${c.id}">Recompute this hash</button><span class="check-line" id="recompute-${c.id}" role="status"></span>` : ""}
      </td></tr>`;
    })
    .join("");

  $("#view").innerHTML = `
    <p style="margin:0 0 4px"><a href="#/" class="muted" style="text-decoration:none">← all vaults</a></p>
    <div class="vault-head">
      <div>
        <h2 id="page-h" tabindex="-1">${esc(v.name)}</h2>
        <div class="sub">serial ${esc(v.id)} · strategy ${short(v.approved_wasm_hash)} ${copyBtn(v.approved_wasm_hash)}</div>
      </div>
      <div style="display:flex; gap:8px; align-items:center; flex-wrap:wrap">
        <span id="b-code">${chip("wait", "code unchecked")}</span>
        <span id="b-trades">${chip("wait", "trades unchecked")}</span>
        <span id="b-risk">${chip("wait", "risk unchecked")}</span>
        <span id="b-prices">${chip("wait", "prices unchecked")}</span>
        <button class="primary" id="run-checks">Run all checks</button>
      </div>
    </div>
    <p class="certline">This page certifies that every decision below was committed on NEAR before it executed anywhere, attested by Intel TDX to the approved code, computed from real market prices, and bounded by the stated guardrails — or shows you exactly where that failed.</p>
    <div class="stamp-row" id="stamps" aria-live="polite"></div>
    ${v.live ? `<div class="panel position" id="position"><h3>Your position</h3><p class="muted">Loading wallet…</p></div>` : ""}
    ${v.emergency ? `<div class="alert"><b>Emergency exit mode.</b> No attested decision for over ${Math.round(v.liveness_timeout_ms / 3.6e6)}h. Trading is frozen; withdrawals pay your share of the local pool now and record a claim on cross-chain value.</div>` : ""}
    ${v.pending_strategy ? `<div class="alert notice"><b>Strategy change proposed.</b> New code <span class="hash">${short(v.pending_strategy.hash)}</span> can take over ${new Date(v.pending_strategy.effective_at_ms) > new Date() ? `from ${new Date(v.pending_strategy.effective_at_ms).toLocaleString([], { weekday: "short", hour: "numeric", minute: "2-digit" })}` : "now: anyone can apply it"}. Changes to what code runs are public for 48 hours first, so you can leave before they apply.</div>` : ""}
    ${v.exit ? `<div class="alert notice"><b>Exit to NEAR proposed.</b> ${(Number(v.exit.amount_usdc) / 1e6).toFixed(2)} USDC goes to NEAR Intents deposit address <span class="hash">${short(v.exit.deposit_address)}</span>, ${v.exit.signed ? "signed and sent." : new Date(v.exit.effective_at_ms) > new Date() ? `signable from ${new Date(v.exit.effective_at_ms).toLocaleString([], { weekday: "short", hour: "numeric", minute: "2-digit" })}.` : "signable now."} Check that its recipient is this vault: <a href="https://1click.chaindefuser.com/v0/status?depositAddress=${encodeURIComponent(v.exit.deposit_address)}" target="_blank" rel="noopener">1Click status</a>.</div>` : ""}
    ${v.integrity_alert ? `<div class="alert"><b>Integrity alert — deposits paused.</b> A decision arrived attested to different code: <code>${esc(v.integrity_alert)}</code></div>` : ""}
    <div class="cols">
      <div class="panel"><h3>Net asset value${v.return_pct == null ? "" : ` · ${fmtPct(v.return_pct)} per share`}</h3>${navChart(v)}${v.external_near > 0 ? `<p class="muted" style="font-size:12.5px; font-style:italic; margin:8px 0 0">The step up is the vault’s cross-chain wallet being valued for the first time. The manager funded that wallet and holds every share, so this is a valuation report, not a realized return: no fee can be charged on it until it comes back as NEAR.</p>` : ""}</div>
      <div class="panel"><h3>Terms of issue</h3>
        <dl class="kv">
          <dt>max leverage</dt><dd>${(g.max_leverage_bps / 10000).toFixed(1)}x</dd>
          <dt>max drawdown</dt><dd>${(g.max_drawdown_bps / 100).toFixed(0)}%</dd>
          <dt>max position</dt><dd>${(g.max_position_bps / 100).toFixed(0)}% of NAV</dd>
          <dt>pairs</dt><dd>${g.allowed_pairs.map(esc).join(", ")}</dd>
          <dt>net asset value</dt><dd>${v.pool_near} NEAR</dd>
          ${v.external_near != null ? `<dt>· on NEAR</dt><dd>${v.local_near} NEAR</dd><dt>· cross-chain</dt><dd>${v.external_near} NEAR</dd>` : ""}
          ${v.evm_address ? `<dt>cross-chain wallet</dt><dd style="font-size:11px"><a href="${netOf(v.id).explorer}/address/${esc(v.evm_address)}" target="_blank" rel="noopener">${esc(v.evm_address.slice(0, 10))}…</a> (vault-owned)</dd>` : ""}
          ${v.last_settlement_ref ? `<dt>last settlement</dt><dd style="font-size:11px">${esc(v.last_settlement_ref.slice(0, 34))}…</dd>` : ""}
          ${v.last_attested_ms ? `<dt>exit guarantee</dt><dd style="font-size:11.5px">${v.emergency ? "EMERGENCY — withdraw pro rata" : `anyone may trigger exit if silent ${Math.round(v.liveness_timeout_ms / 3.6e6)}h (last: ${when(v.last_attested_ms)})`}</dd>` : ""}
          ${v.mode ? `<dt>trust mode</dt><dd>${v.mode === "Attested" ? "attested: only TDX-delivered output can act" : `relayed by <span title="openly trusted to deliver enclave output on time">${esc(v.executor ?? "executor")}</span>`}</dd>` : ""}
          <dt>perf fee</dt><dd>20% of realized gains above high-water mark</dd>
          ${v.live && v.evm_address ? `<dt>way back</dt><dd id="way-back">NEAR Intents: cross-chain USDC returns as NEAR</dd>` : ""}
          <dt>manager</dt><dd title="holder of the strategy NFT">${esc(v.manager)}</dd>
        </dl>
        <p class="muted" style="font-size:12.5px; margin:10px 0 0">The sealed strategy decides <em>what</em> to trade; it cannot decide to exceed these. A breach is refused before any signature exists.</p>
        ${v.mode === "Relayed" ? `<p class="muted" style="font-size:12.5px; margin:8px 0 0">In relayed mode the relayer can delay a run or misreport the cross-chain value on the chart. It cannot choose trade sizes, price your deposit or cash a fee: deposits only open while everything is in NEAR, and fees only come from NEAR that has actually returned.</p>` : ""}
      </div>
    </div>
    <h2>Decision ledger <span class="annot">— each entry committed on NEAR before execution; open a row to inspect</span></h2>
    <div class="panel" style="padding:4px 10px">
      ${v.commitments.length ? `<table class="commits">
        <thead><tr><th></th><th>commitment</th><th>decision</th><th>status</th><th>committed</th></tr></thead>
        <tbody>${commitRows}</tbody>
      </table>` : `<p class="muted" style="padding:10px">No decisions yet. The first enclave run will commit here before it executes anywhere.</p>`}
    </div>
    <h2>Examiner's checks <span class="annot">— performed by your browser, never by our server</span></h2>
    <div class="panel" id="check-log" aria-live="polite">
      <div class="check-row"><span class="what">Attested code: TDX attestations fetched from OutLayer; executed wasm equals the approved strategy</span><span class="result muted" id="r-code">not run</span></div>
      <div class="check-row"><span class="what">Every revealed trade recomputes to its pre-execution commitment (SHA-256, WebCrypto)</span><span class="result muted" id="r-trades">not run</span></div>
      <div class="check-row"><span class="what">Bulletproof range proofs: private risk metrics stayed within the guardrails</span><span class="result muted" id="r-risk">not run</span></div>
      <div class="check-row"><span class="what">Real prices: each published enclave input hashes to its attested input, and its oracle price matches public market history</span><span class="result muted" id="r-prices">not run</span></div>
    </div>`;
}

// ------------------------------------------------------------ your position
async function mountPosition(v) {
  const box = $("#position");
  if (!box) return;
  try {
    await W.ensureWallet(v.id);
  } catch (e) {
    box.innerHTML = `<h3>Your position</h3><p class="muted">Wallet unavailable: ${esc(e.message)}</p>`;
    return;
  }
  loadWayBack(v);
  await renderPosition(v);
}

/** Live NEAR Intents (1Click) dry-run quote for the vault wallet's USDC -> NEAR.
 * NEAR Intents is mainnet-only, so a testnet vault is priced on the same
 * mainnet route (Ethereum USDC -> NEAR); the label says so. */
async function loadWayBack(v) {
  const el = document.getElementById("way-back");
  if (!el) return;
  try {
    const bal = await fetch(netOf(v.id).evmRpc, { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "eth_call", params: [{ to: netOf(v.id).usdc, data: "0x70a08231" + v.evm_address.slice(2).toLowerCase().padStart(64, "0") }, "latest"] }) }).then((r) => r.json());
    const usdc = BigInt(bal.result || "0x0");
    if (usdc === 0n) { el.textContent = "NEAR Intents: nothing cross-chain to bring back right now"; return; }
    const q = await fetch("https://1click.chaindefuser.com/v0/quote", { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ dry: true, swapType: "EXACT_INPUT", slippageTolerance: 100, originAsset: netOf(v.id).intentsUsdc,
        depositType: "ORIGIN_CHAIN", destinationAsset: "nep141:wrap.near", amount: usdc.toString(), refundTo: v.evm_address, refundType: "ORIGIN_CHAIN",
        recipient: v.id.endsWith(".near") ? v.id : "wrap.near", recipientType: "DESTINATION_CHAIN", deadline: new Date(Date.now() + 3600e3).toISOString() }) }).then((r) => r.json());
    if (!q.quote) throw new Error(q.message || "no quote");
    el.innerHTML = `NEAR Intents, live quote: ${esc(q.quote.amountInFormatted)} USDC → <b>${Number(q.quote.amountOutFormatted).toFixed(2)} NEAR</b> in ~${q.quote.timeEstimate}s${v.id.endsWith(".near") ? "" : ` <span class="muted" title="NEAR Intents has no testnet; this is the same route on mainnet">(mainnet route; testnet vault)</span>`}`;
  } catch {
    el.textContent = "NEAR Intents: cross-chain USDC returns as NEAR (quote unavailable right now)";
  }
}

async function renderPosition(v) {
  const box = $("#position");
  if (!box) return;
  const acct = W.currentAccount();
  if (!acct) {
    box.innerHTML = `<h3>Your position</h3>
      <p style="margin:0 0 12px">Connect a NEAR <b>${netOf(v.id).near}</b> wallet to deposit, see your shares, and withdraw.</p>
      <button class="primary" data-act="connect">Connect wallet</button>`;
    return;
  }
  box.innerHTML = `<h3>Your position</h3><p class="muted">Reading ${esc(acct)}…</p>`;
  const p = await W.position(v.id, acct);
  const s = p.summary;
  const emergency = s.emergency, stale = s.stale;
  const ext = p.ext ?? { units: "0", est_value_yocto: "0", claimable_yocto: "0", failed_payout_yocto: "0" };
  const hasUnits = BigInt(ext.units) > 0n;
  const claimable = BigInt(ext.claimable_yocto) > 0n;
  const failed = BigInt(ext.failed_payout_yocto) > 0n;
  box.innerHTML = `<h3>Your position · ${esc(acct)}</h3>
    <dl class="kv" style="margin-bottom:14px">
      <dt>shares</dt><dd>${W.yoctoToNear(p.shares.toString())}</dd>
      <dt>worth now</dt><dd>${W.yoctoToNear(p.value.toString())} NEAR</dd>
      ${p.shares > 0n ? `<dt>withdrawable now</dt><dd>${W.yoctoToNear(p.localSlice.toString())} NEAR</dd>` : ""}
      ${hasUnits ? `<dt>cross-chain units</dt><dd>≈ ${W.yoctoToNear(ext.est_value_yocto)} NEAR</dd>` : ""}
      ${claimable ? `<dt>claimable now</dt><dd>${W.yoctoToNear(ext.claimable_yocto)} NEAR</dd>` : ""}
    </dl>
    ${!emergency && s.deposits_open === false ? `<p style="margin:0 0 10px">Deposits are closed while capital is deployed cross-chain. They reopen once it has all come back as NEAR, so a valuation report never sets the price you pay.</p>` : ""}
    ${emergency ? `<p class="v-bad" style="margin:0 0 10px"><b>Emergency exit mode.</b> Trading and deposits are frozen. Withdrawals still pay your exact slice of the NEAR pool and move your slice of the cross-chain wallet to you as claimable units.</p>` : ""}
    <div class="actions">
      ${!emergency && s.deposits_open !== false ? `<label class="field"><span>Deposit (NEAR)</span><input id="dep-amt" inputmode="decimal" placeholder="1.0" autocomplete="off"></label>
      <button class="primary" data-act="deposit">Deposit</button>` : ""}
      ${p.shares > 0n ? `<label class="field"><span>Withdraw (% of your shares)</span><input id="wd-pct" inputmode="numeric" value="100"></label>
      <button data-act="withdraw">${emergency ? "Emergency withdraw" : "Withdraw"}</button>` : ""}
      ${claimable ? `<button data-act="claim">Claim repatriated NEAR</button>` : ""}
      ${failed ? `<button data-act="retry">Retry failed payout</button>` : ""}
      ${stale && !emergency ? `<button data-act="emergency" title="The vault has been silent past its liveness timeout">Trigger emergency exit</button>` : ""}
    </div>
    <p class="muted" id="pos-msg" role="status" style="font-size:12.5px; margin:10px 0 0">${p.shares > 0n && p.value > p.localSlice ? "Withdrawing pays your NEAR-pool slice now; your cross-chain slice becomes units, paid out as funds are repatriated." : "Each action is a transaction you sign in your own wallet."}</p>
    <p style="margin:6px 0 0; font-size:12.5px"><a href="#" data-act="disconnect">Sign out</a></p>`;
}

async function onPositionAction(act) {
  const v = CURRENT_VAULT;
  const msg = (t, cls = "muted") => { const el = $("#pos-msg"); if (el) { el.className = cls; el.textContent = t; } };
  try {
    if (act === "connect") return W.connect(v.id);
    if (act === "disconnect") { await W.disconnect(); return renderPosition(v); }
    msg("Waiting for your wallet…");
    if (act === "deposit") {
      const amt = $("#dep-amt").value;
      W.nearToYocto(amt); // validates
      if (Number(amt) <= 0) throw new Error("enter an amount above 0");
      await W.deposit(amt);
    } else if (act === "withdraw") {
      const pct = BigInt(Math.max(1, Math.min(100, parseInt($("#wd-pct").value, 10) || 0)));
      const p = await W.position(v.id, W.currentAccount());
      const shares = (p.shares * pct) / 100n;
      if (shares === 0n) throw new Error("no shares to withdraw");
      await W.withdrawShares(shares);
    } else if (act === "emergency") {
      await W.triggerEmergency();
    } else if (act === "claim") {
      await W.claimExternal();
    } else if (act === "retry") {
      await W.retryPayout();
    }
    msg("Done. Refreshing…", "v-ok");
    DATA = null; // re-read chain state
    await renderVault(v.id);
    if (CURRENT_VAULT?.live) await renderPosition(CURRENT_VAULT);
  } catch (e) {
    const m = String(e?.message ?? e);
    msg(m.includes("User rejected") || m.includes("cancel") ? "Cancelled in your wallet." : `Failed: ${m.replace(/^.*Smart contract panicked: /, "")}`, "v-bad");
  }
}

W.onWalletChange(() => { if (CURRENT_VAULT?.live) renderPosition(CURRENT_VAULT); });

async function onRunChecks() {
  const v = CURRENT_VAULT;
  if (!v) return;
  const setChip = (sel, st, lbl) => { const el = $(sel); if (el) el.innerHTML = chip(st, lbl); };
  const setRes = (sel, r) => {
    const el = $(sel);
    if (el) el.innerHTML = `<span class="${{ ok: "v-ok", bad: "v-bad", wait: "v-wait" }[r.state]}">${{ ok: "✓", bad: "✗", wait: "–" }[r.state]} ${esc(r.detail)}</span>`;
  };
  ["#b-code", "#b-trades", "#b-risk", "#b-prices"].forEach((s) => { const el = $(s); if (el) el.innerHTML = `<span class="chip running"><span class="dot"></span>checking…</span>`; });
  const stamps = $("#stamps");
  if (stamps) stamps.innerHTML = "";
  const addStamp = (r, words) => {
    if (stamps) stamps.insertAdjacentHTML("beforeend", stamp(r.state, words[r.state]));
  };
  const label = (names) => (r) => names[r.state];
  const codeL = label({ ok: "code attested", bad: "code mismatch", wait: "code unchecked" });
  const tradesL = label({ ok: "trades committed", bad: "back-fill detected", wait: "trades unchecked" });
  const riskL = label({ ok: "risk proven", bad: "risk breach", wait: "risk unproven" });
  const priceL = label({ ok: "prices verified", bad: "prices forged", wait: "prices unchecked" });
  await runAllChecks(v, {
    onCode: (r) => { setChip("#b-code", r.state, codeL(r)); setRes("#r-code", r); addStamp(r, { ok: "ATTESTED", bad: "VOID", wait: "UNVERIFIED" }); },
    onTrades: (r) => { setChip("#b-trades", r.state, tradesL(r)); setRes("#r-trades", r); addStamp(r, { ok: "COMMITTED", bad: "VOID", wait: "UNVERIFIED" }); },
    onRisk: (r) => { setChip("#b-risk", r.state, riskL(r)); setRes("#r-risk", r); addStamp(r, { ok: "IN LIMITS", bad: "VOID", wait: "UNPROVEN" }); },
    onTradeStep: (id) => { const el = $("#r-trades"); if (el) el.textContent = `recomputing #${id}…`; },
    onRiskStep: (ep) => { const el = $("#r-risk"); if (el) el.textContent = `verifying epoch ${ep}…`; },
    onPrices: (r) => { setChip("#b-prices", r.state, priceL(r)); setRes("#r-prices", r); addStamp(r, { ok: "PRICED", bad: "VOID", wait: "UNPRICED" }); },
    onPriceStep: (id) => { const el = $("#r-prices"); if (el) el.textContent = `hashing enclave input #${id}…`; },
    onCodeStep: (id) => { const el = $("#r-code"); if (el) el.textContent = `fetching TDX attestation for #${id}…`; },
  });
}

// ------------------------------------------------------------ red-team console
async function renderRedTeam() {
  document.title = "Counterfeit Bureau — Verifiable Alpha Vaults";
  const data = await loadData();
  if (!data.cheats) {
    $("#view").innerHTML = `<p class="muted">Red-team fixtures unavailable (serve the frontend folder over http).</p>`;
    return;
  }
  const degen = data.vaults.find((v) => v.name === "Degen Max");
  const shadow = data.vaults.find((v) => v.integrity_alert);
  const degenRej = degen?.commitments.find((c) => c.status === "Rejected");

  $("#view").innerHTML = `
    <h2 id="page-h" tabindex="-1">Counterfeit Bureau</h2>
    <p class="muted" style="max-width:70ch; font-style:italic">Three ways a manager might forge a track record, and where each forgery dies. Everything below runs against real data — press the buttons and watch.</p>
    <div class="redteam">
      <div class="redteam-item">
        <h4>1 · Back-fill a winning trade</h4>
        <p>The manager committed to one decision before execution, then claims a different, winning decision afterwards. Recompute the hash and compare.</p>
        <button data-cheat="backfill">Reveal the "winning" trade against the real commitment</button>
        <div class="console-out" id="out-backfill" hidden></div>
      </div>
      <div class="redteam-item">
        <h4>2 · Ask for 10x leverage under a 3x cap</h4>
        <p>Sealed parameters demanded 10x. Two independent layers refuse: no Bulletproof can exist for a breach, and the contract rejects the reveal before anything is signed.</p>
        <button data-cheat="leverage">Show the contract's refusal</button>
        <div class="console-out" id="out-leverage" hidden></div>
      </div>
      <div class="redteam-item">
        <h4>3 · Swap in different strategy code</h4>
        <p>A decision arrives attested by the TEE — but to a different WASM hash. The vault refuses it and pauses deposits.</p>
        <button data-cheat="swap">Compare the attested hash</button>
        <div class="console-out" id="out-swap" hidden></div>
      </div>
    </div>`;
}

async function onCheat(kind) {
  const data = await loadData();
  const cheat = data.cheats.backfill;
  const degen = data.vaults.find((v) => v.name === "Degen Max");
  const shadow = data.vaults.find((v) => v.integrity_alert);
  if (kind === "backfill") {
    const out = $("#out-backfill"); out.hidden = false;
    const { ok, recomputed } = await checkCommitment(cheat.claimed_decision, cheat.salt_hex, cheat.commitment);
    out.innerHTML =
      `committed on NEAR (before execution):\n  ${cheat.commitment}\n` +
      `claimed decision at reveal:\n  ${esc(cheat.claimed_decision)}\n` +
      `sha256(claimed ‖ salt):\n  ${recomputed}\n\n` +
      (ok ? "" : `<span class="v-bad">✗ MISMATCH — contract aborts: "reveal does not match commitment: this trade was not committed when claimed"</span>\n\n`) +
      `the decision that was actually committed:\n  ${esc(cheat.honest_decision)}`;
  }
  if (kind === "leverage") {
    const c = degen.commitments.find((x) => x.status === "Rejected");
    const out = $("#out-leverage"); out.hidden = false;
    out.innerHTML =
      `revealed decision:\n  ${esc(c.revealed)}\n\n` +
      `<span class="v-bad">✗ guardrail_rejected: ${esc(c.reject_reason)}</span>\n` +
      `status on-chain: Rejected — nothing was signed\n` +
      `risk proofs posted by this vault: ${degen.risk_proofs.length} (a breach admits no proof)`;
  }
  if (kind === "swap") {
    const out = $("#out-swap"); out.hidden = false;
    out.innerHTML =
      `approved strategy wasm:\n  ${shadow.approved_wasm_hash}\n` +
      `attested wasm on incoming decision:\n  ${"9d".repeat(32)}\n\n` +
      `<span class="v-bad">✗ integrity alert — commit refused, deposits paused:</span>\n  "${esc(shadow.integrity_alert)}"`;
  }
}

// ------------------------------------------- one delegated listener, installed once
function toggleRow(row) {
  const det = $(`.detail-row[data-for="${row.dataset.id}"]`);
  if (!det) return;
  det.hidden = !det.hidden;
  row.setAttribute("aria-expanded", String(!det.hidden));
}

document.addEventListener("click", async (e) => {
  const row = e.target.closest("tr.expandable");
  if (row && !e.target.closest("button")) return toggleRow(row);

  const copy = e.target.closest("[data-copy]");
  if (copy) {
    await navigator.clipboard.writeText(copy.dataset.copy);
    const old = copy.textContent;
    copy.textContent = "copied";
    setTimeout(() => (copy.textContent = old), 900);
    return;
  }

  const re = e.target.closest("[data-recompute]");
  if (re && CURRENT_VAULT) {
    const c = CURRENT_VAULT.commitments.find((x) => String(x.id) === re.dataset.recompute);
    const out = $(`#recompute-${c.id}`);
    out.textContent = " computing…";
    const { ok, recomputed } = await checkCommitment(c.revealed, c.salt_hex, c.commitment);
    out.innerHTML = ok
      ? ` <span class="v-ok">✓ sha256(decision ‖ salt) = ${short(recomputed)} — matches</span>`
      : ` <span class="v-bad">✗ recomputed ${short(recomputed)} ≠ committed ${short(c.commitment)}</span>`;
    return;
  }

  const act = e.target.closest("[data-act]");
  if (act) { e.preventDefault(); return onPositionAction(act.dataset.act); }
  if (e.target.closest("#run-checks")) return onRunChecks();
  const cheatBtn = e.target.closest("[data-cheat]");
  if (cheatBtn) return onCheat(cheatBtn.dataset.cheat);
});

document.addEventListener("keydown", (e) => {
  if ((e.key === "Enter" || e.key === " ") && e.target.matches("tr.expandable")) {
    e.preventDefault();
    toggleRow(e.target);
  }
});

// --------------------------------------------------------------------- router
async function route() {
  const h = location.hash || "#/";
  CURRENT_VAULT = null;
  if (h.startsWith("#/vault/")) {
    await renderVault(decodeURIComponent(h.slice(8)));
    if (CURRENT_VAULT?.live) mountPosition(CURRENT_VAULT);
  }
  else if (h === "#/redteam") await renderRedTeam();
  else await renderLeaderboard();
  $("#page-h")?.focus({ preventScroll: true });
}
window.addEventListener("hashchange", route);
route().then(() => {
  if (qs.has("autocheck") && location.hash.startsWith("#/vault/")) setTimeout(onRunChecks, 300);
});
