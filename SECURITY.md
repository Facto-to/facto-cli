# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Facto CLI, please report it responsibly.

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, email us at: **dev@facto.to**

Please include:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

We will acknowledge your report within 48 hours and provide a timeline for a fix.

## Scope

The following are in scope:

- Credential storage and handling (`~/.facto/credentials.json`)
- Authentication flows (Privy OAuth, API key, HMAC signing)
- x402 payment signing and transaction construction
- Network communication (API calls, proxy requests)

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | Yes       |

## Security Practices

- Credentials are stored locally in the user's home directory, never transmitted except to authenticated endpoints
- HMAC-SHA256 is used for API key authentication
- All network requests use TLS (rustls)
- No secrets are hardcoded in the binary
- The `FACTO_API_URL` override is intended for development only
