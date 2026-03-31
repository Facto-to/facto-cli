# Contributing to Facto CLI

Thank you for your interest in contributing. Here's how to get started.

## Development Setup

```bash
git clone https://github.com/Facto-to/facto-cli.git
cd facto-cli
cargo build
cargo test
```

### Requirements

- Rust 1.75+ (install via [rustup](https://rustup.rs/))
- A Facto account for integration testing (optional)

## Making Changes

1. Fork the repository and create a feature branch from `main`
2. Write your code and add tests where appropriate
3. Run `cargo fmt` and `cargo clippy` before committing
4. Keep commits focused — one logical change per commit
5. Open a pull request against `main`

## Code Style

- Follow existing patterns in the codebase
- Use `anyhow` for error handling in command implementations
- Add `--terse` JSON output support for any new commands
- Keep dependencies minimal — prefer standard library solutions

## Testing

```bash
cargo test          # Run unit tests
cargo clippy        # Lint check
cargo fmt -- --check  # Format check
```

For integration testing against a local engine, set `FACTO_API_URL=http://localhost:8080`.

## Reporting Issues

- Use [GitHub Issues](https://github.com/Facto-to/facto-cli/issues) to report bugs
- Include your OS, Rust version (`rustc --version`), and CLI version (`facto --version`)
- Provide steps to reproduce the issue

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
