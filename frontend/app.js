// UI: leaderboard + vault page + red-team console. Hash routing, no framework.
//
// Modes:
//   default                      — demo fixtures (real proofs, simulated market)
//   ?vault=<contract>[&rpc=url]  — live NEAR testnet state, pinned above demo rows
import { runAllChecks, checkCodeAttested, checkCommitment } from "./verify.js";
import { LiveVault } from "./near-rpc.mjs";

const $ = (s, el = document) => el.querySelector(s);
const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

const qs = new URLSearchParams(location.search);
const LIVE_CONTRACT = qs.get("vault");
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
  if (LIVE_CONTRACT) {
    try {
      const live = await new LiveVault(LIVE_CONTRACT, RPC_URL).load();
      data.vaults.unshift(live);
    } catch (e) {
      data.liveError = `couldn't reach ${LIVE_CONTRACT}: ${e.message}`;
    }
  }
  DATA = data;
  return DATA;
}

const fmtPct = (p) => (p == null ? "—" : `${p >= 0 ? "+" : ""}${p.toFixed(2)}%`);
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
    <polyline points="${pts}" fill="none" stroke="${up ? "var(--proven)" : "var(--failed)"}" stroke-width="1.6"/></svg>`;
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
      .map((val) => `<line x1="8" x2="${w - 56}" y1="${py(val)}" y2="${py(val)}" stroke="var(--line)"/>
        <text x="${w - 50}" y="${py(val) + 4}" fill="var(--muted)" font-size="11" font-family="var(--mono)">${val.toFixed(1)}</text>`)
      .join("")}
    <text x="8" y="${h - 8}" fill="var(--muted)" font-size="11" font-family="var(--mono)">${t(0)} UTC</text>
    <text x="${w - 110}" y="${h - 8}" fill="var(--muted)" font-size="11" font-family="var(--mono)">${t(hist.length - 1)} UTC</text>
    <polyline points="${pts}" fill="none" stroke="var(--text)" stroke-width="1.8"/>
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
        <span class="vault-name">${esc(v.name)}${v.live ? ` <span class="chip ok" style="margin-left:6px"><span class="dot"></span>live</span>` : ""}<small>${esc(v.id)}</small></span>
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
    <h2 id="page-h" tabindex="-1">Vaults</h2>
    <div class="ledger">
      <div class="ledger-head"><span>Vault</span><span>NAV</span><span style="text-align:right">Return</span><span style="text-align:right">Verification</span></div>
      ${rows}
    </div>
    <p class="muted" style="margin-top:12px">Chips summarize on-chain state. Open a vault and run the checks yourself — verification happens in your browser, not on a server.</p>`;
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
        <td>${stateChip}</td>
        <td class="num muted">${when(c.committed_at_ms)}</td>
      </tr>
      <tr class="detail-row" data-for="${c.id}" hidden><td colspan="5">
        <div class="reveal-box">
          <div><span class="lbl">commitment (on NEAR, pre-execution)</span>&nbsp; ${c.commitment} ${copyBtn(c.commitment)}</div>
          ${c.revealed ? `<div style="margin-top:6px"><span class="lbl">revealed decision</span>&nbsp; ${esc(c.revealed)} ${copyBtn(c.revealed)}</div>
          <div><span class="lbl">salt</span>&nbsp; ${c.salt_hex} ${copyBtn(c.salt_hex)}</div>` : ""}
          ${c.reject_reason ? `<div class="v-bad" style="margin-top:6px">refused by guardrails: ${esc(c.reject_reason)}</div>` : ""}
          ${c.attestation_ref ? `<div style="margin-top:6px"><span class="lbl">attestation</span>&nbsp; ${esc(c.attestation_ref)} <span class="lbl">· wasm</span> ${short(c.wasm_hash)}</div>` : ""}
          ${c.execution_ref ? `<div><span class="lbl">target-chain tx</span>&nbsp; ${esc(c.execution_ref)}</div>` : ""}
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
        <div class="sub">${esc(v.id)} · strategy ${short(v.approved_wasm_hash)} ${copyBtn(v.approved_wasm_hash)}</div>
      </div>
      <div style="display:flex; gap:8px; align-items:center; flex-wrap:wrap">
        <span id="b-code">${chip("wait", "code unchecked")}</span>
        <span id="b-trades">${chip("wait", "trades unchecked")}</span>
        <span id="b-risk">${chip("wait", "risk unchecked")}</span>
        <button class="primary" id="run-checks">Run all checks</button>
      </div>
    </div>
    ${v.integrity_alert ? `<div class="alert"><b>Integrity alert — deposits paused.</b> A decision arrived attested to different code: <code>${esc(v.integrity_alert)}</code></div>` : ""}
    <div class="cols">
      <div class="panel"><h3>NAV${v.return_pct == null ? "" : ` (${fmtPct(v.return_pct)} since inception)`}</h3>${navChart(v)}</div>
      <div class="panel"><h3>Guardrails — enforced by the contract, not the strategy</h3>
        <dl class="kv">
          <dt>max leverage</dt><dd>${(g.max_leverage_bps / 10000).toFixed(1)}x</dd>
          <dt>max drawdown</dt><dd>${(g.max_drawdown_bps / 100).toFixed(0)}%</dd>
          <dt>max position</dt><dd>${(g.max_position_bps / 100).toFixed(0)}% of NAV</dd>
          <dt>pairs</dt><dd>${g.allowed_pairs.map(esc).join(", ")}</dd>
          <dt>pool</dt><dd>${v.pool_near} NEAR</dd>
          <dt>perf fee</dt><dd>20% above high-water mark</dd>
          <dt>manager</dt><dd title="holder of the strategy NFT">${esc(v.manager)}</dd>
        </dl>
        <p class="muted" style="font-size:12.5px; margin:10px 0 0">The sealed strategy decides <em>what</em> to trade; it cannot decide to exceed these. A breach is refused before any signature exists.</p>
      </div>
    </div>
    <h2>Decision ledger <span class="muted" style="font-weight:400">— committed on NEAR before execution; open a row</span></h2>
    <div class="panel" style="padding:4px 10px">
      ${v.commitments.length ? `<table class="commits">
        <thead><tr><th></th><th>commitment</th><th>decision</th><th>status</th><th>committed</th></tr></thead>
        <tbody>${commitRows}</tbody>
      </table>` : `<p class="muted" style="padding:10px">No decisions yet. The first enclave run will commit here before it executes anywhere.</p>`}
    </div>
    <h2>Checks <span class="muted" style="font-weight:400">— run in this browser</span></h2>
    <div class="panel" id="check-log" aria-live="polite">
      <div class="check-row"><span class="what">Attested code hash equals the approved strategy on every decision</span><span class="result muted" id="r-code">not run</span></div>
      <div class="check-row"><span class="what">Every revealed trade recomputes to its pre-execution commitment (SHA-256, WebCrypto)</span><span class="result muted" id="r-trades">not run</span></div>
      <div class="check-row"><span class="what">Bulletproof range proofs: private risk metrics stayed within the guardrails</span><span class="result muted" id="r-risk">not run</span></div>
    </div>`;
}

async function onRunChecks() {
  const v = CURRENT_VAULT;
  if (!v) return;
  const setChip = (sel, st, lbl) => { const el = $(sel); if (el) el.innerHTML = chip(st, lbl); };
  const setRes = (sel, r) => {
    const el = $(sel);
    if (el) el.innerHTML = `<span class="${{ ok: "v-ok", bad: "v-bad", wait: "v-wait" }[r.state]}">${{ ok: "✓", bad: "✗", wait: "–" }[r.state]} ${esc(r.detail)}</span>`;
  };
  ["#b-code", "#b-trades", "#b-risk"].forEach((s) => { const el = $(s); if (el) el.innerHTML = `<span class="chip running"><span class="dot"></span>checking…</span>`; });
  const label = (names) => (r) => names[r.state];
  const codeL = label({ ok: "code attested", bad: "code mismatch", wait: "code unchecked" });
  const tradesL = label({ ok: "trades committed", bad: "back-fill detected", wait: "trades unchecked" });
  const riskL = label({ ok: "risk proven", bad: "risk breach", wait: "risk unproven" });
  await runAllChecks(v, {
    onCode: (r) => { setChip("#b-code", r.state, codeL(r)); setRes("#r-code", r); },
    onTrades: (r) => { setChip("#b-trades", r.state, tradesL(r)); setRes("#r-trades", r); },
    onRisk: (r) => { setChip("#b-risk", r.state, riskL(r)); setRes("#r-risk", r); },
    onTradeStep: (id) => { const el = $("#r-trades"); if (el) el.textContent = `recomputing #${id}…`; },
    onRiskStep: (ep) => { const el = $("#r-risk"); if (el) el.textContent = `verifying epoch ${ep}…`; },
  });
}

// ------------------------------------------------------------ red-team console
async function renderRedTeam() {
  document.title = "Red-team console — Verifiable Alpha Vaults";
  const data = await loadData();
  if (!data.cheats) {
    $("#view").innerHTML = `<p class="muted">Red-team fixtures unavailable (serve the frontend folder over http).</p>`;
    return;
  }
  const degen = data.vaults.find((v) => v.name === "Degen Max");
  const shadow = data.vaults.find((v) => v.integrity_alert);
  const degenRej = degen?.commitments.find((c) => c.status === "Rejected");

  $("#view").innerHTML = `
    <h2 id="page-h" tabindex="-1">Red-team console</h2>
    <p class="muted" style="max-width:70ch">Three ways a vault manager might cheat, and where each one dies. Everything below runs against real data structures — press the buttons.</p>
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
  if (h.startsWith("#/vault/")) await renderVault(decodeURIComponent(h.slice(8)));
  else if (h === "#/redteam") await renderRedTeam();
  else await renderLeaderboard();
  $("#page-h")?.focus({ preventScroll: true });
}
window.addEventListener("hashchange", route);
route().then(() => {
  if (qs.has("autocheck") && location.hash.startsWith("#/vault/")) setTimeout(onRunChecks, 300);
});
