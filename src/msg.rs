use cosmwasm_schema::QueryResponses;
use cosmwasm_std::{Addr, Timestamp, Uint128};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::{
    CircleStatus, DistributionThreshold, EjectionPolicy, MemberStatus, PayoutOrderType, Visibility,
};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg {
    pub platform_fee_percent: u64, // Basis points
    pub platform_address: Addr,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MigrateMsg {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    CreateCircle {
        circle_name: String,
        circle_description: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        circle_image: Option<String>,
        max_members: u32,
        min_members_required: u32,
        invite_only: bool,
        contribution_amount: Uint128,
        /// `"usaf"` (default) or the allow-listed Noble USDC IBC denom of the network.
        #[serde(skip_serializing_if = "Option::is_none")]
        denomination: Option<String>,
        /// Basis points of the original lock forfeited on ejection / exit.
        exit_penalty_percent: u64,
        /// Basis points of contribution charged (from the lock) per missed round.
        late_fee_percent: u64,
        total_cycles: u32,
        cycle_duration_days: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        cycle_duration_seconds: Option<u64>,
        /// Informative only; the schedule is anchored when the circle starts.
        #[serde(skip_serializing_if = "Option::is_none")]
        start_date: Option<Timestamp>,
        grace_period_hours: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        grace_period_seconds: Option<u64>,
        auto_start_when_full: bool,
        /// `"by_members"` (start when full) or `"by_date"` (start at `auto_start_date`).
        #[serde(skip_serializing_if = "Option::is_none")]
        auto_start_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        auto_start_date: Option<Timestamp>,
        payout_order_type: PayoutOrderType,
        #[serde(skip_serializing_if = "Option::is_none")]
        payout_order_list: Option<Vec<Addr>>,
        /// Ignored on v5 (always automatic).
        auto_payout_enabled: bool,
        /// Ignored on v5 (settlement is permissionless and automatic).
        manual_trigger_enabled: bool,
        emergency_stop_enabled: bool,
        auto_refund_if_min_not_met: bool,
        strict_mode: bool,
        visibility: Visibility,
        show_member_identities: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        distribution_threshold: Option<DistributionThreshold>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ejection_policy: Option<EjectionPolicy>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        co_admin_address: Option<Addr>,
    },
    /// Attach exactly `contribution_amount` (security lock).
    JoinCircle { circle_id: u64 },
    /// Same as JoinCircle for invited addresses.
    AcceptInvite { circle_id: u64 },
    InviteMember { circle_id: u64, member_address: Addr },
    /// Before start: full refund of the lock (not for the creator: cancel instead).
    /// After start (`GetCircleView.me.exit_quote` gives the exact amounts):
    ///   * not served yet in the current cycle → lock back minus the exit penalty,
    ///     prepaid rounds refunded, and a claim on what you put into the cycle
    ///     (paid now from the pooled carry, the rest at what would have been your turn);
    ///   * already served in the current cycle → you must settle the rest of the
    ///     cycle: attach exactly `exit_quote.to_attach` (the lock pays first).
    ///
    /// `strict_mode`: voluntary exits only right after a cycle ends.
    ExitCircle { circle_id: u64 },
    StartCircle { circle_id: u64 },
    /// Pay the oldest open round you have not paid. Attach between
    /// `contribution_amount` and `contribution_amount + arrears + lock refill`;
    /// the extra clears arrears first, then refills the lock.
    DepositContribution { circle_id: u64 },
    /// Permissionless. Settles every round whose settlement time has passed,
    /// starts date-scheduled circles and finalizes finished ones. Funds are pushed.
    Tick { circle_id: u64 },
    /// v4 alias of `Tick`.
    ProcessPayout { circle_id: u64 },
    /// v4 alias of `Tick`.
    AdvanceRound { circle_id: u64 },
    /// v4 alias of `Tick` (ejection rules run during settlement).
    CheckAndEject { circle_id: u64 },
    /// Pull a payout whose push failed (credited to the pending ledger by `reply`).
    Withdraw { circle_id: u64 },
    /// Admin, for cause only: the member has a strike or arrears. Creator and
    /// co-admin are members like the others; nobody can eject themselves.
    EjectMember { circle_id: u64, member_address: Addr },
    SetCoAdmin { circle_id: u64, co_admin_address: Addr },
    ClearCoAdmin { circle_id: u64 },
    /// Removed in v5 (deadlines are derived from the clock).
    ReanchorSchedule { circle_id: u64 },
    /// Admin. Before the first distribution of the current cycle (nobody served
    /// yet, see `GetCircleView.end_now_allowed`): end now — contributions of the
    /// cycle come back, locks returned. Otherwise: toggle "stop after the current
    /// cycle" (`ending_after_cycle`); the cycle completes, everyone gets a turn.
    EndCycle { circle_id: u64 },
    RateMember {
        circle_id: u64,
        member_address: Addr,
        score: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Admin. Bounded pause: it ends by itself after `min(round, 7 days)`, and all
    /// pauses together last at most twice that. Deposits stay possible.
    PauseCircle {
        circle_id: u64,
        /// Shown to members (≤ 140 characters).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    UnpauseCircle { circle_id: u64 },
    /// Same as `PauseCircle` when `emergency_stop_enabled` (default reason "emergency").
    EmergencyStop {
        circle_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Creator only. Before start: every lock back. After start, only before the
    /// first distribution: contributions back, the creator pays the exit penalty.
    CancelCircle { circle_id: u64 },
    /// Repay arrears (then refill the lock, for active members) at any time.
    /// Attach between 1 and `arrears + lock refill` (`arrears` once departed).
    RepayArrears { circle_id: u64 },
    UpdateCircle {
        circle_id: u64,
        circle_name: Option<String>,
        circle_description: Option<String>,
        circle_image: Option<String>,
    },
    /// v5 sends platform fees at payout time; kept for compatibility.
    WithdrawPlatformFees { circle_id: Option<u64> },
    /// Permissionless on closed circles: send stray funds to the platform.
    SweepDust { circle_id: u64 },
    /// Platform-funded reward pushed to the creator once the circle ended.
    DepositCreatorReward { circle_id: u64 },
    /// Admin adds a member to a private circle, attaching their lock.
    AddPrivateMember {
        circle_id: u64,
        member_address: Addr,
        #[serde(skip_serializing_if = "Option::is_none")]
        pseudonym: Option<String>,
    },
    UpdateMemberPseudonym {
        circle_id: u64,
        member_address: Addr,
        pseudonym: String,
    },
    /// Removed in v5 (use EjectMember).
    BlockMember { circle_id: u64, member_address: Addr },
    /// Removed in v5 (penalties are distributed by the engine).
    DistributeBlockedFunds { circle_id: u64, cycle: u32 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, QueryResponses)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    /// Live, time-derived view of the circle. Always correct for the block time
    /// of the query, even if nobody sent `Tick`.
    #[returns(CircleViewResponse)]
    GetCircleView {
        circle_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member: Option<Addr>,
    },
    #[returns(CircleResponse)]
    GetCircle { circle_id: u64 },
    #[returns(CirclesResponse)]
    GetCircles {
        start_after: Option<u64>,
        limit: Option<u32>,
        status: Option<CircleStatus>,
        creator: Option<Addr>,
    },
    #[returns(MembersResponse)]
    GetCircleMembers { circle_id: u64 },
    #[returns(StatusResponse)]
    GetCircleStatus { circle_id: u64 },
    #[returns(CycleResponse)]
    GetCurrentCycle { circle_id: u64 },
    #[returns(DepositsResponse)]
    GetCycleDeposits { circle_id: u64, cycle: u32 },
    #[returns(DepositsResponse)]
    GetMemberDeposits { circle_id: u64, member: Addr },
    #[returns(PayoutsResponse)]
    GetPayouts { circle_id: u64 },
    #[returns(PayoutsResponse)]
    GetPayoutHistory { circle_id: u64, cycle: Option<u32> },
    #[returns(BalanceResponse)]
    GetCircleBalance { circle_id: u64 },
    #[returns(BalanceResponse)]
    GetMemberBalance { circle_id: u64, member: Addr },
    #[returns(PenaltiesResponse)]
    GetPenalties { circle_id: u64, member: Option<Addr> },
    #[returns(RefundsResponse)]
    GetRefunds { circle_id: u64 },
    #[returns(PendingPayoutResponse)]
    GetPendingPayout { circle_id: u64, member: Addr },
    #[returns(AccumulatedLateFeesResponse)]
    GetMemberAccumulatedLateFees { circle_id: u64, member: Addr },
    #[returns(DepositRequirementResponse)]
    GetDepositRequirement { circle_id: u64, member: Addr },
    #[returns(EventsResponse)]
    GetEvents { circle_id: u64, limit: Option<u32> },
    #[returns(CircleStatsResponse)]
    GetCircleStats { circle_id: u64 },
    #[returns(MemberStatsResponse)]
    GetMemberStats { circle_id: u64, member: Addr },
    #[returns(MemberLockedAmountResponse)]
    GetMemberLockedAmount { circle_id: u64, member: Addr },
    #[returns(BlockedMembersResponse)]
    GetBlockedMembers { circle_id: u64 },
    #[returns(MemberPseudonymResponse)]
    GetMemberPseudonym { circle_id: u64, member: Addr },
    #[returns(PrivateMembersResponse)]
    GetPrivateMembers { circle_id: u64 },
    #[returns(DistributionCalendarResponse)]
    GetDistributionCalendar { circle_id: u64 },
    #[returns(ArchivedDateResponse)]
    GetArchivedDate { circle_id: u64 },
    #[returns(LockReserveResponse)]
    GetLockReserve { circle_id: u64, member: Addr },
    #[returns(LockReservesResponse)]
    GetLockReserves { circle_id: u64 },
    #[returns(EndCyclePreviewResponse)]
    PreviewEndCycle { circle_id: u64 },
    #[returns(WalletReputationResponse)]
    GetWalletReputation { address: Addr },
    /// 5 = time-driven engine with GetCircleView / Tick / pushed payouts.
    #[returns(ContractVersionResponse)]
    GetContractVersion {},
}

// ---------------------------------------------------------------------------
// v5 live view
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoundKind {
    /// Pot is pooled for the end of the cycle.
    Accumulate,
    /// Pot goes to one member.
    Turn,
    /// Pot + everything pooled during the cycle is split among remaining members.
    Group,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoundPhase {
    /// Not open yet.
    Upcoming,
    /// Open, before the due time.
    Open,
    /// Past the due time, still accepting late payments without penalty.
    Grace,
    /// Settlement time reached; the next transaction (or the keeper) settles it.
    Settling,
    Settled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct RoundView {
    pub round: u32,
    pub cycle: u32,
    pub round_in_cycle: u32,
    pub kind: RoundKind,
    pub phase: RoundPhase,
    /// Wall-clock instants (shifted by pauses; frozen while paused).
    pub opens_at: Timestamp,
    pub due_at: Timestamp,
    pub settles_at: Timestamp,
    /// Turn recipient, or the members sharing a Group round.
    pub recipients: Vec<Addr>,
    /// Members (active) who paid this round so far.
    pub paid: Vec<Addr>,
    pub expected_count: u32,
    /// Real contributions received so far for this round.
    pub collected: Uint128,
    /// Gross amount that will be distributed if everyone pays (incl. pooled carry for Group).
    pub expected_pot: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MemberView {
    pub address: Addr,
    pub slot: Option<u32>,
    pub status: MemberStatus,
    pub left_round: Option<u32>,
    pub lock: Uint128,
    pub lock_original: Uint128,
    pub arrears: Uint128,
    pub paid_through: u32,
    pub missed_total: u32,
    pub strikes: u32,
    pub contributed: Uint128,
    pub received: Uint128,
    pub refunded: Uint128,
    pub fees_paid: Uint128,
    pub last_received_cycle: u32,
    /// Round in which this member receives (Turn) or shares (Group) in the current cycle.
    pub payout_round_this_cycle: Option<u32>,
    /// Owed to this member: they left before being served in the cycle and get
    /// back what they put into it at what would have been their turn.
    #[serde(default)]
    pub claim: Uint128,
    /// Rounds paid during grace (after the due time).
    #[serde(default)]
    pub late_count: u32,
    /// Received their turn in the current cycle.
    #[serde(default)]
    pub served_this_cycle: bool,
    /// A push that failed and can be pulled with `Withdraw`.
    #[serde(default)]
    pub pending_payout: Uint128,
    /// An admin may eject this member now (active, with a strike or arrears).
    #[serde(default)]
    pub can_be_ejected: bool,
    /// What `ExitCircle` would do right now (only filled in `me`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_quote: Option<ExitQuote>,
}

/// Exact effect of `ExitCircle` for the viewing member at the view's block time.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ExitQuote {
    pub allowed: bool,
    /// `strict_mode` | `catching_up` | `creator_before_start` | `not_active` | `status`
    pub blocked_reason: Option<String>,
    /// Already received a turn in the current cycle (settle-up required).
    pub served_this_cycle: bool,
    /// Exit penalty taken from the lock (to the common pool).
    pub penalty: Uint128,
    /// (served) Remaining contributions of the cycle + arrears.
    pub owed: Uint128,
    /// (served) Part of `owed` taken from the lock.
    pub from_lock: Uint128,
    /// Funds to attach to `ExitCircle` (zero → attach nothing).
    pub to_attach: Uint128,
    /// Lock returned immediately.
    pub lock_refund: Uint128,
    /// Paid future rounds refunded immediately.
    pub prepaid_refund: Uint128,
    /// (not served) Claim acquired on what you put into the cycle.
    pub claim: Uint128,
    /// (not served) Part of the claim paid immediately.
    pub claim_paid_now: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MemberDue {
    /// Round the next deposit pays (oldest open unpaid round).
    pub next_round: Option<u32>,
    pub can_pay_now: bool,
    /// When `next_round` opens if it is not open yet.
    pub opens_at: Option<Timestamp>,
    pub due_at: Option<Timestamp>,
    pub min_payment: Uint128,
    /// min + arrears + lock refill. Recommended amount.
    pub max_payment: Uint128,
    pub arrears: Uint128,
    pub lock_refill: Uint128,
    pub strikes_left: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CircleViewResponse {
    pub engine: u8,
    pub circle_id: u64,
    pub status: CircleStatus,
    /// Block time of the query.
    pub now: Timestamp,
    pub paused: bool,
    pub denomination: String,
    pub contribution: Uint128,
    pub slots: u32,
    pub total_rounds: u32,
    pub rounds_settled: u32,
    pub total_cycles: u32,
    pub cycles_completed: u32,
    pub round_secs: u64,
    pub grace_secs: u64,
    pub threshold_start: u32,
    /// Strikes (unrepaired missed rounds) that trigger auto-ejection.
    pub max_strikes: u32,
    pub ejection_policy: crate::state::EjectionPolicy,
    pub start_at: Option<Timestamp>,
    pub end_at: Option<Timestamp>,
    /// Scheduled auto start (by date) while not started.
    pub auto_start_at: Option<Timestamp>,
    /// Focus round (oldest unsettled).
    pub round: Option<RoundView>,
    /// The following round (already open during the focus round's grace).
    pub next_round: Option<RoundView>,
    /// When the next automatic transition becomes due (keeper schedules on it).
    pub next_action_at: Option<Timestamp>,
    /// A transition is due right now: any `Tick` will execute it.
    pub tick_due: bool,
    /// Rounds the stored state is behind the clock (settled virtually in this view).
    pub rounds_behind: u32,
    pub carry_pot: Uint128,
    pub common_pool: Uint128,
    pub locks_total: Uint128,
    pub total_distributed: Uint128,
    pub total_arrears: Uint128,
    pub active_members: u32,
    pub members: Vec<MemberView>,
    pub me: Option<MemberView>,
    pub my_due: Option<MemberDue>,
    // ---- 0.5.1 ------------------------------------------------------------
    /// Cycle of the focus round (0 before start).
    #[serde(default)]
    pub current_cycle: u32,
    /// When the current pause ends by itself.
    #[serde(default)]
    pub paused_until: Option<Timestamp>,
    #[serde(default)]
    pub pause_reason: Option<String>,
    /// Longest single pause for this circle.
    #[serde(default)]
    pub max_pause_secs: u64,
    /// Pause budget left (all pauses together).
    #[serde(default)]
    pub pause_secs_left: u64,
    /// Set by `EndCycle`: the circle stops once this cycle is settled.
    #[serde(default)]
    pub ending_after_cycle: Option<u32>,
    /// `EndCycle` would end the circle now (no distribution yet in this cycle).
    #[serde(default)]
    pub end_now_allowed: bool,
    /// Addresses holding admin powers now.
    #[serde(default)]
    pub admins: Vec<Addr>,
    pub creator: Addr,
    #[serde(default)]
    pub co_admin: Option<Addr>,
    #[serde(default)]
    pub strict_mode: bool,
    #[serde(default)]
    pub exit_penalty_bps: u64,
    #[serde(default)]
    pub late_fee_bps: u64,
    #[serde(default)]
    pub platform_fee_bps: u64,
    /// Failed pushes waiting for `Withdraw`.
    #[serde(default)]
    pub pending_payouts_total: Uint128,
    /// Remainder of orphaned turns, settled at the end of the cycle.
    #[serde(default)]
    pub comp_pool: Uint128,
    /// `EmergencyStop` is available to admins (a pause with reason "emergency").
    #[serde(default)]
    pub emergency_stop_enabled: bool,
}

// ---------------------------------------------------------------------------
// v4-compatible responses
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CircleResponse {
    pub circle: crate::state::Circle,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CirclesResponse {
    pub circles: Vec<crate::state::Circle>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MembersResponse {
    pub members: Vec<Addr>,
    pub pending_members: Vec<Addr>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct StatusResponse {
    pub status: CircleStatus,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CycleResponse {
    pub current_cycle: u32,
    pub total_cycles: u32,
    pub next_payout_date: Option<Timestamp>,
    pub members_paid: Vec<Addr>,
    pub members_late: Vec<Addr>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct DepositsResponse {
    pub deposits: Vec<crate::state::DepositRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct PayoutsResponse {
    pub payouts: Vec<crate::state::PayoutRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct BalanceResponse {
    pub balance: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct PenaltiesResponse {
    pub penalties: Vec<crate::state::PenaltyRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct RefundsResponse {
    pub refunds: Vec<crate::state::RefundRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct EventsResponse {
    pub events: Vec<crate::state::EventLog>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CircleStatsResponse {
    pub circle_id: u64,
    pub total_members: u32,
    pub total_cycles: u32,
    pub cycles_completed: u32,
    pub total_amount_locked: Uint128,
    pub total_payouts: Uint128,
    pub total_penalties: Uint128,
    pub total_platform_fees: Uint128,
    pub total_pending_payouts: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MemberStatsResponse {
    pub member: Addr,
    pub circles_joined: u32,
    pub total_contributed: Uint128,
    pub total_received: Uint128,
    pub total_penalties: Uint128,
    pub missed_payments: u32,
    pub pending_payout: Uint128,
    pub accumulated_late_fees: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MemberLockedAmountResponse {
    pub amount: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct BlockedMembersResponse {
    pub blocked_members: Vec<(Addr, u32)>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MemberPseudonymResponse {
    pub pseudonym: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct PrivateMembersResponse {
    pub members: Vec<Addr>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct DistributionCalendarResponse {
    pub rounds: Vec<CalendarRound>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CalendarRound {
    pub round_number: u32,
    pub cycle_number: u32,
    /// v5: when contributions for this round are due.
    pub deposit_deadline: Timestamp,
    /// v5: when the round settles and pays out (due + grace).
    pub distribution_date: Timestamp,
    pub distribution_occurs: bool,
    /// Turn recipient (None for accumulation and group rounds).
    pub recipient: Option<Addr>,
    #[serde(default)]
    pub opens_at: Option<Timestamp>,
    #[serde(default)]
    pub kind: Option<RoundKind>,
    #[serde(default)]
    pub recipients: Vec<Addr>,
    #[serde(default)]
    pub settled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ArchivedDateResponse {
    pub archived_date: Option<Timestamp>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct PendingPayoutResponse {
    pub amount: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct AccumulatedLateFeesResponse {
    pub amount: Uint128,
    pub missed_rounds: u32,
    pub late_fee_per_round: Uint128,
    pub exit_penalty: Uint128,
    pub locked_amount: Uint128,
    pub rounds_until_ejection: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct DepositRequirementResponse {
    /// Recommended amount: contribution + arrears + lock refill.
    pub required_amount: Uint128,
    pub missed_rounds: u32,
    pub can_deposit: bool,
    pub contribution_amount: Uint128,
    /// v5: late fees are taken from the lock at settlement (0 here).
    pub late_fee_total: Uint128,
    #[serde(default)]
    pub lock_refill_needed: Uint128,
    #[serde(default)]
    pub arrears: Uint128,
    #[serde(default)]
    pub min_amount: Uint128,
    #[serde(default)]
    pub round: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ContractVersionResponse {
    pub api_version: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct LockReserveResponse {
    pub member: Addr,
    pub original_lock: Uint128,
    pub remaining_lock: Uint128,
    pub used_for_coverage: Uint128,
    pub is_blocked: bool,
    pub blocked_from_cycle: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct LockReservesResponse {
    pub reserves: Vec<LockReserveResponse>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct EndCyclePreviewResponse {
    pub settleable_amount: Uint128,
    pub eligible_members: Vec<Addr>,
    pub amount_per_member: Uint128,
    pub remainder: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct WalletReputationResponse {
    pub address: Addr,
    pub rating_count: u32,
    pub average_score: Option<String>,
    pub ratings: Vec<crate::state::MemberRating>,
}
