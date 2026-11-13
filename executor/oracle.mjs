// Price oracle. Whatever the enclave sees goes into its input, the input is
// published on the vault (`input_json`), and sha256(input) is in the TDX
// attestation (`input_hash`) — so anyone can re-check these prices against
// the public source at `as_of`. Feeding the strategy fake prices is detectable.
//
// Sources, in order: Pyth Hermes (spot) → CoinGecko (spot + series).

const PYTH = "https://hermes.pyth.network/v2/updates/price/latest";
const PYTH_IDS = {
  eth: "0xff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace",
  near: "0xc415de8d2eba7db216527dff4b60e8f3a5311c740dadb233e13e12547e226750",
};
const CG = "https://api.coingecko.com/api/v3";

async function getJson(url, tries = 3) {
  for (let i = 0; ; i++) {
    const res = await fetch(url, { headers: { accept: "application/json" } });
    if (res.ok) return res.json();
    // free-tier rate limits (429) are common: back off and retry
    if (i + 1 >= tries || (res.status !== 429 && res.status < 500)) throw new Error(`${res.status} ${url}`);
    await new Promise((r) => setTimeout(r, 2000 * 2 ** i));
  }
}

async function pythSpot() {
  const q = `?ids[]=${PYTH_IDS.eth}&ids[]=${PYTH_IDS.near}&parsed=true`;
  const d = await getJson(PYTH + q);
  const px = Object.fromEntries(
    d.parsed.map((p) => [p.id, Number(p.price.price) * 10 ** p.price.expo])
  );
  const t = Math.max(...d.parsed.map((p) => p.price.publish_time));
  return { eth_usd: px[PYTH_IDS.eth.slice(2)], near_usd: px[PYTH_IDS.near.slice(2)], as_of: t, source: "pyth-hermes" };
}

async function coingeckoSpot() {
  const d = await getJson(`${CG}/simple/price?ids=ethereum,near&vs_currencies=usd&include_last_updated_at=true`);
  return {
    eth_usd: d.ethereum.usd,
    near_usd: d.near.usd,
    as_of: d.ethereum.last_updated_at,
    source: "coingecko",
  };
}

/** Last `n` hourly ETH closes (oldest first) — the strategy's signal input. */
async function coingeckoSeries(n = 12) {
  const d = await getJson(`${CG}/coins/ethereum/market_chart?vs_currency=usd&days=1`);
  // ~5-min points for 1 day: take one per hour, ending at the latest
  const pts = d.prices;
  const hourly = [];
  for (let i = pts.length - 1; i >= 0 && hourly.length < n; i -= 12) hourly.unshift(pts[i][1]);
  return hourly.map((x) => Math.round(x * 100) / 100);
}

/** Everything the enclave needs, with provenance. */
export async function fetchMarket() {
  let spot;
  try {
    spot = await pythSpot();
  } catch {
    spot = await coingeckoSpot();
  }
  const prices = await coingeckoSeries(12);
  return {
    prices,
    oracle: {
      spot_source: spot.source,
      series_source: "coingecko market_chart (hourly, last 12)",
      eth_usd: spot.eth_usd,
      near_usd: spot.near_usd,
      as_of: spot.as_of,
    },
  };
}

/** Offline/test mode: deterministic. */
/** Deterministic demo series: rising (default, a buy signal) or falling (FIXED_TREND=down, a sell). */
export function fixedMarket(trend = process.env.FIXED_TREND ?? "up") {
  const up = [3000, 3002, 2998, 3004, 3010, 3025, 3042, 3060, 3081, 3105];
  const prices = trend === "down" ? [...up].reverse() : up;
  return {
    prices,
    oracle: { spot_source: "fixed-demo", series_source: "fixed-demo", eth_usd: prices[prices.length - 1], near_usd: 5, as_of: 0 },
  };
}
