# Changelog

All notable changes to the Safrimba smart contract are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- (Nothing yet)

### Changed

- (Nothing yet)

### Fixed

- (Nothing yet)

### Security

- (Nothing yet)

---

## [0.1.0] - Initial release

### Added

- CosmWasm smart contract for digital tontine on Safrochain (SAF tokens).
- Circle management: create, join, invite-only or public circles with configurable parameters.
- Automatic payouts: cycle duration, payout order (random/fixed), distribution thresholds (Total, None, MinMembers).
- Penalty system: late fees and penalties for missed payments.
- Security controls: emergency stop, arbitration, pause/unpause.
- On-chain event logging for operations.
- Execute messages: CreateCircle, JoinCircle, InviteMember, AcceptInvite, LockJoinDeposit, StartCircle, UpdateCircle, CancelCircle, ExitCircle, DepositContribution, AdvanceRound, CheckAndEject, PauseCircle, UnpauseCircle, ProcessPayout, Withdraw, and related admin/arbiter actions.
- Query messages: GetCircle, GetCircleMembers, GetCurrentCycle, and related queries.
- Deployment and test scripts (deploy.sh, test_create_circle.sh, test_distribution_threshold.sh, test_actions_full.sh).
- Schema generation (examples/schema).

### Fixed

- WASM build: reference-types disabled via `.cargo/config.toml` for chain compatibility.
- Optimizer: use single-contract `cosmwasm/optimizer:0.14.0` instead of workspace-optimizer; Makefile and deploy.sh updated.

---

[Unreleased]: https://github.com/your-org/safrimba-smartcontract/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/your-org/safrimba-smartcontract/releases/tag/v0.1.0
