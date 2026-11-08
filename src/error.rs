use cosmwasm_std::StdError;
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Payment(#[from] cw_utils::PaymentError),

    #[error("Unauthorized: {msg}")]
    Unauthorized { msg: String },

    #[error("Circle not found: {circle_id}")]
    CircleNotFound { circle_id: u64 },

    #[error("Circle is not in the correct status. Expected: {expected}, Got: {actual}")]
    InvalidCircleStatus { expected: String, actual: String },

    #[error("Circle is full. Max members: {max}")]
    CircleFull { max: u32 },

    #[error("Circle is invite-only")]
    InviteOnly { circle_id: u64 },

    #[error("Member already joined")]
    AlreadyMember { address: String },

    #[error("Insufficient funds. Required: {required}, Sent: {sent}")]
    InsufficientFunds { required: String, sent: String },

    #[error("Invalid payment: send between {min} and {max}, sent {sent}")]
    InvalidPayment { min: String, max: String, sent: String },

    #[error("Circle has not started yet")]
    CircleNotStarted { circle_id: u64 },

    #[error("Strict mode: voluntary exits are only possible right after a cycle ends")]
    StrictModeNoExit { circle_id: u64 },

    #[error("Member {member} is up to date (no missed round, no arrears): an admin can only eject for cause")]
    NoCause { member: String },

    #[error("Pause budget exhausted: this circle has already been paused for the maximum total time")]
    PauseBudgetExhausted {},

    #[error("Exit not allowed")]
    ExitNotAllowed { circle_id: u64 },

    #[error("Minimum members not met. Required: {required}, Current: {current}")]
    MinMembersNotMet { required: u32, current: u32 },

    #[error("Round {round} is not open yet. It opens at {opens_at}")]
    RoundNotOpen { round: u32, opens_at: u64 },

    #[error("Every round is already paid")]
    NothingToPay {},

    #[error("Nothing to settle yet. Next automatic action at: {next_action_at}")]
    NothingToDo { next_action_at: u64 },

    #[error("The circle is catching up on overdue rounds. Send Tick, then retry.")]
    CatchingUp {},

    #[error("Removed in engine v5: {msg}")]
    Removed { msg: String },

    #[error("Invalid parameters: {msg}")]
    InvalidParameters { msg: String },

    #[error("No pending payouts available for withdrawal")]
    NoPendingPayouts {},

    #[error("Cannot cancel: at least one distribution has occurred. Circle must complete or use emergency procedures.")]
    CancelNotAllowedAfterDistribution {},
}
