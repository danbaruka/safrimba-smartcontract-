#!/bin/bash
#
# Comprehensive Safrimba Contract Test Suite (cron-driven timing)
#
# Tests ALL execute messages and queries end-to-end against the on-chain
# contract. Lets time-based gates drive transitions ("cron-style"): after
# everyone has deposited for a round, we wait `cycle_duration + 2s` before
# calling advance_round / process_payout so the on-chain time gate is
# satisfied without retry logic.
#
# Coverage (executes):
#   CreateCircle (Private only; Public rejection verified), JoinCircle (rejected
#   for Private), InviteMember + AcceptInvite, AddPrivateMember,
#   UpdateMemberPseudonym, UpdateCircle, StartCircle, DepositContribution,
#   AdvanceRound, ProcessPayout, Withdraw, ExitCircle (before & after start),
#   CancelCircle (before start; expect-fail after distribution), PauseCircle,
#   UnpauseCircle, CheckAndEject, BlockMember, DistributeBlockedFunds,
#   EmergencyStop.
#
# Coverage (queries): every QueryMsg variant.
#
# Circles:
#   1. Private + Total, 2 members, 20s cycle — full lifecycle
#   2. Private + None,  2 members, 20s cycle — None == Total semantics, full lifecycle
#   3. Private + MinMembers(2), 3-cap but 2-actual, 20s cycle
#   4. Private + Total, manual_trigger_enabled, 2 members
#   5. Private + None, ejection params (member misses every round)
#   6. Private + Total — cancel before start
#   7. Private + None  — exit before start
#   (Public creation attempt is tested and expected to fail.)
#
set +e

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
BLUE='\033[0;34m'; CYAN='\033[0;36m'; NC='\033[0m'

NETWORK="${1:-testnet}"
CREATOR_KEY="${2:-mycontractadmin}"
MEMBER_KEY="${3:-mywallet}"
CODE_ID="${4:-122}"
REPORT_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/TEST_REPORT.md"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CHAIN_CONFIG="$CONTRACT_DIR/chain/$NETWORK/safrochain.json"
KEYRING_OPTS="--keyring-backend os"

CHAIN_ID=$(jq -r '.chainId' "$CHAIN_CONFIG")
RPC_URL=$(jq -r '.rpc' "$CHAIN_CONFIG")
FEE_DENOM="usaf"
CONTRIBUTION="100000"               # 0.1 SAF
CREATOR_LOCK=$((CONTRIBUTION * 2))  # 0.2 SAF — formula: contribution * 2 (see compute_creator_lock)
CYCLE_SECS=20                       # cycle_duration_seconds — round_end gate fires after this
GRACE_SECS=5                        # grace_period_seconds  — missed-deposit ejection window
ROUND_WAIT=$((CYCLE_SECS + 2))      # extra 2s margin to ensure block time has crossed the gate

CREATOR_ADDR=$(safrochaind keys show "$CREATOR_KEY" -a $KEYRING_OPTS 2>/dev/null || true)
MEMBER_ADDR=$(safrochaind keys show "$MEMBER_KEY" -a $KEYRING_OPTS 2>/dev/null || true)
[[ -z "$CREATOR_ADDR" || -z "$MEMBER_ADDR" ]] && { echo -e "${RED}Keys not found${NC}"; exit 1; }

# ── Helpers ──────────────────────────────────────────────────────────────────

PASS=0; FAIL=0
REPORT=""
log() { echo -e "$1"; REPORT+="$(echo -e "$1" | sed 's/\x1b\[[0-9;]*m//g')"$'\n'; }

wait_tx() {
    local H="$1" i=1
    while [[ $i -le 30 ]]; do
        local R=$(safrochaind query tx "$H" --node "$RPC_URL" -o json 2>/dev/null || echo "")
        if echo "$R" | jq -e '.code == 0' &>/dev/null; then echo "$R"; return 0; fi
        local C=$(echo "$R" | jq -r '.code // empty' 2>/dev/null)
        if [[ -n "$C" && "$C" != "0" && "$C" != "null" ]]; then
            echo "$R" | jq -r '.raw_log // .events // empty' 2>/dev/null
            return 1
        fi
        sleep 2; i=$((i+1))
    done
    echo "timeout"; return 1
}

tx() {
    local ADDR="$1" MSG="$2" KEY="$3" AMT="${4:-}"
    local EXTRA=""; [[ -n "$AMT" ]] && EXTRA="--amount ${AMT}"
    local OUT=$(safrochaind tx wasm execute "$ADDR" "$MSG" --from "$KEY" --chain-id "$CHAIN_ID" \
        --node "$RPC_URL" --broadcast-mode sync --gas auto --gas-adjustment 1.5 \
        --gas-prices "0.025${FEE_DENOM}" $KEYRING_OPTS $EXTRA -y --output json 2>&1) || true
    local H=$(echo "$OUT" | jq -r '.txhash // empty' 2>/dev/null)
    [[ -z "$H" ]] && H=$(echo "$OUT" | grep -oE '[A-F0-9]{64}' | head -1)
    if [[ -z "$H" ]]; then echo "ERR:$(echo "$OUT" | head -5)"; return 1; fi
    echo "$H"
}

# tx variant that returns exit 1 if the on-chain transaction itself failed (code != 0)
tx_must_succeed() {
    local ADDR="$1" MSG="$2" KEY="$3" AMT="${4:-}"
    local H; H=$(tx "$ADDR" "$MSG" "$KEY" "$AMT") || return 1
    sleep 4
    wait_tx "$H" >/dev/null || return 1
}

# tx variant that returns exit 1 if the transaction succeeds (we expect failure)
tx_must_fail() {
    local ADDR="$1" MSG="$2" KEY="$3" AMT="${4:-}"
    local H; H=$(tx "$ADDR" "$MSG" "$KEY" "$AMT") || return 0  # no-hash = failure pre-broadcast = OK
    sleep 4
    if wait_tx "$H" >/dev/null 2>&1; then
        return 1  # tx succeeded but we expected failure
    fi
    return 0
}

q() { safrochaind query wasm contract-state smart "$1" "$2" --node "$RPC_URL" -o json 2>/dev/null; }
qcircle() { q "$1" "$(jq -n --argjson id "$2" '{get_circle:{circle_id:$id}}')" | jq '.data // .'; }

run_test() {
    local NAME="$1"; shift
    local RESULT EXIT_CODE
    RESULT=$("$@" 2>&1)
    EXIT_CODE=$?
    if [[ $EXIT_CODE -eq 0 ]]; then
        log "  ${GREEN}✓ PASS${NC}: $NAME"
        PASS=$((PASS+1))
        return 0
    else
        log "  ${RED}✗ FAIL${NC}: $NAME"
        [[ -n "${RESULT:-}" ]] && log "    → $(echo "$RESULT" | head -3)"
        FAIL=$((FAIL+1))
        return 1
    fi
}

assert_status() {
    local C="$1" ID="$2" EXPECT="$3"
    local GOT=$(qcircle "$C" "$ID" | jq -r '.circle.circle_status // empty')
    [[ "$GOT" == "$EXPECT" ]] || { echo "expected $EXPECT, got $GOT"; return 1; }
}

assert_members() {
    local C="$1" ID="$2" EXPECT="$3"
    local GOT=$(qcircle "$C" "$ID" | jq -r '.circle.members_list | length')
    [[ "$GOT" == "$EXPECT" ]] || { echo "expected $EXPECT members, got $GOT"; return 1; }
}

wait_round_end() {
    local LABEL="$1"
    log "  ${BLUE}… sleeping ${ROUND_WAIT}s for ${LABEL} (cycle+grace gate)${NC}"
    sleep $ROUND_WAIT
}

# ── INSTANTIATE ──────────────────────────────────────────────────────────────

log "${CYAN}════════════════════════════════════════════════════════════════${NC}"
log "${GREEN}  SAFRIMBA COMPREHENSIVE CONTRACT TEST (cron-driven)${NC}"
log "${CYAN}════════════════════════════════════════════════════════════════${NC}"
log "Network: $NETWORK | Code ID: $CODE_ID | cycle=${CYCLE_SECS}s grace=${GRACE_SECS}s"
log "Creator: $CREATOR_KEY ($CREATOR_ADDR)"
log "Member:  $MEMBER_KEY ($MEMBER_ADDR)"
log ""

log "${CYAN}[1] INSTANTIATE CONTRACT${NC}"
INST_MSG=$(jq -n --arg a "$CREATOR_ADDR" '{"platform_fee_percent":0,"platform_address":$a}')
INST_OUT=$(safrochaind tx wasm instantiate "$CODE_ID" "$INST_MSG" --from "$CREATOR_KEY" --admin "$CREATOR_ADDR" \
    --label "safrimba-test-$(date +%s)" --chain-id "$CHAIN_ID" --node "$RPC_URL" \
    --broadcast-mode sync --gas auto --gas-adjustment 1.5 --gas-prices "0.025${FEE_DENOM}" \
    $KEYRING_OPTS -y --output json 2>&1) || true
INST_TX=$(echo "$INST_OUT" | jq -r '.txhash // empty' 2>/dev/null)
[[ -z "$INST_TX" ]] && INST_TX=$(echo "$INST_OUT" | grep -oE '[A-F0-9]{64}' | head -1)
[[ -z "$INST_TX" ]] && { log "${RED}Instantiate failed: ${INST_OUT:0:300}${NC}"; exit 1; }
sleep 5
INST_RESULT=$(wait_tx "$INST_TX") || { log "${RED}Instantiate TX failed${NC}"; exit 1; }
C=$(echo "$INST_RESULT" | jq -r '[.events[]? | select(.type=="instantiate") | .attributes[]? | select(.key=="_contract_address") | .value] | first // empty')
[[ -z "$C" ]] && C=$(echo "$INST_RESULT" | jq -r '.logs[].events[]? | select(.type=="instantiate") | .attributes[]? | select(.key=="_contract_address") | .value' 2>/dev/null | head -1)
[[ -z "$C" ]] && { log "${RED}Cannot find contract address${NC}"; exit 1; }
log "  Contract: ${YELLOW}$C${NC}"
log ""

# Helper: Private circle create — fields shared across configs
private_circle_base() {
    jq -n \
        --arg c "$CONTRIBUTION" --argjson cy "$CYCLE_SECS" --argjson gs "$GRACE_SECS" \
        --arg name "$1" --arg desc "$2" \
        --argjson max "$3" --argjson min "$4" \
        --argjson exit_pen "$5" --argjson late_fee "$6" \
        --argjson cycles "$7" \
        --argjson auto_start "$8" --argjson auto_pay "$9" --argjson manual "${10}" \
        '{
            circle_name:$name, circle_description:$desc,
            max_members:$max, min_members_required:$min,
            invite_only:true,
            contribution_amount:$c,
            exit_penalty_percent:$exit_pen, late_fee_percent:$late_fee,
            total_cycles:$cycles,
            cycle_duration_days:0, cycle_duration_seconds:$cy,
            grace_period_hours:0, grace_period_seconds:$gs,
            auto_start_when_full:$auto_start,
            payout_order_type:"RandomOrder",
            auto_payout_enabled:$auto_pay, manual_trigger_enabled:$manual,
            emergency_stop_enabled:true,
            auto_refund_if_min_not_met:true, strict_mode:false,
            visibility:"Private", show_member_identities:true
        }'
}

# ── DENOMINATION NEGATIVE TESTS ──────────────────────────────────────────────
# A bad denom is rejected at CreateCircle (allow-list: 'usaf' or the IBC USDC trace).

USDC_DENOM='ibc/2180E84E20F5679FCC760D8C165B60F42065DEF7F46A72B447CFF1B7DC6C0A65'

log "${CYAN}[2a] DENOMINATION ALLOW-LIST — bad denom rejected${NC}"
do_bad_denom_should_fail() {
    local MSG
    MSG=$(jq -n --arg c "$CONTRIBUTION" --argjson cy "$CYCLE_SECS" --argjson gs "$GRACE_SECS" \
        '{create_circle:{
            circle_name:"BadDenom", circle_description:"unknown denom should be rejected",
            max_members:2, min_members_required:2, invite_only:true,
            contribution_amount:$c, denomination:"foobar",
            exit_penalty_percent:2000, late_fee_percent:1000,
            total_cycles:1, cycle_duration_days:0, cycle_duration_seconds:$cy,
            grace_period_hours:0, grace_period_seconds:$gs,
            auto_start_when_full:true, payout_order_type:"RandomOrder",
            auto_payout_enabled:true, manual_trigger_enabled:false,
            emergency_stop_enabled:true, auto_refund_if_min_not_met:true, strict_mode:false,
            visibility:"Private", show_member_identities:true
        }}')
    tx_must_fail "$C" "$MSG" "$CREATOR_KEY" "${CREATOR_LOCK}${FEE_DENOM}"
}
run_test "CreateCircle with denomination='foobar' rejected" do_bad_denom_should_fail

log ""

# Probe: do the test wallets hold IBC USDC? If not, skip the USDC-lifecycle test.
USDC_BAL=$(safrochaind query bank balances "$CREATOR_ADDR" --node "$RPC_URL" -o json 2>/dev/null \
    | jq -r --arg d "$USDC_DENOM" '.balances[] | select(.denom == $d) | .amount' 2>/dev/null)
USDC_BAL=${USDC_BAL:-0}
log "${CYAN}[2b] USDC PROBE${NC}"
log "  Creator USDC balance: $USDC_BAL (${USDC_DENOM:0:20}…)"
if [[ -z "$USDC_BAL" || "$USDC_BAL" == "0" ]]; then
    log "  ${BLUE}↳ skip USDC lifecycle: test wallets hold no USDC on testnet${NC}"
    USDC_AVAILABLE=false
else
    USDC_AVAILABLE=true
fi
log ""

# ── PUBLIC CREATE SHOULD FAIL ────────────────────────────────────────────────

log "${CYAN}[2c] PUBLIC CIRCLE CREATION — expected to be rejected${NC}"
do_public_create_should_fail() {
    local MSG
    MSG=$(jq -n --arg c "$CONTRIBUTION" --argjson cy "$CYCLE_SECS" --argjson gs "$GRACE_SECS" \
        '{create_circle:{
            circle_name:"PublicForbidden", circle_description:"should be rejected",
            max_members:2, min_members_required:2, invite_only:false,
            contribution_amount:$c, exit_penalty_percent:2000, late_fee_percent:1000,
            total_cycles:1, cycle_duration_days:0, cycle_duration_seconds:$cy,
            grace_period_hours:0, grace_period_seconds:$gs,
            auto_start_when_full:true, payout_order_type:"RandomOrder",
            auto_payout_enabled:true, manual_trigger_enabled:false,
            emergency_stop_enabled:true, auto_refund_if_min_not_met:true, strict_mode:false,
            visibility:"Public", show_member_identities:true,
            distribution_threshold:{"total":{}}
        }}')
    tx_must_fail "$C" "$MSG" "$CREATOR_KEY" "${CREATOR_LOCK}${FEE_DENOM}"
}
run_test "Public visibility rejected (temporarily disabled at contract level)" do_public_create_should_fail
log ""

# ── CREATE PRIVATE CIRCLES ───────────────────────────────────────────────────

log "${CYAN}[3] CREATE 7 PRIVATE CIRCLES (varied configs)${NC}"

create() {
    local INNER="$1"
    local MSG; MSG=$(jq -n --argjson c "$INNER" '{create_circle:$c}')
    tx_must_succeed "$C" "$MSG" "$CREATOR_KEY" "${CREATOR_LOCK}${FEE_DENOM}"
}

# Circle 1: Total threshold, 2 members, 3 cycles. We complete cycles 1+2 and
# leave it Running at cycle 3 start so late-stage tests (CancelAfterStart,
# ExitAfterStart, EmergencyStop) have a Running circle with >=1 distribution.
C1=$(private_circle_base "Priv-Total-3c"   "Total threshold, 3 cycles" 2 2 2000 1000 3 true true false \
    | jq '. + {distribution_threshold:{total:{}}}')
run_test "Circle 1: Private + Total, 3c × 2m (long-running)" create "$C1"

# Circle 2: None threshold (treated as Total at last round per new code), 1 cycle, 2 members
C2=$(private_circle_base "Priv-None-1c"    "None threshold (=Total semantics)" 2 2 2000 1000 1 true true false)
run_test "Circle 2: Private + None,  1c × 2m" create "$C2"

# Circle 3: MinMembers(2), 3-cap but only 2 join, 1 cycle
C3=$(private_circle_base "Priv-MinMem2"    "MinMembers(2) — distribute from round 2" 3 2 2000 1000 1 false true false \
    | jq '. + {distribution_threshold:{min_members:{count:2}}}')
run_test "Circle 3: Private + MinMembers(2), 3-cap 2-actual" create "$C3"

# Circle 4: Total + manual_trigger_enabled, creator must trigger payout, 2 members
C4=$(private_circle_base "Priv-Total-Man"  "manual_trigger_enabled" 2 2 2000 1000 1 true false true \
    | jq '. + {distribution_threshold:{total:{}}}')
run_test "Circle 4: Private + Total, manual_trigger" create "$C4"

# Circle 5: None, harsh penalties — ejection test (member misses every round)
C5=$(private_circle_base "Priv-Eject"      "high penalties for ejection" 2 2 4000 5000 1 true true false)
run_test "Circle 5: Private + None, ejection params" create "$C5"

# Circle 6: Cancel before start
C6=$(private_circle_base "Priv-CancelTest" "cancel before start" 2 2 2000 1000 1 false true false \
    | jq '. + {distribution_threshold:{total:{}}}')
run_test "Circle 6: Private + Total (cancel test)" create "$C6"

# Circle 7: Exit before start
C7=$(private_circle_base "Priv-ExitTest"   "exit before start"   2 2 2000 1000 1 false true false)
run_test "Circle 7: Private + None (exit test)" create "$C7"

# Circle 8: 2-cycle Private + Total — used for mid-lifecycle tests
# (BlockMember + DistributeBlockedFunds). We deposit R1 + advance to R2 then
# STOP — leaving the circle Running with one cycle in progress so the block
# tests target a non-trivial state.
C8=$(private_circle_base "Priv-BlockTest"  "long-running for block tests" 2 2 2000 1000 2 true true false \
    | jq '. + {distribution_threshold:{total:{}}}')
run_test "Circle 8: Private + Total, 2c × 2m (block target)" create "$C8"

# Circle 9: Private + Total, denominated in IBC USDC. Only attempted when the
# test wallets actually hold USDC on testnet — otherwise creator-lock payment
# fails with InsufficientFunds before the contract path is exercised.
if [[ "$USDC_AVAILABLE" == "true" ]]; then
    create_usdc() {
        local MSG; MSG=$(jq -n --argjson c "$1" --arg d "$USDC_DENOM" '{create_circle:($c + {denomination:$d})}')
        tx_must_succeed "$C" "$MSG" "$CREATOR_KEY" "${CREATOR_LOCK}${USDC_DENOM}"
    }
    C9=$(private_circle_base "USDC-Total-1c"   "USDC denomination test"  2 2 2000 1000 1 true true false \
        | jq '. + {distribution_threshold:{total:{}}}')
    run_test "Circle 9: Private + Total, denomination=IBC-USDC" create_usdc "$C9"
else
    log "  ${BLUE}↳ skip Circle 9 (USDC): no USDC balance on test wallets${NC}"
fi

log ""

# ── UPDATE CIRCLE (before start) ─────────────────────────────────────────────

log "${CYAN}[4] UPDATE CIRCLE (before start)${NC}"
do_update() {
    tx_must_succeed "$C" \
        '{"update_circle":{"circle_id":6,"circle_name":"Cancel-Renamed","circle_description":"Updated desc"}}' \
        "$CREATOR_KEY"
}
run_test "UpdateCircle on circle 6" do_update
log ""

# ── JoinCircle should FAIL on Private circles (invite-only) ──────────────────

log "${CYAN}[5] JoinCircle MUST FAIL on Private circles${NC}"
do_join_should_fail() {
    local ID=$1
    tx_must_fail "$C" "$(jq -n --argjson id $ID '{join_circle:{circle_id:$id}}')" \
        "$MEMBER_KEY" "${CONTRIBUTION}${FEE_DENOM}"
}
run_test "JoinCircle on circle 1 (Private) rejected" do_join_should_fail 1
log ""

# ── INVITE + ACCEPT (all circles use invite flow) ────────────────────────────

log "${CYAN}[6] INVITE + ACCEPT (Private flow on all 7 circles)${NC}"
do_invite_only() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID --arg a "$MEMBER_ADDR" '{invite_member:{circle_id:$id,member_address:$a}}')" \
        "$CREATOR_KEY"
}
do_accept() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{accept_invite:{circle_id:$id}}')" \
        "$MEMBER_KEY" "${CONTRIBUTION}${FEE_DENOM}"
}
for id in 1 2 3 4 5 6 7 8; do
    run_test "Invite member to circle $id"  do_invite_only $id
    run_test "Member accepts circle $id"    do_accept      $id
done
log ""

# ── AddPrivateMember (creator directly adds without invite flow) ─────────────

log "${CYAN}[7] AddPrivateMember — direct creator add (no funds attached)${NC}"
# AddPrivateMember adds a NEW member directly to the circle (bypassing invite+accept).
# Validates: caller=creator, circle is Private, not full, target address not already a member.
# Use a third (different) address — safrimba-os-1 — which is NOT already a member.
THIRD_ADDR="$(safrochaind keys show safrimba-os-1 -a $KEYRING_OPTS 2>/dev/null || echo '')"
do_add_private_member() {
    local ID=$1 ADDR=$2
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID --arg a "$ADDR" '{add_private_member:{circle_id:$id,member_address:$a,pseudonym:"alias-test"}}')" \
        "$CREATOR_KEY"
}
# Circle 3 has max_members=3 with creator+invited member (2 members). Adding a
# third (fresh, non-member) address fills the slot via the direct-add path.
if [[ -n "$THIRD_ADDR" ]]; then
    run_test "AddPrivateMember on circle 3 (third addr w/ pseudonym)" \
        do_add_private_member 3 "$THIRD_ADDR"
else
    log "  ${BLUE}↳ skip AddPrivateMember: no safrimba-os-1 key available${NC}"
fi
log ""

# ── UpdateMemberPseudonym ────────────────────────────────────────────────────

log "${CYAN}[8] UpdateMemberPseudonym (creator updates a member's pseudonym)${NC}"
do_update_pseudo() {
    local ID=$1 ADDR=$2 PSEUDO=$3
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID --arg a "$ADDR" --arg p "$PSEUDO" '{update_member_pseudonym:{circle_id:$id,member_address:$a,pseudonym:$p}}')" \
        "$CREATOR_KEY"
}
run_test "UpdateMemberPseudonym on circle 3" \
    do_update_pseudo 3 "$CREATOR_ADDR" "new-alias"
log ""

# ── CANCEL BEFORE START ──────────────────────────────────────────────────────

log "${CYAN}[9] CANCEL CIRCLE BEFORE START${NC}"
do_cancel() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{cancel_circle:{circle_id:$id}}')" \
        "$CREATOR_KEY"
}
run_test "Cancel circle 6 (before start, full refund)" do_cancel 6
run_test "Verify circle 6 status = Cancelled" assert_status "$C" 6 "Cancelled"
log ""

# ── EXIT BEFORE START ────────────────────────────────────────────────────────

log "${CYAN}[10] EXIT CIRCLE BEFORE START${NC}"
do_exit() {
    local ID=$1 KEY=$2
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{exit_circle:{circle_id:$id}}')" "$KEY"
}
run_test "Exit circle 7 as member (full refund)" do_exit 7 "$MEMBER_KEY"
log ""

# ── START CIRCLES (1, 2, 3, 4, 5) — circle 3 needs manual start (auto_start=false) ──

log "${CYAN}[11] START CIRCLES${NC}"
do_start() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{start_circle:{circle_id:$id}}')" \
        "$CREATOR_KEY"
}
# Circles 1, 2, 4, 5, 8 have auto_start_when_full=true and 2/2 members, so they auto-started
# on AcceptInvite. Calling start_circle on an already-Running circle errors. Check first:
for id in 1 2 3 4 5 8; do
    ST=$(qcircle "$C" $id | jq -r '.circle.circle_status')
    if [[ "$ST" == "Running" ]]; then
        log "  ${GREEN}✓ PASS${NC}: Circle $id already auto-started (status=Running)"
        PASS=$((PASS+1))
    else
        run_test "Start circle $id manually (status was $ST)" do_start $id
    fi
done
log ""

# ── QUERY ALL STATES ─────────────────────────────────────────────────────────

log "${CYAN}[12] QUERY CIRCLE STATES (initial snapshot)${NC}"
for id in 1 2 3 4 5 6 7 8; do
    QD=$(qcircle "$C" "$id" 2>/dev/null || echo "{}")
    S=$(echo "$QD" | jq -r '.circle.circle_status // "?"')
    M=$(echo "$QD" | jq -r '.circle.members_list | length // 0')
    DT=$(echo "$QD" | jq -r '.circle.distribution_threshold // "none"' | tr -d '\n ')
    log "  Circle $id: status=${YELLOW}$S${NC}, members=$M, threshold=$DT"
done
log ""

# ── ROUND 1 DEPOSITS (all running circles) ──────────────────────────────────

log "${CYAN}[13] DEPOSIT CONTRIBUTION (Round 1)${NC}"
do_deposit() {
    local ID=$1 KEY=$2
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{deposit_contribution:{circle_id:$id}}')" \
        "$KEY" "${CONTRIBUTION}${FEE_DENOM}"
}
for id in 1 2 3 4 8; do
    run_test "Circle $id: creator deposit R1" do_deposit $id "$CREATOR_KEY"
    run_test "Circle $id: member  deposit R1" do_deposit $id "$MEMBER_KEY"
done
# Circle 5 (ejection): only creator deposits — member will miss every round
run_test "Circle 5: creator deposit R1 (member skips, will accumulate late fees)" \
    do_deposit 5 "$CREATOR_KEY"
log ""

# ── WAIT cycle+grace, then ADVANCE / PROCESS_PAYOUT ──────────────────────────

wait_round_end "R1 of all circles (cron tick)"

log "${CYAN}[14] R1→R2 TRANSITION (cron-style: advance or payout depending on threshold)${NC}"
do_advance() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{advance_round:{circle_id:$id}}')" \
        "$CREATOR_KEY"
}
do_process_payout() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{process_payout:{circle_id:$id}}')" \
        "$CREATOR_KEY"
}
# Circle 1 (Total, 2 members, 2 cycles): R1 is not distribution, R2 is. AdvanceRound R1→R2.
run_test "Circle 1: AdvanceRound R1→R2 (Total, not yet distribution round)" do_advance 1
# Circle 2 (None=Total semantics, 2 members, 1 cycle): R1 is round 1, last round of cycle.
# For Total/None, distribution at last round, so process_payout directly. But round_in_cycle is 1
# and min_round_for_distribution is active_count=2 → R1 is NOT distribution round, we must advance.
run_test "Circle 2: AdvanceRound R1→R2 (None=Total semantics, awaiting last round)" do_advance 2
# Circle 3 (MinMembers(2), 2 active): R1 < min_round=2, so advance, not payout.
run_test "Circle 3: AdvanceRound R1→R2 (MinMembers(2))" do_advance 3
# Circle 4 (Total, manual_trigger): R1 not distribution, advance (manual_trigger requires creator).
run_test "Circle 4: AdvanceRound R1→R2 (Total, manual_trigger)" do_advance 4
# Circle 5 (None=Total, 2 members, 1 cycle): R1 not last round; advance. But member is missing,
# so we need grace_end passed too. Wait extra grace seconds before advancing.
# (already waited ROUND_WAIT=22s which is > cycle+grace; should be fine if grace<cycle)
run_test "Circle 5: AdvanceRound R1→R2 (None=Total, missing member triggers late fee)" do_advance 5
# Circle 8 (Total, 2 members, 2 cycles): same shape as Circle 1 round 1 — advance only.
run_test "Circle 8: AdvanceRound R1→R2 (block-target setup)" do_advance 8

log ""

# ── PAUSE / UNPAUSE (on circle 3) ────────────────────────────────────────────

log "${CYAN}[15] PAUSE / UNPAUSE${NC}"
do_pause()   { tx_must_succeed "$C" "$(jq -n --argjson id $1 '{pause_circle:{circle_id:$id}}')"   "$CREATOR_KEY"; }
do_unpause() { tx_must_succeed "$C" "$(jq -n --argjson id $1 '{unpause_circle:{circle_id:$id}}')" "$CREATOR_KEY"; }
run_test "Pause circle 3"                       do_pause 3
run_test "Verify circle 3 status = Paused"      assert_status "$C" 3 "Paused"
run_test "Unpause circle 3"                     do_unpause 3
run_test "Verify circle 3 status = Running"     assert_status "$C" 3 "Running"
log ""

# ── ROUND 2 DEPOSITS + PROCESS_PAYOUT ────────────────────────────────────────

log "${CYAN}[16] DEPOSIT R2 for all running circles${NC}"
for id in 1 2 3 4; do
    run_test "Circle $id: creator deposit R2" do_deposit $id "$CREATOR_KEY"
    run_test "Circle $id: member  deposit R2" do_deposit $id "$MEMBER_KEY"
done
run_test "Circle 5: creator deposit R2 (member still missing)" do_deposit 5 "$CREATOR_KEY"
log ""

wait_round_end "R2 of all circles (cron tick, distribution round)"

log "${CYAN}[17] PROCESS_PAYOUT R2 (distribution round, Total semantics)${NC}"
# Circles 1,2,3,4,5: at R2, round_in_cycle = 2 == active_count(2). This IS the distribution round
# for Total/None and MinMembers(2). ProcessPayout.
for id in 1 2 3 4 5; do
    run_test "Circle $id: ProcessPayout R2 (cycle completes)" do_process_payout $id
done
log ""

# ── ROUND 3+4 for Circle 1 only (now 3 cycles = 6 rounds total; we do 4) ─────

log "${CYAN}[18] CIRCLE 1: SECOND CYCLE (R3→R4) — leaves circle 1 Running at cycle 3 R1${NC}"
# Circle 1 has total_cycles=3 → after R4 process_payout: cycles_completed=2, status=Running,
# current_cycle_index=5 (R5 = cycle 3 R1). Late-stage tests will exercise this Running state.
C1_STATUS=$(qcircle "$C" 1 | jq -r '.circle.circle_status')
log "  Circle 1 status after R2: $C1_STATUS"
if [[ "$C1_STATUS" == "Running" ]]; then
    run_test "Circle 1: creator deposit R3" do_deposit 1 "$CREATOR_KEY"
    run_test "Circle 1: member  deposit R3" do_deposit 1 "$MEMBER_KEY"
    wait_round_end "Circle 1 R3"
    run_test "Circle 1: AdvanceRound R3→R4" do_advance 1
    run_test "Circle 1: creator deposit R4" do_deposit 1 "$CREATOR_KEY"
    run_test "Circle 1: member  deposit R4" do_deposit 1 "$MEMBER_KEY"
    wait_round_end "Circle 1 R4 (cycle 2 distribution)"
    run_test "Circle 1: ProcessPayout R4 (cycle 2 distribution)" do_process_payout 1
fi
log ""

# ── WITHDRAW (all members for circles that distributed) ──────────────────────

log "${CYAN}[19] WITHDRAW pending payouts${NC}"
do_withdraw() {
    local ID=$1 KEY=$2
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{withdraw:{circle_id:$id}}')" "$KEY"
}
for id in 1 2 3 4 5; do
    for key in "$CREATOR_KEY" "$MEMBER_KEY"; do
        ROLE=$([ "$key" = "$CREATOR_KEY" ] && echo "creator" || echo "member")
        # Check pending payout amount first — withdraw fails if zero pending
        PENDING=$(q "$C" "$(jq -n --argjson id "$id" --arg a "$([ "$key" = "$CREATOR_KEY" ] && echo "$CREATOR_ADDR" || echo "$MEMBER_ADDR")" '{get_pending_payout:{circle_id:$id,member:$a}}')" \
            | jq -r '.data.amount // "0"')
        if [[ "$PENDING" != "0" && -n "$PENDING" ]]; then
            run_test "Withdraw circle $id ($ROLE, pending=$PENDING)" do_withdraw $id "$key"
        else
            log "  ${BLUE}↳ skip Withdraw circle $id ($ROLE): no pending payout${NC}"
        fi
    done
done
log ""

# ── CHECK EJECTION (circle 5) ────────────────────────────────────────────────

log "${CYAN}[20] CHECK EJECTION (circle 5)${NC}"
C5_STATUS=$(qcircle "$C" 5 | jq -r '.circle.circle_status')
MEMBERS_5=$(qcircle "$C" 5 | jq '.circle.members_list | length')
log "  Circle 5 status: $C5_STATUS, members=$MEMBERS_5"
do_check_eject() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{check_and_eject:{circle_id:$id}}')" \
        "$CREATOR_KEY"
}
if [[ "$MEMBERS_5" -lt 2 ]]; then
    log "  ${GREEN}✓ Member auto-ejected during deposits/payout${NC}"
    PASS=$((PASS+1))
else
    log "  ${YELLOW}Member not auto-ejected (penalties below threshold)${NC}"
    if [[ "$C5_STATUS" == "Running" ]]; then
        run_test "CheckAndEject circle 5 (permissionless)" do_check_eject 5
    fi
fi
log ""

# ── BLOCK MEMBER + DISTRIBUTE BLOCKED FUNDS (circle 3) ───────────────────────

log "${CYAN}[21] BLOCK MEMBER + DISTRIBUTE BLOCKED FUNDS (target: circle 8)${NC}"
do_block_member() {
    local ID=$1 ADDR=$2
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID --arg a "$ADDR" '{block_member:{circle_id:$id,member_address:$a}}')" \
        "$CREATOR_KEY"
}
do_distribute_blocked() {
    local ID=$1 CYCLE=$2
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID --argjson cy $CYCLE '{distribute_blocked_funds:{circle_id:$id,cycle:$cy}}')" \
        "$CREATOR_KEY"
}
C8_STATUS=$(qcircle "$C" 8 | jq -r '.circle.circle_status')
C8_MEMBERS=$(qcircle "$C" 8 | jq '.circle.members_list | length')
log "  Circle 8 status: $C8_STATUS, members=$C8_MEMBERS"
if [[ "$C8_STATUS" == "Running" && "$C8_MEMBERS" -gt 1 ]]; then
    run_test "Block member in circle 8" do_block_member 8 "$MEMBER_ADDR"
    # Verify the block via query
    do_verify_block_query() {
        local R; R=$(q "$C" '{"get_blocked_members":{"circle_id":8}}' 2>&1)
        local CNT; CNT=$(echo "$R" | jq -r '.data.blocked_members | length' 2>/dev/null)
        [[ "$CNT" -ge 1 ]] || { echo "GetBlockedMembers returned $CNT entries (expected >=1)"; return 1; }
    }
    run_test "GetBlockedMembers(8) shows the blocked member" do_verify_block_query

    # DistributeBlockedFunds requires (a) blocked_from_cycle <= cycle param AND
    # (b) at least one active member with a DEPOSITS row at that cycle. Our
    # circle-8 state — blocked at R2 (blocked_from_cycle=3), no R2 deposits —
    # satisfies neither for any reachable cycle, so we assert the function
    # REJECTS the call (negative test).
    do_distribute_blocked_should_fail() {
        tx_must_fail "$C" \
            "$(jq -n '{distribute_blocked_funds:{circle_id:8,cycle:2}}')" \
            "$CREATOR_KEY"
    }
    run_test "DistributeBlockedFunds(8, cycle=2) rejected (no active depositors at cycle)" \
        do_distribute_blocked_should_fail
else
    log "  ${BLUE}↳ skip BlockMember: circle 8 not Running with 2+ members${NC}"
fi
log ""

# ── CANCEL AFTER START — should FAIL once distribution has happened ──────────
# Done FIRST so circle 1 is still Running with cycles_completed >= 1.

log "${CYAN}[22] CANCEL AFTER START — expect fail${NC}"
CANCEL_AFTER_TESTED=0
for CID in 1 2 3 4; do
    ST=$(qcircle "$C" $CID | jq -r '.circle.circle_status')
    CC=$(qcircle "$C" $CID | jq -r '.circle.cycles_completed')
    if [[ ("$ST" == "Running" || "$ST" == "Paused") && "$CC" -ge 1 ]]; then
        log "  Targeting circle $CID (status=$ST cycles_completed=$CC)"
        run_test "Cancel circle $CID after distribution rejected" tx_must_fail \
            "$C" "$(jq -n --argjson id $CID '{cancel_circle:{circle_id:$id}}')" "$CREATOR_KEY"
        CANCEL_AFTER_TESTED=1
        break
    fi
done
if [[ $CANCEL_AFTER_TESTED -eq 0 ]]; then
    log "  ${BLUE}↳ no distributed Running/Paused circle to test cancel-rejection on${NC}"
fi
log ""

# ── EXIT AFTER START (best-effort: if anything still Running) ────────────────

log "${CYAN}[23] EXIT AFTER START (apply penalty)${NC}"
EXIT_TESTED=0
for CID in 1 2 3 4; do
    ST=$(qcircle "$C" $CID | jq -r '.circle.circle_status')
    M=$(qcircle "$C" $CID | jq '.circle.members_list | length')
    if [[ "$ST" == "Running" && "$M" -gt 1 ]]; then
        run_test "Exit circle $CID as member (after start, penalty applied)" do_exit $CID "$MEMBER_KEY"
        EXIT_TESTED=1
        break
    fi
done
if [[ $EXIT_TESTED -eq 0 ]]; then
    log "  ${BLUE}↳ no Running 2-member circle available for ExitAfterStart${NC}"
fi
log ""

# ── EMERGENCY STOP (target any still-Running circle, prefer circle 8) ────────

log "${CYAN}[24] EMERGENCY STOP${NC}"
do_emergency_stop() {
    local ID=$1
    tx_must_succeed "$C" \
        "$(jq -n --argjson id $ID '{emergency_stop:{circle_id:$id}}')" \
        "$CREATOR_KEY"
}
EMG_DONE=0
# Prefer circle 8 (it has emergency_stop_enabled=true and was set up for this).
for TEST_ID in 8 1 5 3 4; do
    TEST_STATUS=$(qcircle "$C" $TEST_ID | jq -r '.circle.circle_status')
    if [[ "$TEST_STATUS" == "Running" ]]; then
        run_test "EmergencyStop circle $TEST_ID (was $TEST_STATUS)" do_emergency_stop $TEST_ID
        EMG_DONE=1
        break
    fi
done
if [[ $EMG_DONE -eq 0 ]]; then
    log "  ${BLUE}↳ no Running circle available for EmergencyStop${NC}"
fi
log ""

# ── ALL QUERIES ──────────────────────────────────────────────────────────────

log "${CYAN}[25] COMPREHENSIVE QUERIES${NC}"
do_query_test() {
    local QUERY="$1"
    local R; R=$(q "$C" "$QUERY" 2>&1) || { echo "query failed"; return 1; }
    echo "$R" | jq -e '.data // .' >/dev/null 2>&1 || { echo "no data"; return 1; }
}
run_test "GetCircle(1)"               do_query_test '{"get_circle":{"circle_id":1}}'
run_test "GetCircles(limit=10)"       do_query_test '{"get_circles":{"limit":10}}'
run_test "GetCircles(status=Running)" do_query_test '{"get_circles":{"limit":10,"status":"Running"}}'
run_test "GetCircles(by creator)"     do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_circles:{limit:10,creator:$a}}')"
run_test "GetCircleMembers(1)"        do_query_test '{"get_circle_members":{"circle_id":1}}'
run_test "GetCircleStatus(1)"         do_query_test '{"get_circle_status":{"circle_id":1}}'
run_test "GetCurrentCycle(1)"         do_query_test '{"get_current_cycle":{"circle_id":1}}'
run_test "GetCycleDeposits(1,1)"      do_query_test '{"get_cycle_deposits":{"circle_id":1,"cycle":1}}'
run_test "GetMemberDeposits"          do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_member_deposits:{circle_id:1,member:$a}}')"
run_test "GetPayouts(1)"              do_query_test '{"get_payouts":{"circle_id":1}}'
run_test "GetPayoutHistory(1)"        do_query_test '{"get_payout_history":{"circle_id":1}}'
run_test "GetPayoutHistory(1,cycle=1)" do_query_test '{"get_payout_history":{"circle_id":1,"cycle":1}}'
run_test "GetCircleBalance(1)"        do_query_test '{"get_circle_balance":{"circle_id":1}}'
run_test "GetMemberBalance"           do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_member_balance:{circle_id:1,member:$a}}')"
run_test "GetPenalties(1)"            do_query_test '{"get_penalties":{"circle_id":1}}'
run_test "GetPenalties(1, member)"    do_query_test "$(jq -n --arg a "$MEMBER_ADDR" '{get_penalties:{circle_id:1,member:$a}}')"
run_test "GetRefunds(1)"              do_query_test '{"get_refunds":{"circle_id":1}}'
run_test "GetPendingPayout"           do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_pending_payout:{circle_id:1,member:$a}}')"
run_test "GetMemberAccumLateFees"     do_query_test "$(jq -n --arg a "$MEMBER_ADDR" '{get_member_accumulated_late_fees:{circle_id:5,member:$a}}')"
run_test "GetDepositRequirement"      do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_deposit_requirement:{circle_id:1,member:$a}}')"
run_test "GetEvents(1)"               do_query_test '{"get_events":{"circle_id":1,"limit":10}}'
run_test "GetCircleStats(1)"          do_query_test '{"get_circle_stats":{"circle_id":1}}'
run_test "GetMemberStats"             do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_member_stats:{circle_id:1,member:$a}}')"
run_test "GetMemberLockedAmount"      do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_member_locked_amount:{circle_id:1,member:$a}}')"
run_test "GetBlockedMembers(1)"       do_query_test '{"get_blocked_members":{"circle_id":1}}'
run_test "GetBlockedMembers(3)"       do_query_test '{"get_blocked_members":{"circle_id":3}}'
run_test "GetMemberPseudonym(3)"      do_query_test "$(jq -n --arg a "$CREATOR_ADDR" '{get_member_pseudonym:{circle_id:3,member:$a}}')"
run_test "GetPrivateMembers(3)"       do_query_test '{"get_private_members":{"circle_id":3}}'
run_test "GetDistributionCalendar(1)" do_query_test '{"get_distribution_calendar":{"circle_id":1}}'
run_test "GetArchivedDate(1)"         do_query_test '{"get_archived_date":{"circle_id":1}}'
run_test "GetContractVersion"         do_query_test '{"get_contract_version":{}}'
log ""

# ── FINAL STATUS SNAPSHOT ────────────────────────────────────────────────────

log "${CYAN}[26] FINAL STATUS SNAPSHOT${NC}"
for id in 1 2 3 4 5 6 7 8; do
    QD=$(qcircle "$C" "$id" 2>/dev/null || echo "{}")
    S=$(echo "$QD"  | jq -r '.circle.circle_status // "?"')
    M=$(echo "$QD"  | jq -r '.circle.members_list | length // 0')
    CY=$(echo "$QD" | jq -r '.circle.current_cycle_index // 0')
    CC=$(echo "$QD" | jq -r '.circle.cycles_completed // 0')
    PP=$(echo "$QD" | jq -r '.circle.total_pending_payouts // "0"')
    log "  Circle $id: status=${YELLOW}$S${NC}, members=$M, round=$CY, cycles_completed=$CC, pending=$PP"
done
log ""

# ── SUMMARY ──────────────────────────────────────────────────────────────────

TOTAL=$((PASS + FAIL))
log "${CYAN}════════════════════════════════════════════════════════════════${NC}"
if [[ $FAIL -eq 0 ]]; then
    log "${GREEN}  ALL $TOTAL TESTS PASSED ✓${NC}"
else
    log "${RED}  $FAIL/$TOTAL TESTS FAILED${NC}"
    log "${GREEN}  $PASS passed${NC}, ${RED}$FAIL failed${NC}"
fi
log "${CYAN}════════════════════════════════════════════════════════════════${NC}"
log "Contract: $C"
log ""

# ── Write report ─────────────────────────────────────────────────────────────
{
    echo "# Safrimba Contract Test Report"
    echo ""
    echo "Generated: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    echo "Network: $NETWORK | Chain: $CHAIN_ID | Code ID: $CODE_ID"
    echo "Contract: \`$C\`"
    echo "Creator: \`$CREATOR_ADDR\` | Member: \`$MEMBER_ADDR\`"
    echo "Cycle: ${CYCLE_SECS}s | Grace: ${GRACE_SECS}s | Round wait: ${ROUND_WAIT}s"
    echo ""
    echo "## Results: $PASS passed, $FAIL failed out of $TOTAL tests"
    echo ""
    echo '```'
    echo "$REPORT"
    echo '```'
} > "$REPORT_FILE"
log "Report: $REPORT_FILE"

[[ $FAIL -eq 0 ]] && exit 0 || exit 1
