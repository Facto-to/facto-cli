# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-04-06

### Added

- `facto login` — authenticate via Privy OAuth, API key, or dev token
- `facto whoami` — display current account info
- `facto pipelines` — list DeFi positions with real-time on-chain balance
- `facto fund` — withdraw USDC from DeFi positions (Aave V3, Morpho)
- `facto pay` — call x402-enabled APIs with automatic payment signing
- `facto balance` — check server wallet USDC balance
- `facto services` — discover x402 services with keyword search and category filter
- `facto history` — view funding and x402 payment history
- `facto config` — configure per-chain deposit addresses
- `-t` / `--terse` flag for machine-readable JSON output on all commands
- `--dry-run` support for `fund` and `pay` commands
- HMAC-SHA256 authentication for API key mode
- Support for Base (8453), Monad (143), and Tempo (4217) chains

[0.1.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.1.0
