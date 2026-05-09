# AGENTS.md — facto-cli

Rust CLI for **takara-facto** — DeFi-funded agent payments via x402 / Monad MPP / Monad x402. Distributed via `cargo install facto-cli` and (more commonly) the install script at `https://monad-api.facto.to/install`. Cross-AI / cross-tool entry — Codex, Aider, Cursor, Claude Code, plain terminal — read this first.

## What you've cloned

`facto-cli` is one sub-repo of takara-facto. Stack: Rust 2021 edition, MSRV 1.75. Sibling sub-repos:
- `facto-engine` (Rust backend the CLI talks to)
- `frontend` (web apps)
- `facto-contracts` (Solidity)
- `facto-pay`, `facto-infra`

The monorepo top-level `takara-facto/` itself has no git.

## I'm a new contributor — get me building locally

```bash
bash scripts/dev/init.sh
```

What it does:
1. Verify cargo / rustc installed (MSRV 1.75)
2. `cargo check`
3. `cargo test`
4. Check `.env.local` exists for any optional env-var overrides

The CLI itself only reads `FACTO_API_URL` (override the default backend URL) and `HOME` (standard). Pure development needs neither. `.env.local` is only relevant when testing against a non-default backend.

## Verb reference

| Command | Purpose |
|---|---|
| `bash scripts/dev/init.sh`        | First-day setup orchestrator |
| `bash scripts/dev/preflight.sh`   | Env-var presence check |
| `cargo check --all-targets`       | Compile sanity (L1) |
| `cargo test`                      | Run all tests (L2) |
| `cargo build --release`           | Build release binary (L4) |
| `cargo run -- <subcommand>`       | Run CLI directly during dev |
| `bash install.sh`                 | Local install (mirrors public flow) |

In Claude Code these are also `/onboard` and `/preflight`.

## Project rules (cross-AI authoritative)

The project-wide rule set lives in `../facto-engine/docs/harness/PROJECT_CONSTRAINTS.md`. CLI-specific:

- **No human-hour estimates** — evaluate by task volume + complexity + scope + dependency depth
- **Secrets never in chat** — Privy / Turnkey login flows write tokens to `~/.facto/`; AI must refuse chat-pasted secrets
- **Cross-repo contracts** — when changing CLI ↔ backend API call shapes, update or check `../facto-engine/docs/harness/cross_repo_changes.md`
- **Validation levels**: `cargo fmt --check` (L0), `cargo clippy` (L1), `cargo test` (L2), `cargo build --release` (L4)

## Where to read deeper

- `SKILL.md` (this repo) — Skill packaging for the published CLI (x402 payment context for agentic uses)
- `../facto-engine/AGENTS.md` — backend onboarding (the engine the CLI talks to)
- `../facto-engine/docs/harness/AGENTS.md` — full project agent harness
- `../facto-engine/docs/harness/PROJECT_CONSTRAINTS.md` — every project rule
- `README.md` — public-facing CLI docs (how end-users consume it)
- `../facto-engine/docs/harness/contracts/http-api.md` — backend API contract surface
- `../facto-engine/docs/harness/cross_repo_changes.md` — cross-repo change notifications
