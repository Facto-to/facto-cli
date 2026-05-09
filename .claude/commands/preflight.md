# /preflight

Env-var presence check for facto-cli (mostly a placeholder — the CLI reads almost no env at runtime).

## Steps

1. `bash scripts/dev/preflight.sh` from repo root.
2. Reports whether `FACTO_API_URL` is overriding the default backend, or if the CLI will use prod default.
