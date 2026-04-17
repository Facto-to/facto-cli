# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.2] - 2026-04-17

### Added

- `facto pipeline create --monad-x402` as a direct browser handoff shortcut for the Monad x402 setup surface

### Changed

- Agentic setup now follows a three-surface model: Base x402, Monad x402, and Monad MPP
- `facto pipeline create --x402` remains pinned to Base for backward compatibility, while `--protocol x402 --chain monad` now resolves to the Monad x402 handoff
- CLI docs, install output, and hosted install guidance now describe the three setup surfaces explicitly

## [0.6.1] - 2026-04-16

### Added

- `facto pipeline create` now supports `--x402` and `--mpp` as direct browser handoff shortcuts for Base x402 and Monad MPP pipeline setup

### Changed

- The default browser handoff for `facto pipeline create` is now neutral (`/pipelines/create?source=cli`) so agentic frontends can offer a payment-surface chooser without forcing Base-specific behavior
- CLI setup docs, install output, and hosted install guidance now explain the shared chooser flow plus the direct `--x402` / `--mpp` shortcuts
- Full-mode user frontends continue to fall back to the existing raw pipeline creation wizard instead of the agentic chooser/onboarding surface

## [0.6.0] - 2026-04-16

### Fixed

- Successful Monad MPP payments now surface complete CLI payment summaries, including amount, recipient, method, and on-chain reference instead of `?` placeholders
- MPP pay-route tests no longer race on duplicate rustls crypto-provider installation during repeated test setup

### Changed

- Public CLI examples now default to `--yes` for copy-paste, agent, and automation flows so runs do not stop on interactive funding and confirmation prompts
- `llms.txt`, installer output, and the seeded MPP catalog examples now reflect the same non-interactive `facto pay ... --yes` guidance

## [0.5.9] - 2026-04-15

### Added

- `facto pay` now supports `--protocol auto|x402|mpp`, plus a resolve step that can temporarily choose a compatible execution pipeline without silently changing the user's selected default pipeline
- Monad MPP now has dedicated CLI funding and balance paths through backend `/v1/mpp/balance` and `/v1/mpp/fund`, keeping the public MPP flow isolated from the older x402/card entry points

### Changed

- The CLI now documents a Monad-first MPP rollout: Base x402 remains supported, Monad `monad/charge` is the first MPP method exposed, and Tempo is no longer described as a public CLI payment path
- `facto pay` can auto-fund the Monad payment wallet from the selected pipeline before retrying the MPP purchase, matching the existing x402 operator experience more closely
- README and `SKILL.md` now explain the new `selected pipeline` versus `execution pipeline` model for automatic protocol and chain selection

## [0.5.8] - 2026-04-13

### Changed

- Legacy browser-handoff fallback now also points at `https://facto-pay-agentic.vercel.app`, so both `prod` and `dev` CLI environments fall back to the same agentic user frontend when backend CLI metadata is unavailable
- The fallback frontend no longer branches on local CLI environment; the backend remains the primary source of truth, and the local default is now a single deployment-safe agentic surface

## [0.5.7] - 2026-04-12

### Changed

- Browser handoff URLs now come from the active Facto backend deployment instead of being hardcoded inside the CLI, so `facto login` and `facto pipeline create` stay aligned when the user app domain changes
- CLI now caches the backend-provided frontend URL for follow-up browser flows such as funding recovery and charge detail links, while keeping a legacy fallback for older backend deployments
- README and `SKILL.md` now document that browser setup surfaces are deployment-aware and sourced from the backend

## [0.5.6] - 2026-04-12

### Changed

- Login guidance is now more explicitly agent-first: after setup, operators are told to inspect `facto balance` / `facto pipelines` or hand control to the hosted `SKILL.md`
- README, install guidance, and developer-facing docs now include a short operator setup checklist and copy-ready agent prompt starters
- Command help text now distinguishes operator diagnostics (`whoami`, `balance`, `pipelines`) from the lower-level manual request path (`services`, `pay`)

## [0.5.5] - 2026-04-11

### Changed

- Login and `whoami` no longer expose a misleading Base pipeline authorization-pending state; first-run guidance now only distinguishes missing Base pipeline vs. Base pipeline detected
- Terse login / whoami output now exposes only actionable onboarding hints (`next_command`, `continue_url`) instead of a brittle readiness state field
- README, install guidance, and `SKILL.md` now treat `login -> pipeline create` as the shared setup, then split into agent-driven and manual CLI paths

## [0.5.4] - 2026-04-11

### Added

- `facto pipeline create` as a visible alias-backed entry point that opens the Base pipeline creation flow in the browser

### Changed

- First-run CLI guidance now follows `login -> pipeline create -> services/pay` instead of implying authentication alone is enough for paid requests
- Login and `whoami` now surface Base pipeline setup hints for first-run operators
- Missing-pipeline flows now point to `/pipelines/create?source=cli&chain=base` instead of the generic pipeline list page

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

[0.5.7]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.7
[0.6.1]: https://github.com/Facto-to/facto-cli/releases/tag/v0.6.1
[0.6.2]: https://github.com/Facto-to/facto-cli/releases/tag/v0.6.2
[0.6.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.6.0
[0.5.9]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.9
[0.5.8]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.8
[0.5.4]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.4
[0.5.6]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.6
[0.5.5]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.5
[0.5.3]: https://github.com/Facto-to/facto-cli/releases/tag/v0.5.3
[0.3.1]: https://github.com/Facto-to/facto-cli/releases/tag/v0.3.1
[0.3.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.3.0
[0.2.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.2.0
[0.1.0]: https://github.com/Facto-to/facto-cli/releases/tag/v0.1.0
