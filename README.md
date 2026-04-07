# Facto CLI

[![Crates.io](https://img.shields.io/crates/v/facto-cli.svg)](https://crates.io/crates/facto-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

DeFi-funded Agent payments via [x402 protocol](https://www.x402.org/).

Facto CLI withdraws USDC from your DeFi positions (Aave V3, Morpho) and pays x402-enabled APIs automatically. Built for AI agents and developers who need programmatic access to paid APIs without managing wallets manually.

## How It Works

```
Your DeFi Position (Aave V3 / Morpho)
        |
        | facto fund (withdraw USDC)
        v
  Server Wallet (USDC)
        |
        | facto pay (automatic x402 signing)
        v
  x402-enabled API  -->  Response
```

1. You deposit USDC into a DeFi protocol (Aave V3, Morpho) through [facto.xyz](https://facto.xyz)
2. Facto CLI withdraws from your position on demand
3. When calling a paid API, the CLI detects the `402 Payment Required` response, signs a USDC payment, and retries — all in one command

## Install

**Pre-built binary** (macOS / Linux):

```bash
curl -fsSL https://monad-api.facto.to/install | bash
```

**Via Cargo**:

```bash
cargo install facto-cli
```

The binary is named `facto`. The bundled installer writes to `~/.facto/bin/facto`. `cargo install` follows Cargo's bin directory, typically `~/.cargo/bin/facto`.

## Quick Start

```bash
# 1. Authenticate (opens browser for Privy OAuth)
facto login

# 2. Check your DeFi positions and available balance
facto pipelines

# 3. Withdraw USDC to your server wallet
facto fund --amount 5

# 4. Call a paid API — payment is handled automatically
facto pay GET "https://x402.aurelianflo.com/api/weather/current?lat=40.7&lon=-74.0" --chain 8453

# 5. Discover more x402 services
facto services
```

## Commands

| Command | Description |
|---------|-------------|
| `facto login` | Authenticate via browser (Privy OAuth) or dev token |
| `facto whoami` | Show current account and pipeline count |
| `facto pipelines` | List DeFi positions with real-time balance and spending limits |
| `facto fund --amount N` | Withdraw N USDC from a DeFi position to your wallet |
| `facto pay METHOD URL` | Call an API with automatic x402 payment handling |
| `facto balance` | Check server wallet USDC balance on a given chain |
| `facto services [query]` | Discover x402-enabled services, optionally filtered by keyword |
| `facto history` | Show past funding and payment transactions |
| `facto config` | Configure deposit addresses for cross-chain transfers |
| `facto logout` | Clear local credentials |

### Common Flags

| Flag | Description |
|------|-------------|
| `-t, --terse` | Output compact JSON (machine-readable, ideal for agents) |
| `--dry-run` | Preview a transaction without executing it |
| `--chain <ID>` | Target chain ID (default varies by command) |

## Supported Chains

| Chain | ID | Protocol | Use Case |
|-------|----|----------|----------|
| Base | 8453 | Aave V3 | x402 payments, fund withdrawals |
| Monad | 143 | Morpho | x402 payments, fund withdrawals |
| Tempo | 4217 | — | Fund withdrawals (direct transfer) |

## Authentication

Facto CLI supports these authentication modes today:

```bash
# Browser-based (default) — opens Privy OAuth flow
facto login

# Dev token — for local testing only
facto login --dev-token "dev:<user_id>"
```

`--api-key` / `--signing-key` are reserved for future server and CI support, but CLI user commands currently require browser login or `--dev-token`.

Credentials are stored in `~/.facto/credentials.json`.

## Agent Integration

The `-t` (terse) flag outputs structured JSON on every command, designed for programmatic consumption by AI agents and scripts:

```bash
# Check balance before making a call
facto -t pipelines
# => {"pipelines":[{"route_id":"...","balance":"10.50","asset_symbol":"USDC",...}]}

# Call a paid API
facto -t pay GET "https://api.example.com/data" --chain 8453
# => {"status":"paid","payment":{"amount":"5000","charge_id":"..."},"response":{"status_code":200,"body":"..."}}

# Discover services
facto -t services "weather"
# => [{"url":"https://...","name":"Weather API","price_usdc":"0.0050",...}]
```

### Typical Agent Workflow

1. `facto -t pipelines` — inspect DeFi pipeline balance
2. `facto -t balance --chain 8453` — check server-wallet balance for x402 spends
3. If balance is low: `facto fund --amount 5`
4. `facto -t services "<keyword>"` — find the right API
5. `facto -t pay GET "<url>"` — call and parse the response

## Configuration

| Item | Location |
|------|----------|
| Credentials | `~/.facto/credentials.json` |
| Binary | `~/.facto/bin/facto` |
| API URL override | `FACTO_API_URL` environment variable |

### Deposit Addresses

Configure per-chain deposit addresses for cross-chain fund transfers:

```bash
facto config --deposit-address arbitrum 0xYourAddress...
facto config --show
```

## Building from Source

```bash
git clone https://github.com/Facto-to/facto-cli.git
cd facto-cli
cargo build --release
# Binary at target/release/facto
```

### Requirements

- Rust 1.75+
- An account on [facto.xyz](https://facto.xyz) with an active DeFi position

## Contributing

Contributions are welcome. Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Security

If you discover a security vulnerability, please report it responsibly. See [SECURITY.md](SECURITY.md) for details.

## License

[MIT](LICENSE)
