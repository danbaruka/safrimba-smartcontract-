# Safrimba Smart Contract

[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![CosmWasm](https://img.shields.io/badge/CosmWasm-1.5-blue.svg)](https://github.com/CosmWasm/cosmwasm)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

A CosmWasm smart contract implementing a digital tontine system on Safrochain. The contract manages all deposits, rules, and payouts in SAF tokens automatically.

## Table of contents

- [Features](#features)
- [Contract structure](#contract-structure)
- [Documentation](#documentation)
- [Building](#building)
- [Deployment](#deployment)
- [Usage](#usage)
- [Query examples](#query-examples)
- [Contributing](#contributing)
- [License](#license)

## Features

- **Circle Management**: Create, join, and manage tontine circles with configurable parameters
- **Automatic Payouts**: Scheduled payouts based on cycle duration and payout order
- **Penalty System**: Late fees and penalties for missed payments
- **Security Controls**: Emergency stop, arbitration, and pause/unpause functionality
- **Transparent Tracking**: On-chain event logging for all operations
- **Flexible Membership**: Public or invite-only circles with configurable member limits

## Contract structure

- `src/contract.rs` - Entry points (instantiate, execute, query)
- `src/execute.rs` - Execute message handlers
- `src/query.rs` - Query message handlers
- `src/state.rs` - State definitions and storage
- `src/msg.rs` - Message types
- `src/error.rs` - Error types

## Documentation

- [Contract states and flow](CONTRACT_STATES_EXPLAINED.md) – State machine (Draft → Open → Full → Running → Completed/Cancelled), allowed actions, and frontend integration.
- [Build and deployment fixes](FIXES.md) – Known fixes (reference-types, optimizer) and deployment notes.
- [Deployment scripts](scripts/README.md) – Deploy to testnet/mainnet with `deploy.sh`.
- [Testing](scripts/README_TEST.md) – Test scripts for create circle, distribution threshold, and full actions.

## Building

### Prerequisites

- Rust 1.70+
- `wasm32-unknown-unknown` target
- CosmWasm optimizer (optional, for optimized builds)

### Build Commands

```bash
# Install wasm32 target
rustup target add wasm32-unknown-unknown

# Build the contract
cargo build --release --target wasm32-unknown-unknown

# Generate schemas (optional)
cargo run --example schema
```

### Optimize build (recommended)

The project uses the **single-contract** optimizer (`cosmwasm/optimizer:0.14.0`). Using `workspace-optimizer` is not supported (see [FIXES.md](FIXES.md)).

```bash
# Using Make (recommended)
make optimize
```

Or with Docker directly:

```bash
docker pull cosmwasm/optimizer:0.14.0
docker run --rm -v "$(pwd)":/code \
  --mount type=volume,source="$(basename "$(pwd)")_cache",target=/code/target \
  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
  cosmwasm/optimizer:0.14.0
```

If Docker is unavailable, you can try `make optimize-local` (requires `wasm-opt`; see [FIXES.md](FIXES.md) for Safrochain compatibility).

## Deployment

The optimized WASM file will be in `artifacts/safrimba_contract.wasm` after optimization.

Deploy to Safrochain testnet using:

```bash
# Upload the contract
safrochaind tx wasm store artifacts/safrimba_contract.wasm \
  --from <your-key> \
  --chain-id safrochain-testnet \
  --gas auto \
  --gas-adjustment 1.3 \
  --fees 1000000saf

# Instantiate the contract
safrochaind tx wasm instantiate <code-id> \
  '{"platform_fee_percent": 100, "platform_address": "addr_safro..."}' \
  --from <your-key> \
  --chain-id safrochain-testnet \
  --label "Safrimba Contract" \
  --admin <admin-address> \
  --gas auto \
  --fees 1000000saf
```

## Usage

### Creating a Circle

```json
{
  "create_circle": {
    "circle_name": "My Tontine Circle",
    "circle_description": "A savings circle for friends",
    "max_members": 10,
    "min_members_required": 5,
    "contribution_amount": "1000000",
    "total_cycles": 10,
    "cycle_duration_days": 30,
    "payout_order_type": "random_order",
    "auto_payout_enabled": true,
    ...
  }
}
```

### Joining a Circle

```json
{
  "join_circle": {
    "circle_id": 1
  }
}
```

### Depositing Contribution

Send SAF tokens along with:

```json
{
  "deposit_contribution": {
    "circle_id": 1
  }
}
```

### Processing Payout

```json
{
  "process_payout": {
    "circle_id": 1
  }
}
```

## Query Examples

### Get Circle Information

```json
{
  "get_circle": {
    "circle_id": 1
  }
}
```

### Get Circle Members

```json
{
  "get_circle_members": {
    "circle_id": 1
  }
}
```

### Get Current Cycle

```json
{
  "get_current_cycle": {
    "circle_id": 1
  }
}
```

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) and our [Code of Conduct](CODE_OF_CONDUCT.md) before opening an issue or pull request.

## License

This project is licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the full text.
