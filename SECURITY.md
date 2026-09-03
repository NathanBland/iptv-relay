# Security

IPTV Gateway handles provider credentials, administrator credentials, session data, and output tokens. Protect this data during development and operation.

## Report a vulnerability

Do not open a public issue for a suspected vulnerability. Use the repository host's private vulnerability-reporting feature or contact the project maintainers through a private channel.

Include these details:

- A short description of the issue.
- The affected commit, release, or component.
- Clear reproduction steps.
- The possible security impact.
- A safe proof of concept, when one is necessary.
- Your preferred contact method for follow-up.

Remove passwords, tokens, provider URLs, personal data, and other secrets from the report. Encrypt attachments when the reporting channel supports encryption.

The maintainers will acknowledge a report when they can. They will assess the report, request needed details, and coordinate a fix or mitigation.

## Protect local secrets

1. Copy `.env.example` to `.env` for local setup.
2. Replace every placeholder with a local value.
3. Keep `.env`, `.env.local`, `.env.live`, and other environment variants untracked.
4. Use `.env.test` only for deterministic nonproduction test values.
5. Do not place live-provider credentials in source files, test fixtures, logs, or issues.
6. Rotate a credential immediately when you suspect exposure.

The repository ignores environment variants and keeps only safe templates tracked. Confirm the result with `git status --short` before you commit.

## Security expectations

- Use HTTPS through a trusted reverse proxy for exposed deployments.
- Use strong random values for output tokens, bootstrap tokens, database passwords, and master keys.
- Use an Argon2id password hash for production administrator access.
- Restrict database and administration endpoints to trusted networks.
- Keep the Rust toolchain and project dependencies current.
- Run `./scripts/run-test-suite.sh` before you submit a security-related change.

See [LICENSE](LICENSE) for project terms and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for dependency notices.
