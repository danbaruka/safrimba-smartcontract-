use cosmwasm_std::{Addr, Timestamp, Uint128};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Settlement engine generation stored on every circle created by this code.
/// v5 = time-driven, lazily-settled, push-payout engine ("clockwork").
pub const ENGINE_VERSION: u8 = 5;

/// On-chain circle aggregate.
///
/// The field set is a strict superset of the v4 wire format so existing
/// clients keep parsing `GetCircle`. All `Timestamp` fields are UTC epoch
/// instants. Vocabulary (kept from v4 for wire compatibility):
///   * a **round** is one contribution period (`cycle_duration_*` long);
///     `current_cycle_index`, `DepositRecord::cycle` and `PayoutRecord::cycle`
///     are absolute **round** indexes (1-based);
///   * a **cycle** is one full rotation = `members_at_start` rounds;
///     `total_cycles` counts rotations.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct Circle {
    // General Information
    pub circle_id: u64,
    pub circle_name: String,
    pub circle_description: String,
    pub circle_image: Option<String>,
    pub creator_address: Addr,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,

    // Membership Parameters
    pub max_members: u32,
    pub min_members_required: u32,
    pub invite_only: bool,
    /// Active members (joined and not ejected/exited).
    pub members_list: Vec<Addr>,
    pub pending_members: Vec<Addr>,

    // Financial Parameters
    pub contribution_amount: Uint128,
    pub denomination: String,
    /// Display: contribution × slots (one full round pot).
    pub payout_amount: Uint128,
    /// Basis points of the member's original lock forfeited on ejection / exit.
    pub exit_penalty_percent: u64,
    /// Basis points of `contribution_amount` charged (from the lock) per missed round.
    pub late_fee_percent: u64,
    pub platform_fee_percent: u64,
    /// Strikes (unrepaired missed rounds) that trigger auto-ejection. Fixed at start.
    pub max_missed_payments_allowed: u32,
    /// Number of payout slots, fixed at start (= members when the circle started).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members_at_start: Option<u32>,

    // Cycle & Time Parameters
    pub total_cycles: u32,
    pub cycle_duration_days: u32,
    #[serde(default)]
    pub cycle_duration_seconds: u64,
    /// Schedule anchor (round 1 opens here). Set at start.
    pub start_date: Option<Timestamp>,
    /// Same as `start_date` once started (kept for v4 clients).
    pub first_cycle_date: Option<Timestamp>,
    /// Wall-clock opening time of the focus round (oldest unsettled round).
    pub next_payout_date: Option<Timestamp>,
    /// Wall-clock time at which the last round is due.
    pub end_date: Option<Timestamp>,
    pub grace_period_hours: u32,
    #[serde(default)]
    pub grace_period_seconds: u64,
    pub auto_start_when_full: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_start_type: Option<String>, // "by_members" | "by_date"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_start_date: Option<Timestamp>,

    // Payout Logic Parameters
    pub payout_order_type: PayoutOrderType,
    pub payout_order_list: Option<Vec<Addr>>,
    /// Always true on v5: settlement is automatic and permissionless.
    pub auto_payout_enabled: bool,
    /// Always false on v5 (kept for wire compatibility).
    pub manual_trigger_enabled: bool,

    // Security & Risk Controls
    pub emergency_stop_enabled: bool,
    pub emergency_stop_triggered: bool,
    pub auto_refund_if_min_not_met: bool,
    pub strict_mode: bool,

    // Escrow and Funds Management (display aggregates, maintained by the engine)
    pub escrow_address: Option<Addr>,
    /// Sum of every member's remaining security lock.
    pub total_amount_locked: Uint128,
    /// Penalties + recovered arrears waiting for the final split (= `common_pool`).
    pub total_penalties_collected: Uint128,
    /// Cumulative platform fees sent to the platform address.
    pub total_platform_fees_collected: Uint128,
    /// Legacy pull-payout ledger. Always zero for v5 (payouts are pushed).
    pub total_pending_payouts: Uint128,
    pub withdrawal_lock: bool,
    pub refund_mode: RefundMode,

    // Locking
    pub creator_lock_amount: Uint128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distribution_threshold: Option<DistributionThreshold>,

    // Internal State Parameters
    pub circle_status: CircleStatus,
    /// Focus round: the oldest round that is not settled yet (1-based).
    pub current_cycle_index: u32,
    /// Fully settled cycles (rotations).
    pub cycles_completed: u32,
    /// Members who paid the focus round on time.
    pub members_paid_this_cycle: Vec<Addr>,
    /// Members who paid the focus round after its due time (during grace).
    pub members_late_this_cycle: Vec<Addr>,

    // Optional UX / Customization Parameters
    pub visibility: Visibility,
    pub show_member_identities: bool,

    #[serde(default)]
    pub ejection_policy: EjectionPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co_admin_address: Option<Addr>,

    // ---- v5 engine state -------------------------------------------------
    #[serde(default)]
    pub engine_version: u8,
    /// Rounds fully settled (0..=total rounds).
    #[serde(default)]
    pub rounds_settled: u32,
    /// Accumulated paused seconds (deadlines shift by this amount).
    #[serde(default)]
    pub paused_secs_total: u64,
    /// Set while paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<Timestamp>,
    /// Current cycle's pooled-but-not-yet-distributed contributions.
    #[serde(default)]
    pub carry_pot: Uint128,
    /// Late fees, exit penalties, recovered arrears and orphaned pots,
    /// split equally among active members at the end.
    #[serde(default)]
    pub common_pool: Uint128,
    /// Cumulative gross amount distributed to recipients.
    #[serde(default)]
    pub total_distributed: Uint128,
    /// Contributions that could not be covered (owed by members).
    #[serde(default)]
    pub total_arrears: Uint128,
    /// When the circle reached a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<Timestamp>,
    /// Set by `EndCycle` once a payout happened in the current cycle: the
    /// circle closes when this cycle is fully settled (PLAN_V5 3.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_after_cycle: Option<u32>,
    /// Reason given for the current pause (≤ 140 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
    /// What is left of orphaned turns (after the leaver's claim), waiting for
    /// the end of the cycle, where it fills each participant's gap between what
    /// they put into the cycle and what they received from it.
    #[serde(default)]
    pub comp_pool: Uint128,
}

impl Circle {
    /// Effective round duration in seconds.
    pub fn cycle_duration_secs(&self) -> u64 {
        if self.cycle_duration_seconds > 0 {
            self.cycle_duration_seconds
        } else {
            self.cycle_duration_days as u64 * 86400
        }
    }

    /// Effective grace period in seconds.
    pub fn grace_period_secs(&self) -> u64 {
        if self.grace_period_seconds > 0 {
            self.grace_period_seconds
        } else {
            self.grace_period_hours as u64 * 3600
        }
    }

    /// Payout slots per cycle once started.
    pub fn slots(&self) -> u32 {
        self.members_at_start.unwrap_or(self.max_members).max(1)
    }

    pub fn total_rounds(&self) -> u32 {
        self.slots().saturating_mul(self.total_cycles)
    }

    pub fn auto_eject_enabled(&self) -> bool {
        !matches!(self.ejection_policy, EjectionPolicy::AdminOnly)
    }

    pub fn manual_eject_enabled(&self) -> bool {
        !matches!(self.ejection_policy, EjectionPolicy::AutoOnly)
    }

    /// No more rounds will settle. `Finalizing` (legacy pending payouts still
    /// owed) is terminal too: only `Withdraw` acts on it.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.circle_status,
            CircleStatus::Completed
                | CircleStatus::Cancelled
                | CircleStatus::Ended
                | CircleStatus::Finalizing
        )
    }

    pub fn is_started(&self) -> bool {
        matches!(
            self.circle_status,
            CircleStatus::Running
                | CircleStatus::Paused
                | CircleStatus::Finalizing
                | CircleStatus::Completed
                | CircleStatus::Ended
        ) || self.members_at_start.is_some()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum CircleStatus {
    Draft,
    Open,
    Full,
    Running,
    Paused,
    /// Legacy: final distribution credited to PENDING_PAYOUTS, awaiting withdrawals.
    /// v5 only uses it when legacy pending payouts are still owed.
    Finalizing,
    /// Every round settled and every obligation paid out.
    Completed,
    Cancelled,
    /// Ended early (admin, or fewer than two active members); funds settled.
    Ended,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum PayoutOrderType {
    PredefinedOrder,
    RandomOrder,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum RefundMode {
    FullRefund,
    PartialRefund,
    AutoDistribute,
}

/// When distribution starts inside each cycle.
///
/// v5 unified rule with `s` = slots and `t` = threshold start round:
///   * rounds `1..t-1` accumulate into the cycle pot;
///   * rounds `t..s-1` each pay one member (payout order `0..s-t-1`) that round's pot;
///   * the last round pays every remaining member an equal share of its pot
///     plus everything accumulated during the cycle.
///
/// `Total` is `t = s` (one equal split at the end of each cycle);
/// `MinMembers { count: 1 }` is the classic rotating tontine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DistributionThreshold {
    Total {},
    MinMembers { count: u32 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum EjectionPolicy {
    AdminOnly,
    #[default]
    AutoAndAdmin,
    AutoOnly,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum Visibility {
    Public,
    Private,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    Active,
    /// Removed by the rules (auto) or by an admin.
    Ejected,
    /// Left voluntarily after start.
    Exited,
}

/// Per-member ledger (v5). One entry per address that ever joined.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MemberState {
    pub address: Addr,
    /// Position in the payout order (0-based). `u32::MAX` before start.
    pub slot: u32,
    pub status: MemberStatus,
    /// Round in which the member left (0 while active). The member stays liable
    /// for contributions until the end of that round's cycle.
    pub left_round: u32,
    /// Remaining security lock held by the contract.
    pub lock: Uint128,
    /// Lock deposited at join (creator: 2× contribution).
    pub lock_original: Uint128,
    /// Contributions that could not be covered by the lock.
    pub arrears: Uint128,
    /// Highest round that is closed for this member (paid, or settled as missed).
    /// Invariant: rounds `rounds_settled+1 ..= paid_through` are paid for real.
    pub paid_through: u32,
    pub missed_total: u32,
    /// Unrepaired missed rounds; reset once arrears and lock are restored.
    pub strikes: u32,
    pub paid_count: u32,
    /// Real contributions (contribution_amount per paid round).
    pub contributed: Uint128,
    /// Lock consumed to cover missed contributions.
    pub covered: Uint128,
    /// Late fees + exit penalty taken from the lock.
    pub fees_paid: Uint128,
    /// Extra paid on deposits to clear arrears / refill the lock.
    pub extra_paid: Uint128,
    /// Net amount pushed to the member (payouts, not lock refunds).
    pub received: Uint128,
    /// Lock and other amounts refunded.
    pub refunded: Uint128,
    /// Cycle in which the member last received a payout (0 = never).
    pub last_received_cycle: u32,
    pub joined_at: Timestamp,
    /// Owed TO a member who left before being served in the cycle they left
    /// (their contributions to that cycle, net of arrears). Paid from the pooled
    /// carry at departure, then from what would have been their turn; whatever
    /// is still unpaid when that cycle ends is extinguished (PLAN_V5 3.3 a).
    #[serde(default)]
    pub claim: Uint128,
    /// Total claim acquired (paid + outstanding + extinguished).
    #[serde(default)]
    pub claim_acquired: Uint128,
    /// Rounds paid during grace, after the due time (reputation only).
    #[serde(default)]
    pub late_count: u32,
    /// Cycle the two counters below refer to (stale counters read as zero).
    #[serde(default)]
    pub cy: u32,
    /// Contributions of cycle `cy` that were neither paid nor covered.
    #[serde(default)]
    pub cy_short: Uint128,
    /// Gross turn / group payouts received in cycle `cy`.
    #[serde(default)]
    pub cy_received: Uint128,
}

impl MemberState {
    pub fn new(address: Addr, lock: Uint128, joined_at: Timestamp) -> Self {
        MemberState {
            address,
            slot: u32::MAX,
            status: MemberStatus::Active,
            left_round: 0,
            lock,
            lock_original: lock,
            arrears: Uint128::zero(),
            paid_through: 0,
            missed_total: 0,
            strikes: 0,
            paid_count: 0,
            contributed: Uint128::zero(),
            covered: Uint128::zero(),
            fees_paid: Uint128::zero(),
            extra_paid: Uint128::zero(),
            received: Uint128::zero(),
            refunded: Uint128::zero(),
            last_received_cycle: 0,
            joined_at,
            claim: Uint128::zero(),
            claim_acquired: Uint128::zero(),
            late_count: 0,
            cy: 0,
            cy_short: Uint128::zero(),
            cy_received: Uint128::zero(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.status == MemberStatus::Active
    }

    /// Point the per-cycle counters at `cycle` (resetting them on a new cycle).
    pub fn touch_cycle(&mut self, cycle: u32) {
        if self.cy != cycle {
            self.cy = cycle;
            self.cy_short = Uint128::zero();
            self.cy_received = Uint128::zero();
        }
    }

    /// (uncovered, received) in `cycle`.
    pub fn cycle_counters(&self, cycle: u32) -> (Uint128, Uint128) {
        if self.cy == cycle {
            (self.cy_short, self.cy_received)
        } else {
            (Uint128::zero(), Uint128::zero())
        }
    }

    /// Lock that must be added back to reach the original lock.
    pub fn refill_needed(&self) -> Uint128 {
        self.lock_original.saturating_sub(self.lock)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct PayoutRecord {
    pub cycle: u32,
    pub recipient: Addr,
    pub amount: Uint128,
    pub timestamp: Timestamp,
    pub transaction_hash: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct DepositRecord {
    pub member: Addr,
    pub cycle: u32,
    pub amount: Uint128,
    pub timestamp: Timestamp,
    pub on_time: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct PenaltyRecord {
    pub member: Addr,
    pub cycle: u32,
    pub amount: Uint128,
    pub reason: String,
    pub timestamp: Timestamp,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct RefundRecord {
    pub member: Addr,
    pub amount: Uint128,
    pub reason: String,
    pub timestamp: Timestamp,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct EventLog {
    pub event_type: String,
    pub circle_id: u64,
    pub data: String,
    pub timestamp: Timestamp,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct PlatformConfig {
    pub platform_fee_percent: u64,
    pub platform_address: Addr,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct MemberRating {
    pub circle_id: u64,
    pub ratee: Addr,
    pub rater: Addr,
    /// 1..=5 participation score
    pub score: u8,
    pub note: Option<String>,
    pub timestamp: Timestamp,
}

/// Bank sends of the last transaction, kept so `reply` can tell which send
/// failed (reply id = index). Overwritten by every transaction that sends.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct SendBatch {
    pub circle_id: u64,
    pub denom: String,
    pub sends: Vec<(Addr, Uint128)>,
}

// --- MONEY INVARIANT (asserted by the test-suite after every step) -----------
// bank(contract, denom) ==
//     Σ member.lock
//   + carry_pot + common_pool + comp_pool
//   + Σ contribution × (member.paid_through − rounds_settled)⁺   (paid, unsettled)
//   + total_pending_payouts                     (sends that failed, withdrawable)
// and bank == total_pending_payouts once the circle is Completed / Cancelled / Ended.
// Claims (`MemberState::claim`) are promises funded from future pots, not money held.

// Storage
pub const PLATFORM_CONFIG: Item<PlatformConfig> = Item::new("platform_config");
pub const CIRCLE_COUNTER: Item<u64> = Item::new("circle_counter");
pub const CIRCLES: Map<u64, Circle> = Map::new("circles");
/// v5 member ledger.
pub const MEMBERS: Map<(u64, &Addr), MemberState> = Map::new("v5_members");
/// (circle_id, round, recipient)
pub const PAYOUTS: Map<(u64, u32, Addr), PayoutRecord> = Map::new("payouts");
/// (circle_id, member, round) — real deposits and lock-covered (synthetic) ones.
pub const DEPOSITS: Map<(u64, Addr, u32), DepositRecord> = Map::new("deposits");
pub const PENALTIES: Map<(u64, Addr, u32), PenaltyRecord> = Map::new("penalties");
pub const REFUNDS: Map<(u64, Addr), RefundRecord> = Map::new("refunds");
pub const EVENTS: Map<(u64, u64), EventLog> = Map::new("events");
pub const EVENT_COUNTER: Map<u64, u64> = Map::new("event_counter");
/// Pull ledger: sends that failed (credited by `reply`), released by `Withdraw`.
pub const PENDING_PAYOUTS: Map<(u64, Addr), Uint128> = Map::new("pending_payouts");
/// Sends of the current transaction (see [`SendBatch`]).
pub const SEND_BATCH: Item<SendBatch> = Item::new("v5_send_batch");
pub const MEMBER_PSEUDONYMS: Map<(u64, Addr), String> = Map::new("member_pseudonyms");
pub const PRIVATE_MEMBER_LIST: Map<u64, Vec<Addr>> = Map::new("private_member_list");
/// Platform-funded creator reward (idempotency guard).
pub const CREATOR_REWARDS_CREDITED: Map<u64, Uint128> = Map::new("creator_rewards_credited");
/// (circle_id, ratee, rater) → rating
pub const MEMBER_RATINGS: Map<(u64, Addr, Addr), MemberRating> = Map::new("member_ratings");
/// (ratee, n) → (circle_id, rater)
pub const WALLET_RATING_INDEX: Map<(Addr, u64), (u64, Addr)> = Map::new("wallet_rating_index");
pub const WALLET_RATING_COUNT: Map<Addr, u64> = Map::new("wallet_rating_count");
