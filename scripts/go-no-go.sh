#!/usr/bin/env bash
# =============================================================================
# THURSDAY GO/NO-GO: can OutLayer run our strategy WASM with a sealed secret
# and return a TDX attestation on testnet, fast enough for a live demo?
#
# PASS criteria (all three):
#   1. call completes with status=completed and sane output (commitment + sealed decision)
#   2. attestation is fetchable at /attestations/by-call/{call_id}
#   3. end-to-end latency < 60s (ideally < 10s)
#
# If this fails by NOON Thursday -> switch to fallback idea "Proof of Goal".
#
# One-time setup first (see scripts/outlayer-setup.md):
#   - deploy the strategy project on OutLayer testnet
#   - store secret STRATEGY_PARAMS in profile 'default' at app.outlayer.ai/secrets
#   - create a testnet Payment Key
#
# Usage:
#   OUTLAYER_PAYMENT_KEY="owner:nonce:secret" \
#   PROJECT_OWNER="amudhan.testnet" PROJECT_NAME="alpha-vault-strategy" \
#   ./scripts/go-no-go.sh
# =============================================================================
set -euo pipefail

BASE="${OUTLAYER_BASE:-https://testnet-api.outlayer.ai}"
OWNER="${PROJECT_OWNER:?set PROJECT_OWNER (your NEAR testnet account)}"
PROJECT="${PROJECT_NAME:-alpha-vault-strategy}"
KEY="${OUTLAYER_PAYMENT_KEY:?set OUTLAYER_PAYMENT_KEY (owner:nonce:secret)}"

INPUT='{
  "epoch": 1, "nonce": 1, "pair": "ETH/USDC",
  "prices": [100,100,100,100,100,102,105,109,114,120],
  "nav_yocto": "990", "peak_nav_yocto": "1000",
  "max_leverage_bps": 30000, "max_drawdown_bps": 2000
}'

BODY=$(jq -n --argjson input "$INPUT" --arg owner "$OWNER" '{
  input: $input,
  secrets_ref: { profile: "default", account_id: $owner },
  async: false
}')

echo "== 1/3 submitting execution to $BASE/call/$OWNER/$PROJECT =="
T0=$(date +%s.%N)
RESP=$(curl -sS -m 120 -X POST "$BASE/call/$OWNER/$PROJECT" \
  -H "X-Payment-Key: $KEY" -H "Content-Type: application/json" \
  -d "$BODY")
T1=$(date +%s.%N)
LATENCY=$(echo "$T1 - $T0" | bc)
echo "$RESP" | jq .

STATUS=$(echo "$RESP" | jq -r .status)
CALL_ID=$(echo "$RESP" | jq -r .call_id)
if [[ "$STATUS" != "completed" ]]; then
  echo "❌ NO-GO: status=$STATUS (error: $(echo "$RESP" | jq -r '.error // "n/a"'))"
  exit 1
fi

echo "== 2/3 checking output shape (commitment + sealed decision + risk bundle) =="
OUTPUT=$(echo "$RESP" | jq -r .output)
echo "$OUTPUT" | jq -e '.commitment and .sealed.decision_json and .sealed.salt_hex' >/dev/null \
  || { echo "❌ NO-GO: output missing commitment/sealed fields"; echo "$OUTPUT"; exit 1; }
echo "$OUTPUT" | jq '{commitment: .commitment, decision: (.sealed.decision_json | fromjson), risk_proof: (.risk_bundle != null)}'

# local re-check: sha256(decision_json || salt) == commitment
DEC=$(echo "$OUTPUT" | jq -r .sealed.decision_json)
SALT=$(echo "$OUTPUT" | jq -r .sealed.salt_hex)
RECOMPUTED=$(python3 -c "import hashlib,sys; print(hashlib.sha256(sys.argv[1].encode()+bytes.fromhex(sys.argv[2])).hexdigest())" "$DEC" "$SALT")
CLAIMED=$(echo "$OUTPUT" | jq -r .commitment)
[[ "$RECOMPUTED" == "$CLAIMED" ]] \
  && echo "✓ commitment recomputes locally" \
  || { echo "❌ NO-GO: commitment mismatch ($RECOMPUTED != $CLAIMED)"; exit 1; }

echo "== 3/3 fetching TDX attestation =="
for i in 1 2 3 4 5 6; do
  ATT=$(curl -sS -m 30 -H "X-Payment-Key: $KEY" "$BASE/attestations/by-call/$CALL_ID" || true)
  if echo "$ATT" | jq -e . >/dev/null 2>&1 && [[ "$(echo "$ATT" | jq -r 'has("error") | not')" == "true" ]]; then
    break
  fi
  echo "  attestation not ready yet (attempt $i), waiting 10s..."
  sleep 10
done
echo "$ATT" | jq 'with_entries(if (.value|type)=="string" and (.value|length)>120 then .value = (.value[:120]+"...") else . end)' \
  || { echo "❌ NO-GO: attestation not fetchable for call $CALL_ID"; exit 1; }

echo
echo "=============================================="
printf "✅ GO — completed in %.1fs, attestation fetched.\n" "$LATENCY"
echo "   call_id: $CALL_ID"
echo "   (record this call_id: it's the first attested run for the demo)"
awk -v l="$LATENCY" 'BEGIN { if (l > 60) print "⚠️  but latency > 60s: pre-run executions before the live demo" }'
echo "=============================================="
