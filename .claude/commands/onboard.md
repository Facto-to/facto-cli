# /onboard

Run the contributor-onboarding orchestrator for facto-cli.

## Steps

1. `bash scripts/dev/init.sh` from repo root.
2. If cargo missing, walk user through rustup install.
3. If `cargo check` / `cargo test` reports issues, surface and offer to fix.

## Notes

- All logic in `scripts/dev/init.sh` — thin redirect.
- Cross-AI fallback: same bash command.
- `--skip-test` for faster init.
