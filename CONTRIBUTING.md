# Contributing

Thank you for helping improve IPTV Gateway. Read this guide before you make a change.

## Before you start

1. Read the [README](README.md) and the repository instructions in [AGENTS.md](AGENTS.md).
2. Check existing tasks before you start work.
3. Create a focused branch for your change.
4. Do not commit passwords, tokens, provider URLs, or other private data.

Use `.env.example` for local configuration. Use `.env.test` for deterministic tests. Keep personal environment files untracked.

## Make a change

1. Keep each change within one clear scope.
2. Add or update tests for changed behavior.
3. Update user-facing documentation when behavior or configuration changes.
4. Keep generated files out of commits unless the task requires them.
5. Preserve the [MIT license](LICENSE) and [third-party notices](THIRD_PARTY_NOTICES.md).

Use the repository terms and interfaces that the existing code uses. Use safe Rust. Treat `unsafe` as a last resort.

## Run checks

Run the standard repository test command from the repository root:

```bash
./scripts/run-test-suite.sh
```

The command runs noncredentialed checks in isolated Compose resources. It removes runner-owned resources after the run.

Use `--help` to view options. Use `--keep` only when you need to inspect runner-owned Compose resources after a failure.

Use `--live` only when you have authorized live-provider credentials. Keep `.env.live` untracked and do not add its contents to logs or issues.

Set safe overrides when required:

```bash
IPTV_TEST_DATABASE_URL=postgres://iptv:iptv-development@127.0.0.1:54329/iptv ./scripts/run-test-suite.sh
```

The runner uses temporary directories and isolated Compose project names. It preserves shared build caches.

Run focused checks during development. Run the full command before you submit a change.

## Submit a change

1. Write a clear commit message.
2. Include the test commands and results in the change description.
3. Explain configuration changes and migration steps.
4. Confirm that no secret file is tracked.
5. Keep review changes small and easy to verify.

Report security issues with the process in [SECURITY.md](SECURITY.md). Do not use a public issue for sensitive reports.
