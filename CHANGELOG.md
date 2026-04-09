# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.3] - 2026-04-09

### Fixed

- `facto balance` and `facto pay` now honor the selected default pipeline when `--chain` is omitted instead of always using the first active pipeline
- Default pipeline cache is refreshed from backend preferences before auto-selecting the chain, keeping CLI behavior aligned with the user's current pipeline selection
- Release CI is clean again: remove the unused `chain_id` field from pipeline init results and resolve the Clippy warning in interactive confirmation flow

### Changed

- README now documents `facto pipelines default [ID]` and clarifies how auto-selected chains work for `balance` and `pay`

## [0.3.1] - 2026-04-07

### Fixed

- Install script auto-configures PATH in `.zshrc` / `.bashrc` — `facto` works immediately in new shells
- Install URL updated to `monad-api.facto.to/install` (live endpoint)

### Changed

- Simplify all docs: remove `--chain` flags (auto-detected), remove manual fund step from quickstart
- Agent workflow: discover → pay → fund only if needed
- SKILL.md rewritten for cleaner agent integration

## [0.3.0] - 2026-04-07

### Added

- Clear `Settling` vs `Confirmed` history output for x402 payments, including settlement transaction hash extraction from x402 response payloads

### Fixed

- Preserve configured deposit addresses across re-login flows
- Reject unsupported API key login for user commands with an explicit error instead of storing broken credentials
- Stop `fund --dry-run` from mutating recipient allowlists
- Return non-zero exit codes for `facto pay` validation and payment errors instead of printing success-shaped output
- Surface authentication failures from `whoami` and `pipelines` instead of silently returning empty success responses
- Show micro-amount USDC values accurately in `facto history`
- Use BaseScan links for Base funding transactions

### Changed

- README and command help now document browser login and dev-token flows as the supported authentication paths for CLI user commands
- Cargo install documentation now distinguishes Cargo's binary directory from the bundled install script location

## [0.2.0] - 2026-04-06

### Added

- `facto config --env dev|prod` — switch between dev and prod environments
- Integration test suite (`tests/smoke.sh`) for local and remote verification

### Fixed

- Recipient allowlist uses user-level endpoint (`/v1/recipients`) matching engine validation
- Remove legacy `tempo_wallet` field from credentials and all commands

### Changed

- Default environment set to `dev` (`https://monad-api.facto.to`)
- `prod` environment uses `https://api.facto.xyz`
- `FACTO_API_URL` env var takes highest priority over config

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

[0.5.3]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.3
[0.3.1]: https://github.com/Facto-to/facto-cli/releases/tag/v0.3.1
[0.3.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.3.0
[0.2.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.2.0
[0.1.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.1.0
