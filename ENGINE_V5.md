# Safrimba engine v5 — "clockwork"

`safrimba-contract` **0.5.1** · `GetContractVersion → { api_version: 5 }` ·
optimized wasm sha256 `4d836fd64a61d3998be7798472bad4d9e9da4f815d4bf73f43a4acb8cdaeef7d`
(620 KiB, `make optimize-local`, no bulk-memory, no floats, exports `reply`).

v5 replaces the transaction-driven round machine of v4 (stored round counter that
only moved when someone sent `ProcessPayout` / `AdvanceRound`) with a
**time-driven, lazily settled, never-blocking** engine. 0.5.1 adds the rules of
[PLAN_V5.md](../PLAN_V5.md) P0: no admin power over other members' money,
bounded pauses, fair departures, robust payouts.

## Principles

1. **Time is the source of truth.** Every deadline is a pure function of
   `(anchor, round duration, grace, slots, paused seconds)` — see
   `src/engine/clock.rs`. Nothing needs to "advance" a round.
2. **Lazy settlement.** Every execute first applies all transitions the clock made
   due (`Model::sync`): automatic end of a pause, date-scheduled start, round
   settlements, final settlement. Queries run the same code in memory, so
   `GetCircleView` / `GetCircle` are correct at the block time of the query even if
   nobody sent a transaction.
3. **Permissionless `Tick`.** Anyone (the Pulse keeper, a member, a bot) can
   materialize due transitions. `ProcessPayout`, `AdvanceRound` and
   `CheckAndEject` are aliases of `Tick`. A `Tick` with nothing due fails with
   `NothingToDo { next_action_at }`, so keepers never waste gas.
4. **Settlement never fails for expected situations.** Missing contribution →
   lock coverage → arrears for the uncovered part. Absent admin, empty lock,
   member in default, `AdminOnly` policy: the schedule always moves.
5. **Push, not pull — and a refused push never blocks.** Payouts, refunds, lock
   returns and platform fees are sent inside the settling transaction, each as a
   sub-message with `reply_on_error`. If the chain refuses one (blocked module
   account…), `reply` credits it to the recipient's pending ledger
   (`safrimba_payout_deferred`) and everything else goes through; `Withdraw`
   releases it. A completed circle that still owes a withdrawal is `Finalizing`.
6. **Nobody gains from someone else's departure**, and **admins organise, they
   never touch other members' money** (see below).
7. **One circle per instance** (unchanged): the instance's bank balance is the
   circle's escrow.

## Schedule

`slots` = members when the circle starts (fixed). `R = slots × total_cycles`.

| Round `k` | Effective time |
|---|---|
| opens | `start + (k-1)·D` |
| due | `start + k·D` |
| settles | `due + G` (grace, `G < D`) |

A deposit pays the member's **oldest open unpaid round**. During `(due, settles]`
the next round is already open; a late payer still pays round `k` first and the
deposit is counted late (`late_count`, reputation only).

**Bounded pause.** A pause freezes effective time and shifts every later
deadline. One pause lasts at most `max_pause = min(D, 7 days)`; all pauses
together at most `2 × max_pause`. At `paused_at + pause_limit` the circle
resumes **by itself** (`safrimba_status { reason: "auto_resume" }`) — no admin
action needed. A reason (≤ 140 characters) is recorded and shown
(`pause_reason`, `paused_until`). Deposits and exits stay possible while paused.

## Distribution (unified rule)

`t` = threshold start (`Total` → `t = slots`, `MinMembers{count}` → `count`).
Within each cycle:

* rounds `1 … t-1` **accumulate** into the cycle pot (`carry_pot`);
* rounds `t … slots-1` pay **one member each** (payout order index `r - t`);
* the **last round** pays each of the `t` group slots an equal share of its pot
  plus everything pooled during the cycle.

So `MinMembers{1}` is the classic rotating tontine, `Total` is "everyone shares
at the end of the cycle", and `MinMembers{k}` is the hybrid — and in every mode
each active member receives exactly once per cycle.

Platform fee: `platform_fee_percent` of every payout (turn, group share,
compensation), sent immediately. Never on refunds, locks or claims.

## Missed contributions, debts, ejection

At settlement of round `k`, for an active member who did not pay:

1. `missed_total += 1`, `strikes += 1`;
2. the lock covers up to one contribution (synthetic deposit record);
3. the uncovered part becomes **arrears**;
4. a **late fee** (`late_fee_percent` of the contribution) is taken from what is
   left of the lock into the **common pool** (PLAN_V5 3.11 will turn it into a debt);
5. **auto-ejection** (policies `auto_*`; the creator and the co-admin are members
   like the others) when the lock could not cover the round, or
   `strikes ≥ max_missed_payments_allowed`.

Repaying: a deposit may attach between `contribution` and
`contribution + arrears + lock refill`; the extra clears arrears first (→ common
pool), then refills the lock. `RepayArrears` does the same at any time — after
the last round, or after leaving. Strikes reset when arrears are zero and the
lock is whole again. **Netting**: a payout first clears the recipient's arrears,
then refills their lock (active members); the rest is pushed.

## Leaving (exit, admin ejection, engine ejection)

The exit penalty (`exit_penalty_percent` of the original lock) always goes to
the common pool. Then it depends on whether the member was already served in
the current cycle:

* **(a) Not served yet.** No liability left. Paid future rounds are refunded,
  the lock (minus penalty and any arrears still owed) comes back immediately,
  and the member acquires a **claim** = contributions to the current cycle
  (`C × rounds collected in it`) minus arrears (set off). The claim is paid at
  once from the pooled carry when there is one (Total / hybrid), otherwise from
  what would have been their turn (or their group share), and finally from the
  end-of-cycle regularisation; what is still unpaid when the cycle ends is
  extinguished (`safrimba_claim_forfeited`).
* **(b) Already served.** A **voluntary exit settles the rest of the cycle**:
  `owed = C × (cycle end − paid_through) + arrears`, the lock pays first, the
  member attaches exactly the rest (`exit_quote.to_attach`); the remaining lock
  comes back. An **ejected** member stays liable to the end of the cycle through
  the lock; what it cannot cover is recorded as arrears (a debt, repayable).
* `strict_mode`: voluntary exits only right after a cycle ends.
* Admins eject **for cause only** (a strike or arrears); nobody ejects themselves.

**Orphaned turns and end-of-cycle regularisation.** The share of a slot whose
holder left first pays that member's claim; the rest goes to the cycle's
compensation reserve (`comp_pool`). At the last round of the cycle the reserve
fills, for every participant of the cycle, the gap between what they put into
it (`C × slots − uncovered`) and what they received from it, and pays what is
still owed on claims — pro-rata if the reserve is short, surplus to the common
pool. With any number of departures and in every mode, each participant ends the
cycle with what they contributed (the randomized test asserts that no member
ever receives more than `slots × C` gross in a cycle).

`GetCircleView.me.exit_quote` is computed by the same function as the
execution (`Model::departure_plan`): what the member is shown is exactly what
happens.

## Admin powers

Admins are resolved dynamically: the creator and the co-admin while they are
active members; if neither is, the active member with the lowest slot
(`GetCircleView.admins`). Nothing in the schedule depends on them.

| Power | Rule |
|---|---|
| Invite, start, update, pseudonyms, co-admin | admins |
| `EjectMember` | for cause (strike or arrears); not oneself |
| `PauseCircle` / `EmergencyStop` | bounded, ends by itself, reason recorded |
| `EndCycle` | before the first distribution of the current cycle: end now (contributions of the cycle back); otherwise toggles "stop after the current cycle" (`ending_after_cycle`) — the cycle completes, everyone gets their turn, `Completed` with reason `admin_end` |
| `CancelCircle` | **creator only**; after start only before the first distribution: contributions back, the creator pays the standard exit penalty (shared by the others) |

Roster and schedule changes (`ExitCircle`, `EjectMember`, `EndCycle`,
`CancelCircle`, `PauseCircle`, `UnpauseCircle`, `RepayArrears`, deposits) are
refused with `CatchingUp` while the stored state is more than
`MAX_ROUNDS_PER_TX` (24) rounds behind the clock: `Tick` first.

## Ending

* **Completed** (all rounds settled, or `ending_after_cycle` reached) / **Ended**
  (`EndCycle` before any distribution of the cycle, or fewer than two active
  members): paid-but-unsettled rounds are refunded; an unfinished cycle is
  regularised (members not served get back what they put in, served
  participants get their gap filled) from the carry, the compensation reserve
  and the common pool, pro-rata if short; what is left is split equally among
  active members with arrears set off (water-filling: recovered arrears belong
  to everyone); every lock is returned; unfunded claims are extinguished.
* **Cancelled** before start: every lock returned. After start (creator, before
  the first distribution): as above, the creator pays the exit penalty and is
  left out of the final split.
* `by_date` circles start lazily **anchored at `auto_start_date`**; if the
  minimum is not met and `auto_refund_if_min_not_met`, they cancel and refund.

## Money invariant (asserted after every test step)

```
bank(contract) = Σ member.lock + carry_pot + common_pool + comp_pool
               + contribution × Σ (member.paid_through − rounds_settled)⁺
               + total_pending_payouts                  (refused pushes)
bank(contract) = total_pending_payouts once Completed / Finalizing / Cancelled / Ended
```

Claims are promises funded from future pots, not money held.

`tests/engine_v5.rs` — 42 scenarios: every distribution mode; EndCycle after a
payout never confiscates; bounded pause and budget; eject for cause; co-admin
cannot cancel; admin role moves; departures before / after being served in
rotating, Total and hybrid modes (exactly fair); settle-up exit; recorded and
repayable debts; early close pays unserved members first; catch-up guard with
30 rounds behind; refused pushes (member and platform) deferred and
withdrawable; contracts refused as private members; and a randomized
adversarial simulation of 150 circles (up to 30 members and 6 cycles, misses,
partial repayments, quoted exits, an adversarial admin ejecting, pausing,
ending and cancelling, keeper outages of more than 24 rounds): money is
conserved, every circle finishes, the contract ends empty, quotes are exact,
nobody is served twice in a cycle or receives more than `slots × C` in one,
no claim stays open. `SEED=<n> cargo test -- --ignored replay_seed --nocapture`
replays one circle with its full event log (`SEED=scan` scans 400).

## Messages

Unchanged wire format for: `CreateCircle`, `JoinCircle`, `AcceptInvite`,
`InviteMember`, `StartCircle`, `DepositContribution`, `EjectMember`,
`SetCoAdmin`, `ClearCoAdmin`, `EndCycle`, `RateMember`, `UnpauseCircle`,
`CancelCircle`, `UpdateCircle`, `AddPrivateMember` (now refuses contract
addresses), `UpdateMemberPseudonym`, `DepositCreatorReward`, `SweepDust`,
`Withdraw`, `Tick` and its aliases.

0.5.1 (additive, old clients keep working):
* `PauseCircle { circle_id, reason? }`, `EmergencyStop { circle_id, reason? }`;
* `ExitCircle { circle_id }` becomes payable when the member was served in the
  current cycle (attach `exit_quote.to_attach`);
* `RepayArrears { circle_id }` (new, payable).

Removed (explicit error): `ReanchorSchedule`, `BlockMember`,
`DistributeBlockedFunds`, `WithdrawPlatformFees`.

## Queries

`GetCircleView { circle_id, member? }` — status, focus round (phase, opens /
due / settles, kind, recipients, who paid, collected, expected pot), next round,
`next_action_at` (also while paused: it accounts for the automatic resume),
`tick_due`, `rounds_behind` (stored state vs clock), pools, every member ledger,
and for `member` what they owe right now (`my_due`). 0.5.1 adds:
`current_cycle`, `paused_until`, `pause_reason`, `max_pause_secs`,
`pause_secs_left`, `ending_after_cycle`, `end_now_allowed`, `admins`, `creator`,
`co_admin`, `strict_mode`, `exit_penalty_bps`, `late_fee_bps`,
`platform_fee_bps`, `pending_payouts_total`, `comp_pool`,
`emergency_stop_enabled`; per member `claim`, `late_count`,
`served_this_cycle`, `pending_payout`, `can_be_ejected`, and on `me`
`exit_quote { allowed, blocked_reason, served_this_cycle, penalty, owed,
from_lock, to_attach, lock_refund, prepaid_refund, claim, claim_paid_now }`.
All v4 queries are still served (computed from the v5 ledger after virtual sync).

## Events (for indexers)

`wasm-safrimba_started`, `_round` (+ `compensated`), `_payout` (+ `kind`:
`turn` | `group` | `compensation` | `close_share`), `_deposit`,
`_member_joined`, `_member_left` (+ `by`, `served`, `claim`, `lock_refund`,
`prepaid_refund`, `settled`), `_invited`, `_refund`, `_status` (+ `reason`:
pause reason, `auto_resume`, `admin_resume`, closing reason), `_closed` (also on
cancellation), `_end_requested { after_cycle, by }`,
`_end_cancelled { after_cycle, by }`, `_claim_paid { member, amount, round,
source: carry | turn | cycle_end | close }`, `_claim_forfeited`,
`_arrears_repaid { member, amount, lock_refill }`,
`_payout_deferred { to, amount, denom, error }`. Every response also carries
`circle_status`, `rounds_settled`, `rounds_behind`, `next_action_at`.

## Build & deploy

```bash
cargo test                                   # 47 tests (4 unit, 42 scenarios, 1 custody)
cargo run --example schema                   # schema/
RUSTFLAGS='-C link-arg=-s' cargo build --release --lib --target wasm32-unknown-unknown
make optimize-local                          # lowers bulk-memory (Safrochain wasmvm)
shasum -a 256 artifacts/safrimba_contract.wasm
```

Store the wasm as a **new code id**; then, in `safrimba-frontend/chain/<network>/safrochain.json`,
set `codeId` to it and `"codeApiVersion": 5` (the create form instantiates v5 and
the UI detects it), and add it to `VITE_V5_CODE_IDS` and `SAFRIMBA_CODE_IDS`.
`migrate` only accepts ≥ 0.5 state: **v4 instances stay on their v4 code id**
(the Pulse keeper drives both engines). Do not migrate live v4 circles in place.
