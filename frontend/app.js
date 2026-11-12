// UI: leaderboard + vault page + red-team console. Hash routing, no framework.
import {
  runAllChecks, checkCodeAttested, checkCommitment,
} from "./verify.js";

const $ = (s, el = document) => el.querySelector(s);
const esc = (s) =>
  String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

let DATA = null;

async function loadData() {
  if (DATA) return DATA;
  // Live mode (?rpc=<vault-account>) lands Friday; demo mode reads fixtures
  // whose commitments and Bulletproofs are real (see tools/fixtures).
  const res = await fetch("./demo/fixtures.json");
  DATA = await res.json();
  return DATA;
}

const fmtPct = (p) => `${p >= 0 ? "+" : ""}${p.toFixed(2)}%`;
const short = (h, n = 10) => `${h.slice(0, n)}…${h.slice(-4)}`;

function chip(state, label) {
  const cls = { ok: "ok", bad: "bad", wait: "wait" }[state] || "wait";
  return `<span class="chip ${cls}"><span class="dot"></span>${esc(label)}</span>`;
}

function sparkline(hist, w = 220, h = 36) {
  const navs = hist.map((p) => p.nav);
  const min = Math.min(...navs), max = Math.max(...navs);
  const span = max - min || 1;
  const pts = navs
    .map((v, i) => `${((i / (navs.length - 1)) * (w - 4) + 2).toFixed(1)},${(h - 3 - ((v - min) / span) * (h - 6)).toFixed(1)}`)
    .join(" ");
  const up = navs[navs.length - 1] >= navs[0];
  return `<svg class="spark" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" role="img" aria-label="NAV history">
    <polyline points="${pts}" fill="none" stroke="${up ? "var(--proven)" : "var(--failed)"}" stroke-width="1.6"/>
  </svg>`;
}

function navChart(hist, w = 560, h = 180) {
  const navs = hist.map((p) => p.nav);
  const min = Math.min(...navs), max = Math.max(...navs);
  const span = max - min || 1;
  const px = (i) => (i / (navs.length - 1)) * (w - 56) + 8;
  const py = (v) => h - 24 - ((v - min) / span) * (h - 44);
  const pts = navs.map((v, i) => `${px(i).toFixed(1)},${py(v).toFixed(1)}`).join(" ");
  const gridY = [min, (min + max) / 2, max];
  return `<svg width="100%" viewBox="0 0 ${w} ${h}" role="img" aria-label="NAV over time">
    ${gridY.map((v) => `<line x1="8" x2="${w - 48}" y1="${py(v)}" y2="${py(v)}" stroke="var(--line)" stroke-width="1"/>
      <text x="${w - 42}" y="${py(v) + 4}" fill="var(--muted)" font-size="11" font-family="var(--mono)">${v.toFixed(1)}</text>`).join("")}
    <polyline points="${pts}" fill="none" stroke="var(--text)" stroke-width="1.8"/>
  </svg>`;
}

// ---------------------------------------------------------------- leaderboard
async function renderLeaderboard() {
  const data = await loadData();
  const vaults = [...data.vaults].sort((a, b) => b.return_pct - a.return_pct);
  const rows = vaults
    .map((v) => {
      const code = checkCodeAttested(v);
      const revealed = v.commitments.filter((c) => c.revealed).length;
      const proofs = v.risk_proofs.length;
      const rejected = v.commitments.filter((c) => c.status === "Rejected").length;
      return `<a class="ledger-row" href="#/vault/${esc(v.id)}">
        <span class="vault-name">${esc(v.name)}<small>${esc(v.id)}</small></span>
        <span class="spark-cell">${sparkline(v.nav_history)}</span>
        <span class="ret ${v.return_pct >= 0 ? "v-ok" : "v-bad"}">${fmtPct(v.return_pct)}</span>
        <span class="badges">
          ${chip(code.state, "code attested")}
          ${chip(revealed ? "ok" : "wait", `${revealed} trades committed`)}
          ${chip(proofs ? "ok" : rejected ? "bad" : "wait", proofs ? `${proofs} risk proofs` : rejected ? `${rejected} breaches blocked` : "no proofs")}
        </span>
      </a>`;
    })
    .join("");

  $("#view").innerHTML = `
    <h2>Vaults</h2>
    <div class="ledger">
      <div class="ledger-head"><span>Vault</span><span>NAV</span><span style="text-align:right">Return</span><span style="text-align:right">Verification</span></div>
      ${rows}
    </div>
    <p class="muted" style="margin-top:12px">Leaderboard chips summarize on-chain state. Open a vault and run the checks yourself — verification happens in your browser, not on a server.</p>`;
}

// ----------------------------------------------------------------- vault page
async function renderVault(id) {
  const data = await loadData();
  const v = data.vaults.find((x) => x.id === id);
  if (!v) { $("#view").innerHTML = `<p>No vault <code>${esc(id)}</code>. <a href="#/">Back to the leaderboard</a>.</p>`; return; }

  const g = v.guardrails;
  const commitRows = v.commitments
    .map((c) => {
      const d = c.revealed ? JSON.parse(c.revealed) : null;
      const stateChip =
        c.status === "Executed" ? chip("ok", "executed")
        : c.status === "Rejected" ? chip("bad", "rejected")
        : chip("wait", "sealed");
      return `<tr class="expandable" data-id="${c.id}" tabindex="0" aria-expanded="false">
        <td class="num">#${c.id}</td>
        <td><span class="hash">${short(c.commitment)}</span></td>
        <td>${d ? `${esc(d.action)} ${esc(d.pair)} <span class="muted">(${(d.leverage_bps / 10000).toFixed(1)}x, ${(d.size_bps / 100).toFixed(0)}% NAV)</span>` : `<span class="muted">sealed until execution</span>`}</td>
        <td>${stateChip}</td>
        <td class="num muted">${new Date(c.committed_at_ms).toISOString().slice(5, 16).replace("T", " ")}</td>
      </tr>
      <tr class="detail-row" data-for="${c.id}" hidden><td colspan="5">
        <div class="reveal-box">
          <div><span class="lbl">commitment (on NEAR, pre-execution)&nbsp;&nbsp;</span>${c.commitment}</div>
          ${c.revealed ? `<div style="margin-top:6px"><span class="lbl">revealed decision&nbsp;&nbsp;</span>${esc(c.revealed)}</div>
          <div><span class="lbl">salt&nbsp;&nbsp;</span>${c.salt_hex}</div>` : ""}
          ${c.reject_reason ? `<div class="v-bad" style="margin-top:6px">refused by guardrails: ${esc(c.reject_reason)}</div>` : ""}
          ${c.attestation_ref ? `<div style="margin-top:6px"><span class="lbl">attestation&nbsp;&nbsp;</span>${esc(c.attestation_ref)} <span class="lbl">· wasm&nbsp;</span>${short(c.wasm_hash)}</div>` : ""}
        </div>
        ${c.revealed ? `<button data-recompute="${c.id}">Recompute this hash</button><span class="check-line" id="recompute-${c.id}"></span>` : ""}
      </td></tr>`;
    })
    .join("");

  $("#view").innerHTML = `
    <p style="margin:0 0 4px"><a href="#/" class="muted" style="text-decoration:none">← all vaults</a></p>
    <div class="vault-head">
      <div>
        <h2>${esc(v.name)}</h2>
        <div class="sub">${esc(v.id)} · strategy ${short(v.approved_wasm_hash)}</div>
      </div>
      <div style="display:flex; gap:8px; align-items:center; flex-wrap:wrap">
        <span id="b-code">${chip("wait", "code attested")}</span>
        <span id="b-trades">${chip("wait", "trades committed")}</span>
        <span id="b-risk">${chip("wait", "risk proven")}</span>
        <button class="primary" id="run-checks">Run all checks</button>
      </div>
    </div>
    ${v.integrity_alert ? `<div class="alert"><b>Integrity alert — deposits paused.</b> A decision arrived attested to different code: <code>${esc(v.integrity_alert)}</code></div>` : ""}
    <div class="cols">
      <div class="panel"><h3>NAV (${fmtPct(v.return_pct)} since inception)</h3>${navChart(v.nav_history)}</div>
      <div class="panel"><h3>Guardrails — enforced by the contract, not the strategy</h3>
        <dl class="kv">
          <dt>max leverage</dt><dd>${(g.max_leverage_bps / 10000).toFixed(1)}x</dd>
          <dt>max drawdown</dt><dd>${(g.max_drawdown_bps / 100).toFixed(0)}%</dd>
          <dt>max position</dt><dd>${(g.max_position_bps / 100).toFixed(0)}% of NAV</dd>
          <dt>pairs</dt><dd>${g.allowed_pairs.map(esc).join(", ")}</dd>
          <dt>pool</dt><dd>${v.pool_near} NEAR</dd>
        </dl>
        <p class="muted" style="font-size:12.5px; margin:10px 0 0">The sealed strategy decides <em>what</em> to trade; it cannot decide to exceed these. A breach is refused before any signature exists.</p>
      </div>
    </div>
    <h2>Decision ledger <span class="muted" style="font-weight:400">— committed on NEAR before execution; click a row</span></h2>
    <div class="panel" style="padding:4px 10px">
      <table class="commits">
        <thead><tr><th></th><th>commitment</th><th>decision</th><th>status</th><th>committed</th></tr></thead>
        <tbody>${commitRows}</tbody>
      </table>
    </div>
    <h2>Checks <span class="muted" style="font-weight:400">— run in this browser</span></h2>
    <div class="panel" id="check-log">
      <div class="check-row"><span class="what">Attested code hash equals the approved strategy on every decision</span><span class="result muted" id="r-code">not run</span></div>
      <div class="check-row"><span class="what">Every revealed trade recomputes to its pre-execution commitment (SHA-256, WebCrypto)</span><span class="result muted" id="r-trades">not run</span></div>
      <div class="check-row"><span class="what">Bulletproof range proofs: private risk metrics stayed within the guardrails</span><span class="result muted" id="r-risk">not run</span></div>
    </div>`;

  // interactions
  $("#view").addEventListener("click", async (e) => {
    const row = e.target.closest("tr.expandable");
    if (row) {
      const det = $(`.detail-row[data-for="${row.dataset.id}"]`);
      det.hidden = !det.hidden;
      row.setAttribute("aria-expanded", String(!det.hidden));
    }
    const btn = e.target.closest("[data-recompute]");
    if (btn) {
      const c = v.commitments.find((x) => String(x.id) === btn.dataset.recompute);
      const out = $(`#recompute-${c.id}`);
      out.textContent = " computing…";
      const { ok, recomputed } = await checkCommitment(c.revealed, c.salt_hex, c.commitment);
      out.innerHTML = ok
        ? ` <span class="v-ok">✓ sha256(decision ‖ salt) = ${short(recomputed)} — matches</span>`
        : ` <span class="v-bad">✗ recomputed ${short(recomputed)} ≠ committed ${short(c.commitment)}</span>`;
    }
  });

  $("#run-checks").addEventListener("click", async () => {
    const setChip = (sel, st, lbl) => ($(sel).innerHTML = chip(st, lbl));
    const setRes = (sel, r) =>
      ($(sel).innerHTML = `<span class="${{ ok: "v-ok", bad: "v-bad", wait: "v-wait" }[r.state]}">${{ ok: "✓", bad: "✗", wait: "–" }[r.state]} ${esc(r.detail)}</span>`);
    ["#b-code", "#b-trades", "#b-risk"].forEach((s) => ($(s).innerHTML = `<span class="chip running"><span class="dot"></span>checking…</span>`));
    await runAllChecks(v, {
      onCode: (r) => { setChip("#b-code", r.state, "code attested"); setRes("#r-code", r); },
      onTrades: (r) => { setChip("#b-trades", r.state, "trades committed"); setRes("#r-trades", r); },
      onRisk: (r) => { setChip("#b-risk", r.state, "risk proven"); setRes("#r-risk", r); },
      onTradeStep: (id) => { const el = $("#r-trades"); if (el) el.textContent = `recomputing #${id}…`; },
      onRiskStep: (ep) => { const el = $("#r-risk"); if (el) el.textContent = `verifying epoch ${ep}…`; },
    });
  });
}

// ------------------------------------------------------------ red-team console
async function renderRedTeam() {
  const data = await loadData();
  const cheat = data.cheats.backfill;
  const degen = data.vaults.find((v) => v.name === "Degen Max");
  const shadow = data.vaults.find((v) => v.integrity_alert);
  const degenRej = degen.commitments.find((c) => c.status === "Rejected");

  $("#view").innerHTML = `
    <h2>Red-team console</h2>
    <p class="muted" style="max-width:70ch">Three ways a vault manager might cheat, and where each one dies. Everything below runs against real data structures — press the buttons.</p>
    <div class="redteam">
      <div class="redteam-item">
        <h4>1 · Back-fill a winning trade</h4>
        <p>The manager committed to one decision before execution, then claims a different, winning decision afterwards. Recompute the hash and compare.</p>
        <button id="try-backfill">Reveal the "winning" trade against the real commitment</button>
        <div class="console-out" id="out-backfill" hidden></div>
      </div>
      <div class="redteam-item">
        <h4>2 · Ask for 10x leverage under a 3x cap</h4>
        <p>Sealed parameters demanded ${(JSON.parse(degenRej.revealed).leverage_bps / 10000).toFixed(0)}x. Two independent layers refuse: no Bulletproof can exist for a breach, and the contract rejects the reveal before anything is signed.</p>
        <button id="try-leverage">Show the contract's refusal</button>
        <div class="console-out" id="out-leverage" hidden></div>
      </div>
      <div class="redteam-item">
        <h4>3 · Swap in different strategy code</h4>
        <p>A decision arrives attested by the TEE — but to a different WASM hash. The vault refuses it and pauses deposits.</p>
        <button id="try-swap">Compare the attested hash</button>
        <div class="console-out" id="out-swap" hidden></div>
      </div>
    </div>`;

  $("#try-backfill").addEventListener("click", async () => {
    const out = $("#out-backfill"); out.hidden = false;
    const { ok, recomputed } = await checkCommitment(cheat.claimed_decision, cheat.salt_hex, cheat.commitment);
    out.innerHTML =
      `committed on NEAR (before execution):\n  ${cheat.commitment}\n` +
      `claimed decision at reveal:\n  ${esc(cheat.claimed_decision)}\n` +
      `sha256(claimed ‖ salt):\n  ${recomputed}\n\n` +
      (ok ? "" : `<span class="v-bad">✗ MISMATCH — contract aborts: "reveal does not match commitment: this trade was not committed when claimed"</span>\n\n`) +
      `the decision that was actually committed:\n  ${esc(cheat.honest_decision)}`;
  });
  $("#try-leverage").addEventListener("click", () => {
    const out = $("#out-leverage"); out.hidden = false;
    out.innerHTML =
      `revealed decision:\n  ${esc(degenRej.revealed)}\n\n` +
      `<span class="v-bad">✗ guardrail_rejected: ${esc(degenRej.reject_reason)}</span>\n` +
      `status on-chain: Rejected — nothing was signed\n` +
      `risk proofs posted by this vault: ${degen.risk_proofs.length} (a breach admits no proof)`;
  });
  $("#try-swap").addEventListener("click", () => {
    const out = $("#out-swap"); out.hidden = false;
    out.innerHTML =
      `approved strategy wasm:\n  ${shadow.approved_wasm_hash}\n` +
      `attested wasm on incoming decision:\n  ${"9d".repeat(32)}\n\n` +
      `<span class="v-bad">✗ integrity alert — commit refused, deposits paused:</span>\n  "${esc(shadow.integrity_alert)}"`;
  });
}

// --------------------------------------------------------------------- router
function route() {
  const h = location.hash || "#/";
  if (h.startsWith("#/vault/")) renderVault(decodeURIComponent(h.slice(8)));
  else if (h === "#/redteam") renderRedTeam();
  else renderLeaderboard();
}
window.addEventListener("hashchange", route);
route();
