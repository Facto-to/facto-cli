# Facto — DeFi-Funded Agent Payments

Use this skill to call paid APIs. Facto withdraws USDC from your
DeFi positions and pays x402 services automatically.

## Setup (one-time, requires user interaction)

1. Install:
   curl -fsSL https://monad-api.facto.to/install | bash

2. Login (opens browser):
   facto login

3. Create the Base payment pipeline:
   facto pipeline create

## Call Paid APIs

facto pay -t GET "https://x402.aurelianflo.com/api/weather/current?lat=40.7&lon=-74.0"

Calls the URL. If the API requires x402 payment, Facto handles
signing and payment automatically. Works with both paid and free APIs.

## Discover Services

facto services -t "weather"

Search x402-enabled APIs by keyword. Returns JSON with URL, price, and description.

## Workflow

1. facto login → authenticate
2. facto pipeline create → create and authorize the Base payment pipeline
3. facto -t services "<keyword>" → find the right API
4. facto -t pay GET "<url>" → call and parse response.body
5. If insufficient balance error: facto fund --amount 5 → auto-selects pipeline
6. Retry the pay command

## Check Balance (if needed)

facto -t balance
facto -t pipelines

## Commands

| Command | Description |
|---------|-------------|
| facto login | Browser-based Privy authentication |
| facto pipeline create | Open the browser to create a Base payment pipeline |
| facto whoami | Show account info |
| facto services -t [query] | Discover x402 services (JSON) |
| facto pay -t METHOD URL | Call API with automatic x402 payment |
| facto balance -t | Check server wallet USDC balance |
| facto pipelines -t | Check DeFi pipeline balance |
| facto fund --amount N | Withdraw N USDC from DeFi to wallet |
| facto history -t | Show payment history |
| facto logout | Clear local credentials |

## Rules

- Always use -t flag for machine-readable JSON output
- facto pay auto-detects the chain from the x402 response — no --chain needed
- `facto login` only authenticates; first-time users still need `facto pipeline create`
- facto fund auto-selects the pipeline — no --chain or --pipeline needed for single pipeline
- If fund fails with "insufficient balance", report the pipeline balance to the user
- All amounts in human-readable USDC (e.g., "10", "0.50")
