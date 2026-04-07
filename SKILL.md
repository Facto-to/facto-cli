# Facto — DeFi-Funded Agent Payments

Use this skill to call paid APIs. Facto withdraws USDC from your
DeFi positions and pays x402 services automatically.

Supported chains:
- Monad (chain 143) — Morpho protocol
- Base (chain 8453) — Aave V3 protocol

## Setup (one-time, requires user interaction)

1. Install:
   curl -fsSL https://monad-api.facto.to/install | bash

2. Login (opens browser):
   "$HOME/.facto/bin/facto" login

3. Verify:
   "$HOME/.facto/bin/facto" whoami

Important: Use full absolute paths. Do not use `export PATH=...`.

## Check Available Funding

"$HOME/.facto/bin/facto" pipelines -t

Shows DeFi positions available for withdrawal across all chains:
- Available USDC balance (Morpho on Monad, Aave V3 on Base)
- Per-transaction and daily spending limits remaining
- Wallet balance for x402 payments

## Fund Your Wallet

"$HOME/.facto/bin/facto" fund --amount <USDC_AMOUNT>

Withdraws USDC from DeFi position to your wallet.
Use --pipeline <ID> to choose which chain/protocol.
Returns:
- Transaction hash
- Block explorer URL
- Facto transaction detail URL
- Updated wallet balance

## Call Paid APIs

"$HOME/.facto/bin/facto" pay -t <METHOD> <URL> [--data <JSON>]

Calls the URL. If the API requires x402 payment, Facto handles
signing and payment automatically. Works with both paid and free APIs.

Default chain is Monad (143). Use --chain 8453 for Base x402 services.

Examples:
  "$HOME/.facto/bin/facto" pay -t GET "https://defi-api.monad.xyz/tvl"
  "$HOME/.facto/bin/facto" pay -t --chain 8453 GET "https://api.example.com/data"
  "$HOME/.facto/bin/facto" pay -t POST "https://analytics.monad.xyz/query" --data '{"sql": "..."}'

## Workflow

1. "$HOME/.facto/bin/facto" pipelines -t → check wallet balance
2. If wallet balance < expected cost: "$HOME/.facto/bin/facto" fund --amount 5
3. "$HOME/.facto/bin/facto" pay -t GET "https://target-api.com/endpoint"
4. Parse the response.body from the JSON output

## Commands

| Command | Description |
|---------|-------------|
| facto login | Browser-based Privy authentication |
| facto whoami | Show account and balance |
| facto pipelines -t | Check DeFi balance and wallet balance |
| facto fund --amount N | Withdraw N USDC from Morpho to wallet |
| facto fund --amount N --dry-run | Preview without executing |
| facto pay -t METHOD URL | Call API with automatic x402 payment (Monad) |
| facto pay -t --chain 8453 METHOD URL | Call API via Base x402 |
| facto history -t | Show funding and payment history |
| facto logout | Clear local credentials |

## Rules

- Always use -t flag for machine-readable JSON output
- Check pipelines before fund to verify available balance
- Use --dry-run for amounts over $1
- facto pay works with both paid (402) and free (200) APIs
- All amounts in human-readable USDC (e.g., "10", "0.50")
- After funding, report the explorer URL and Facto URL to the user
