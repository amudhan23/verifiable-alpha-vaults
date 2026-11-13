# Demo video storyboard (~2 min) + submission drafts

Record Saturday against the live frontend (`frontend/`, demo mode is fine;
live testnet vault in the URL bar is better). Screen + voiceover. Keep each
beat under 20 seconds.

## Storyboard

| # | Shot | Say roughly |
|---|------|-------------|
| 1 | Leaderboard, cursor over the three chips on **Momentum Alpha** | "Trading vaults can show you returns — screenshots. This one can *prove* them. Three claims: which code ran, every trade committed before execution, risk limits never broken." |
| 2 | Open Momentum Alpha → click **Run all checks** — let the badges visibly run and land green | "The checks run in *your* browser. SHA-256 over every revealed trade, Bulletproof range proofs over the risk metrics. No server to trust." |
| 3 | Expand one ledger row → **Recompute this hash** | "Every decision was hashed onto NEAR *before* it executed anywhere. Here's one, recomputed live." |
| 4 | Red-team console → cheat 1 (back-fill) | "So what if the manager lies? Try back-filling a winner: the hash doesn't recompute. The contract aborts — 'this trade was not committed when claimed.'" |
| 5 | Cheat 2 (10x leverage) | "Try 10x leverage under a 3x cap: the contract refuses before anything is signed — and mathematically, no range proof can even exist for a breach." |
| 6 | Cheat 3 (swapped code) + Shadow Fork's red badge/alert banner | "Swap in different strategy code: the TDX attestation hash doesn't match — deposits pause automatically." |
| 7 | Terminal: `node executor/run.mjs --enclave outlayer --vault near --execute evm` scrolling; cut to the vault-signed swap on Sepolia Etherscan (its calldata ends in the commitment) | "The strategy runs sealed in an Intel TDX enclave on NEAR's OutLayer. It decides the trade and its size, the vault contract itself builds and MPC-signs exactly that swap through Chain Signatures, and the relayer just broadcasts it." |
| 7b | Live vault page: *Terms of issue* → trust mode, and the closed deposit panel | "And we say plainly what the relayer is still trusted for: running on time and the chart. It can't size trades, price your deposit or cash a fee. Deposits only open when everything is back in NEAR. Attested mode removes even that, and it's built for when OutLayer's on-chain worker goes live." |
| 8 | Back to leaderboard | "The strategy's edge stays secret. Everything an investor needs is proven. Verifiable Alpha Vaults, on NEAR." |

Pre-record safety: capture shots 2–6 as backup GIFs in case of live hiccups.

## X post (follows the organizers' four tips: user problem first, one complete moment, NEAR's role concretely, a next step)

> AI fund managers ask you to trust them. This one can't lie.
>
> Built Verifiable Alpha Vaults at NEAR HACKATHON: the strategy's parameters stay sealed in Intel TDX, yet anyone can check which code ran, that every trade was committed before it executed, and that risk limits held. Bulletproofs, verified in your browser.
>
> What NEAR does here: OutLayer runs the sealed strategy, Chain Signatures lets the vault contract sign its own swaps, NEAR Intents brings the money back as NEAR.
>
> Try to cheat it yourself, then check every claim from your terminal: [repo] · [site]
> Would you put money in a vault you can verify but can't see inside?
>
> @NEARProtocol #NEARHackathon @NearLegion @NearBuilders

Video (60 to 90 seconds, one complete moment, no slides):

1. **(0–8s)** On screen: "AI fund managers say trust me. This one can't lie."
2. **(8–30s)** Open the vault page and press **Run all checks**. Each seal turns green; one sentence per seal.
3. **(30–50s)** Counterfeit Bureau: demand 10x leverage, which is refused; back-fill a trade, which is caught.
4. **(50–75s)** Sepolia Etherscan: the vault's own swap, with the calldata ending in the decision's hash. Say "sized inside the enclave, signed by the contract, through NEAR Chain Signatures".
5. **(75–90s)** Terminal: `node executor/verify.mjs`, ending in 6/6 passed. Close with the repo link.

## Submission description (20–2000 chars; this is ~1,300)

> **Verifiable Alpha Vaults** — AI trading vaults where the strategy's
> parameters stay sealed in an Intel TDX enclave (NEAR OutLayer), while
> everything an investor needs to check is provable: (1) *which code ran* —
> hardware attestation of the strategy WASM, enforced by the vault contract,
> which pauses deposits on a hash mismatch; (2) *no back-filled trades* —
> every decision is hash-committed on NEAR before execution; (3) *risk limits
> held* — guardrails enforced on-chain before any signature exists, plus
> Bulletproofs range proofs over private risk metrics, verified in the
> browser; (4) *real returns* — on-chain share accounting at NAV per share.
> The vault contract builds and MPC-signs its own Uniswap swaps via Chain
> Signatures, using exactly the size and 2% floor the enclave committed, to a
> wallet that can never change. Two honest trust modes: relayed (live; the
> relayer is trusted only for liveness and the chart — deposits open only
> while fully in NEAR, fees only on realized gains) and attested (built; only
> TDX-attested on-chain output can act). Two independent reviews, fuzzed
> invariants, timelocked upgrades. The protected asset is the parameters and
> future decisions. Try the red-team console: back-fill a winner, demand 10x
> leverage, or swap the code — and watch each cheat die.

## Tracks

Main: **Unstoppable DeFi**. Also: Privacy & Private AI (sealed params +
private risk metrics), AI Security & Defense (guardrails + attestation +
integrity alerts), Agent Economy (autonomous strategy agent with provable
conduct). **NFTs**: the strategy is a NEP-171 NFT; its holder earns the
fees and proposes changes, and proposals lapse if it is sold.

## Submission checklist (Sat, aim 4 PM ET, hard stop 6:59 PM ET)

- [ ] after 17:51 UTC: `apply_strategy` (anyone), then one live cycle with a vault-signed, enclave-sized swap
- [ ] final contract deploy, then **delete the vault account's full-access key**; confirm `view_access_key_list` is empty
- [ ] repo public, fresh `git clone` → README one-command run works
- [ ] testnet: vault deployed, OutLayer project live, ≥2 attested cycles
- [ ] frontend `?vault=<contract>` live mode shown in video
- [ ] video uploaded, X post live with tags, link in submission
- [ ] NEAR wallet address in the form
- [ ] README "what is protected" section final (judges will probe this)
