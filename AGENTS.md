# Agent Instructions

## Technical documentation

Use ASD-STE100 Simplified Technical English Issue 9 for all technical documentation.

Apply these rules:

- Use an approved word only for its approved meaning and part of speech.
- Use one term for one meaning.
- Use project terms as technical nouns or technical verbs.
- Use American English spelling.
- Write instructions in the imperative form.
- Put a condition before its instruction.
- Write only one instruction in each sentence.
- Limit each instruction to 20 words.
- Limit each descriptive sentence to 25 words.
- Use one topic in each paragraph.
- Limit each paragraph to six sentences.
- Use the active voice.
- Use simple verb tenses.
- Do not use an `-ing` form as a verb.
- Do not omit necessary words.
- Use vertical lists for complex information.
- Use consistent punctuation and capitalization.

Use the official Issue 9 standard as the authority:

- <https://www.asd-ste100.org/assets/files/ASD-STE100_ISSUE9.pdf>

## Kaneo project tracking

Kaneo is the authoritative project and task tracker for this repository.

Use the `iptv-relay` Kaneo project.

Before substantial work:

1. Search Kaneo for an existing task that describes the work.
2. Use the existing task when it has the correct scope.
3. Create a task when no appropriate task exists.
4. Give each new task a clear problem statement and expected outcome.
5. Move the task to `In Progress` when implementation starts.

During work:

- Add comments for discoveries, decisions, blockers, and scope changes.
- Keep the task description and status aligned with the actual work.
- Link related tasks when appropriate.
- Do not mark a task `Done` because code exists.
- Do not mark a task `Done` until an associated Git commit exists.

When work finishes:

1. Verify the requested behavior.
2. Add a concise completion comment.
3. Confirm that the associated Git commit exists.
4. State what changed, how verification passed, and the commit hash.
5. Move the task to `Done` only after validation and commit checks succeed.
6. Document outstanding work and keep the task in the correct nonfinal stage.

Never invent Kaneo task IDs, project IDs, user IDs, or status names.

Query Kaneo before you use those values.

Do not use `IMPLEMENTATION_PLAN.md` or `IMPLEMENTATION_STATUS.md` as active trackers.

Treat both files as archived migration records.

Do not put credentials, secret URLs, or tokens in Kaneo.

## Git commits

Do not add `Co-Authored-By` tags or `Generated with [Devin]` lines to commit messages.

Do not claim authorship or co-authorship for commits that other tools or agents wrote.

## Rust safety

Prefer safe Rust in all code.

Treat `unsafe` as a last resort.

Exhaust all safe alternatives before you use `unsafe`.

Document the safety invariant when `unsafe` is necessary.

## Test runner

Use `./scripts/run-test-suite.sh` as the standard repository-root test command.

Run the default command for all noncredentialed gates.

Use `--keep` when you need to inspect runner-owned Compose resources after a failure.

Use `--live` only when the operator supplies authorized live-provider credentials.

Run `make test-runner` when you change the runner or its documented behavior.

Do not call broad Docker cleanup commands from the runner or from an agent.

Keep `.env.live` and other credential files untracked and unchanged.
