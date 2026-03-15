# Contributing to Safrimba Smart Contract

Thank you for your interest in contributing. This document explains how to get started, our conventions, and how to submit changes.

## Getting started

### Prerequisites

- **Rust** 1.70 or later
- **wasm32 target**: `rustup target add wasm32-unknown-unknown`
- **Docker** (optional but recommended for optimized WASM builds)
- **safrochaind** CLI (for deployment and manual testing)

### Build and test

```bash
# Clone the repository
git clone <repository-url>
cd safrimba-smartcontract-

# Build the contract
cargo build --release --target wasm32-unknown-unknown

# Run unit tests
cargo test

# Format and lint
cargo fmt
cargo clippy --target wasm32-unknown-unknown
```

### Optimized build

For a production-ready WASM artifact (required for deployment to Safrochain):

```bash
make optimize
```

See [FIXES.md](FIXES.md) for optimizer details and alternatives if Docker is not available.

### Deployment and integration tests

- Deployment: [scripts/README.md](scripts/README.md)
- Contract and script tests: [scripts/README_TEST.md](scripts/README_TEST.md)

## Code style

- **Formatting**: Run `cargo fmt` before committing.
- **Linting**: Fix or justify any `cargo clippy` warnings for the `wasm32-unknown-unknown` target.
- **Contract behavior**: Keep the state machine and allowed actions consistent with [CONTRACT_STATES_EXPLAINED.md](CONTRACT_STATES_EXPLAINED.md). Document any new states or transitions there.

## Commit messages

- Use a short, clear summary line (e.g. "Add validation for min_members_required").
- Optionally add a body with details and reference issues (e.g. "Fixes #42").

## How to contribute

### Reporting bugs

Open an issue using the [Bug report](.github/ISSUE_TEMPLATE/bug_report.md) template. Include:

- Steps to reproduce
- Expected vs actual behavior
- Environment (Rust version, chain/testnet, contract code ID if relevant)

### Suggesting features

Open an issue using the [Feature request](.github/ISSUE_TEMPLATE/feature_request.md) template.

### Submitting changes

1. Fork the repository and create a branch from `main`.
2. Make your changes; ensure `cargo test`, `cargo fmt`, and `cargo clippy` pass.
3. If you change execute/query messages or state, update the schema if applicable (`cargo run --example schema`).
4. Open a pull request and fill out the [pull request template](.github/PULL_REQUEST_TEMPLATE.md).
5. Link any related issues.

## Scope

This repo contains the CosmWasm smart contract and its scripts/schemas. Changes to contract logic, message types, state, or deployment/testing scripts are in scope. For frontend or backend integration, use the appropriate repositories and reference this contract’s docs (e.g. [CONTRACT_STATES_EXPLAINED.md](CONTRACT_STATES_EXPLAINED.md)).

## Code of conduct

Please follow our [Code of Conduct](CODE_OF_CONDUCT.md) in all interactions.

## Questions

If something is unclear, open a discussion or an issue and we’ll do our best to help.
