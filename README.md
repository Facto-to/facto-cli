# Facto CLI

[![Crates.io](https://img.shields.io/crates/v/facto-cli.svg)](https://crates.io/crates/facto-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

DeFi-funded agent payments via [x402](https://www.x402.org/) and Monad MPP.

Facto CLI withdraws USDC from your DeFi positions (Aave V3, Morpho) and pays machine-priced APIs automatically. Today that means:

- Base-first `x402`
- Monad `x402`
- Monad-first MPP with `monad/charge`

The CLI is built for AI agents and operators who want one command flow for login, funding, and payment without hand-managing wallet transactions.

## How It Works

```
Your DeFi Position (Aave V3 / Morpho)
        |
        | facto fund / auto-fund
        v
  Payment Wallet (USDC)
        |
        | facto pay (auto Base x402 / Monad x402 / Monad MPP)
        v
  Paid API  -->  Response
```

1. You deposit USDC into a supported DeFi pipeline through [facto.xyz](https://facto.xyz)
2. Facto CLI resolves the target payment protocol and chooses a compatible execution pipeline
3. If the payment wallet is short, the CLI can auto-fund it from the selected pipeline
4. The CLI retries the paid request after the payment challenge is satisfied

Browser setup surfaces are deployment-aware: `facto login`, `facto pipeline create`, and other browser handoff links are sourced from the active Facto backend, so a frontend domain change does not require a separate CLI URL patch. If backend CLI metadata is temporarily unavailable, the CLI falls back to the agentic user frontend at `https://facto-pay-agentic.vercel.app`.

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

## Shared Setup

```bash
# 1. Authenticate (opens browser for Privy OAuth)
facto login

# 2. Start pipeline setup in the browser
facto pipeline create

# On agentic surfaces this opens the payment-surface chooser.
# Optional direct shortcuts:
facto pipeline create --x402
facto pipeline create --monad-x402
facto pipeline create --mpp

# 3. Inspect operator state
facto balance
facto pipelines
```

After that, choose the path that matches how you work.

## Operator Setup Checklist

- Install the latest `facto-cli` binary.
- Authenticate the operator with `facto login`.
- Link a payment pipeline with `facto pipeline create`.
- Verify payment wallet balance and active pipelines with `facto balance` and `facto pipelines`.

## Use With An Agent

Give your agent the CLI plus the machine-facing setup docs:

```bash
codex "Use facto-cli plus https://monad-api.facto.to/SKILL.md to find a low-cost BTC price API, pay for it, and return only JSON."
```

- Skill: `https://monad-api.facto.to/SKILL.md`
- Catalog: `https://monad-api.facto.to/llms.txt`

Your agent can decide whether it needs `facto services`, `facto pay`, `facto balance`, or `facto pipelines`.

Notes:

- `facto services` is still the x402 discovery surface.
- Monad x402 and Monad MPP services are still best treated as direct URL execution paths through `facto pay`.
- `facto pay` defaults to `--protocol auto`, so it can route to Base x402, Monad x402, or Monad MPP.

Prompt starters:

```bash
codex "Use facto-cli to find a Base token risk API, analyze 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913, and summarize the paid result in five bullets."

claude "使用 facto-cli 和 https://monad-api.facto.to/SKILL.md，查找 Base 钱包分析类 x402 API，分析 0x0a4CAA57ac414f6B936261ff7CB1d6883bBF7264，并返回重点结论。"
```

## Use Manually

```bash
# 1. Discover services yourself
facto services "weather"

# 2. Call an x402 API
facto pay GET "https://x402.aurelianflo.com/api/weather/current?lat=40.7&lon=-74.0" \
  --max-amount 0.001 \
  --yes

# 3. Call a Monad MPP API
facto pay POST "https://monad-api.facto.to/api/sec-edgar/submissions" \
  --protocol mpp \
  --data '{"cik":"320193"}' \
  --max-amount 0.001 \
  --yes
```

Use `--yes` for copy-paste, agent-driven, and automation-friendly runs. It skips the confirmation prompts and, when the execution wallet is short, auto-funds the exact required amount before retrying the paid request.

## Commands

| Command | Description |
|---------|-------------|
| `facto login` | Authenticate via browser (Privy OAuth) or dev token |
| `facto pipeline create` | Open the browser to start pipeline setup; agentic surfaces show the Base x402 / Monad x402 / Monad MPP chooser |
| `facto pipeline create --x402` | Jump straight to the Base x402 onboarding flow |
| `facto pipeline create --monad-x402` | Jump straight to the Monad x402 onboarding flow |
| `facto pipeline create --mpp` | Jump straight to the Monad MPP onboarding flow |
| `facto whoami` | Diagnostic summary for the current operator account |
| `facto balance` | Check payment wallet USDC balance on the selected pipeline's chain, or a given chain |
| `facto pipelines` | List DeFi positions with real-time balance and spending limits |
| `facto pipelines default [ID]` | Show or set the selected default pipeline used for auto-selected payments |
| `facto fund --amount N` | Withdraw N USDC from a DeFi position to your wallet |
| `facto pay METHOD URL` | Call an API with automatic Base x402, Monad x402, or Monad MPP payment handling |
| `facto services [query]` | Discover x402-enabled services, optionally filtered by keyword |
| `facto history` | Show past funding and payment transactions |
| `facto config` | Configure deposit addresses for cross-chain transfers |
| `facto logout` | Clear local credentials |

### Common Flags

| Flag | Description |
|------|-------------|
| `-t, --terse` | Output compact JSON (machine-readable, ideal for agents) |
| `--dry-run` | Preview a transaction without executing it |

## Supported Chains

| Chain | ID | Protocol | Use Case |
|-------|----|----------|----------|
| Base | 8453 | Aave V3 | x402 payments, fund withdrawals |
| Monad | 143 | Morpho | Monad x402, Monad MPP, fund withdrawals |

## Authentication

Facto CLI supports these authentication modes today:

```bash
# Browser-based (default) — opens Privy OAuth flow
facto login

# First-time operator path — create a payment pipeline in the browser
facto pipeline create

# Dev token — for local testing only
facto login --dev-token "dev:<user_id>"
```

`--api-key` / `--signing-key` are reserved for future server and CI support, but CLI user commands currently require browser login or `--dev-token`.

Credentials are stored in `~/.facto/credentials.json`.

The active Facto backend now provides the login and pipeline-create browser URLs. The CLI only keeps a legacy fallback for older backend deployments.

## Agent Integration

The `-t` (terse) flag outputs structured JSON on every command, designed for programmatic consumption by AI agents and scripts:

```bash
# Call a paid API
facto -t pay GET "https://x402.aurelianflo.com/api/weather/current?lat=40.7&lon=-74.0" --yes
# => {"status":"paid","protocol":"x402|mpp","payment":{"charge_id":"...","reference":"0x...","amount_display":"$0.0010"},"response":{"status_code":200,"body":"..."}}

# Discover services
facto -t services "weather"
# => [{"url":"https://...","name":"Weather API","price_usdc":"0.0050",...}]

# Check balance
facto -t balance
# => {"usdc_balance_display":"$0.0500",...}

# Pin the pipeline used by auto-selected balance/pay commands
facto pipelines default <PIPELINE_ID>
```

When `--chain` is omitted, `facto balance` and `facto pay` prefer your selected default pipeline. For `facto pay`, the CLI may temporarily use a different compatible execution pipeline for that one payment without changing your saved default.

### Typical Operator Workflow

1. `facto login` — authenticate the CLI
2. `facto pipeline create` — connect a payment pipeline
3. `facto balance` / `facto pipelines` — inspect payment capacity and pipeline selection
4. `facto pay` — let the CLI auto-resolve Base x402 vs Monad x402 vs Monad MPP
5. If balance is low: `facto fund --amount 5` or let `facto pay` auto-fund when supported
6. `facto pipelines default <ID>` — pin the pipeline/chain used as your selected default

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
