# Facto — DeFi-Funded Agent Payments

Use this skill to call paid APIs. Facto withdraws USDC from your
DeFi positions and pays x402 or Monad MPP services automatically.

## Setup (one-time, requires user interaction)

1. Install:
   curl -fsSL https://monad-api.facto.to/install | bash

2. Login (opens browser):
   facto login

3. Create a payment pipeline:
   facto pipeline create
   # For Monad MPP testing, you can open the Monad-specific create flow:
   facto pipeline create --chain monad

4. Check operator state if needed:
   facto -t balance
   facto -t pipelines

Browser handoff URLs come from the active Facto backend deployment, so `facto login` and `facto pipeline create` stay aligned if the user app domain changes. If backend CLI metadata is unavailable, the CLI falls back to `https://facto-pay-agentic.vercel.app`.

## Operator Handoff

After shared setup, the operator usually stops typing low-level payment commands.
Point the runtime at:

- https://monad-api.facto.to/SKILL.md
- https://monad-api.facto.to/llms.txt

## Call Paid APIs

facto pay -t GET "https://x402.aurelianflo.com/api/weather/current?lat=40.7&lon=-74.0"

Calls the URL. If the API requires x402 payment, Facto handles
signing and payment automatically. For Monad MPP services, use the direct URL
and keep `--protocol auto` unless you need to force `--protocol mpp`.

## Discover Services

facto services -t "weather"

Search x402-enabled APIs by keyword. Returns JSON with URL, price, and description.
This is still the x402 discovery surface; Monad MPP services are paid by direct URL.

## Workflow

1. facto login → authenticate
2. facto pipeline create → create a payment pipeline
3. facto -t balance / facto -t pipelines → inspect capacity when needed
4. If the target URL is unknown: facto -t services "<keyword>" → find the right API
5. facto -t pay GET|POST "<url>" → CLI auto-resolves x402 vs Monad MPP
6. If the payment wallet is short, `facto pay` may auto-fund from a compatible pipeline
7. Retry only if the CLI reports a real payment failure

## Check Balance (if needed)

facto -t balance
facto -t pipelines

## Commands

| Command | Description |
|---------|-------------|
| facto login | Browser-based Privy authentication |
| facto pipeline create | Open the browser to create a payment pipeline |
| facto whoami | Show account info |
| facto services -t [query] | Discover x402 services (JSON) |
| facto pay -t METHOD URL | Call API with automatic x402 or Monad MPP payment |
| facto balance -t | Check payment wallet USDC balance |
| facto pipelines -t | Check DeFi pipeline balance |
| facto fund --amount N | Withdraw N USDC from DeFi to wallet |
| facto history -t | Show payment history |
| facto logout | Clear local credentials |

## Rules

- Always use -t flag for machine-readable JSON output
- facto pay defaults to `--protocol auto` and can route to either x402 or Monad MPP
- `facto login` only authenticates; first-time users still need `facto pipeline create`
- After setup, agents should start from a concrete task and only use `facto services` when discovery is actually needed
- After setup, operators usually stop at `facto balance` / `facto pipelines` and hand control to the agent
- facto fund auto-selects the pipeline — no --chain or --pipeline needed for single pipeline
- The selected default pipeline stays unchanged even if `facto pay` temporarily uses another compatible execution pipeline
- If fund fails with "insufficient balance", report the pipeline balance to the user
- All amounts in human-readable USDC (e.g., "10", "0.50")
