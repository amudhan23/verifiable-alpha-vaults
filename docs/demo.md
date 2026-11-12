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
| 7 | Terminal: `node executor/run.mjs` scrolling; cut to OutLayer attestation JSON (from the go/no-go call_id) | "The strategy itself runs sealed in an Intel TDX enclave on NEAR's OutLayer — parameters enter encrypted, only proofs come out. Execution crosses chains through NEAR Chain Signatures." |
| 8 | Back to leaderboard | "The strategy's edge stays secret. Everything an investor needs is proven. Verifiable Alpha Vaults, on NEAR." |

Pre-record safety: capture shots 2–6 as backup GIFs in case of live hiccups.

## X post (tag @NEARProtocol, #NEARHackathon, attach video)

> Trading vaults that prove they're honest — without revealing the strategy.
>
> Params sealed in a TDX enclave (OutLayer). Every trade hash-committed on @NEARProtocol before execution. Leverage/drawdown limits proven with Bulletproofs — verified in your browser. Cross-chain via Chain Signatures.
>
> Try to cheat it 👇 #NEARHackathon
>
> [video] · github.com/amudhan23/verifiable-alpha-vaults

## Submission description (20–2000 chars; this is ~900)

> **Verifiable Alpha Vaults** — AI trading vaults where the strategy's
> parameters stay sealed in an Intel TDX enclave (NEAR OutLayer), while
> everything an investor needs to check is provable: (1) *which code ran* —
> hardware attestation of the strategy WASM, enforced by the vault contract,
> which pauses deposits on a hash mismatch; (2) *no back-filled trades* —
> every decision is hash-committed on NEAR before execution and revealed with
> its salt afterwards; (3) *risk limits held* — guardrails (leverage ≤ 3x,
> position caps, asset allowlist) enforced on-chain before any signature
> exists, plus Bulletproofs range proofs that private risk metrics stayed in
> range, verified in the browser; (4) *real returns* — on-chain share
> accounting. Execution crosses chains via Chain Signatures. The protected
> asset is the parameters and future decisions; past trades are public by
> design. Includes a red-team console: back-fill a winner, demand 10x
> leverage, or swap the strategy code — and watch each cheat die.

## Tracks

Main: **Unstoppable DeFi**. Also: Privacy & Private AI (sealed params +
private risk metrics), AI Security & Defense (guardrails + attestation +
integrity alerts), Agent Economy (autonomous strategy agent with provable
conduct). NFTs only if the strategy-NFT layer ships.

## Submission checklist (Sat, aim 4 PM ET, hard stop 6:59 PM ET)

- [ ] repo public, fresh `git clone` → README one-command run works
- [ ] testnet: vault deployed, OutLayer project live, ≥2 attested cycles
- [ ] frontend `?vault=<contract>` live mode shown in video
- [ ] video uploaded, X post live with tags, link in submission
- [ ] NEAR wallet address in the form
- [ ] README "what is protected" section final (judges will probe this)
