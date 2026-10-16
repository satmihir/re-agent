# Launching the Rust translation (#141)

Do not launch until #139's parity suite is approved and #140/#149 are resolved.
Freeze the Go reference and suite **after** those prerequisites land; the design
in `reagent-rust-parity.md` is not an implemented or approved test suite.

## Pin the executable and inputs

Use a clean checkout of the reviewed harness commit, not a binary rebuilt while
agents edit it. Substitute reviewed full commit IDs and absolute paths below.
Keep each attempt's artifacts and source worktree separate, outside the harness
checkout. Do not place credentials or private traces in Git.

```sh
HARNESS_COMMIT=<reviewed-full-commit>
ATTEMPT="$HOME/reagent-port-attempt-1"
SOURCE="$HOME/src/re-agent-port-1"
mkdir -p "$ATTEMPT/bin" "$ATTEMPT/traces"
git checkout --detach "$HARNESS_COMMIT"  # in a dedicated, clean build checkout
test -z "$(git status --porcelain)" || exit 1
go build -o "$ATTEMPT/bin/reagent-$HARNESS_COMMIT" ./cmd/reagent
BIN="$ATTEMPT/bin/reagent-$HARNESS_COMMIT"
"$BIN" version
shasum -a 256 "$BIN" > "$ATTEMPT/binary.sha256"
git rev-parse HEAD > "$ATTEMPT/harness-commit"
```

Create `SOURCE` as a sibling worktree at the frozen Go reference commit, on a new
attempt branch. Record that commit, the suite revision/digest, compiler versions,
model/effort, launch command and start time in `ATTEMPT`. Keep the approved suite
and protected Go binary outside `SOURCE`; a separate checkout alone is **not**
an OS write barrier for exec. Run a digest check against the reviewed manifest
before and after parity checks; a changed suite invalidates the attempt. Use the
actual approved #139 runner invocation, not a guessed parity command. Do not
refresh goldens to make the Rust port pass.

## Authenticate and launch one long task

```sh
unset API_PROXY_URL API_PROXY_PROVIDER
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
unset REAGENT_OPENAI_AUTH_ISSUER REAGENT_OPENAI_BASE_URL
unset REAGENT_CONTEXT_WINDOW_TOKENS REAGENT_CONTEXT_GC_AGE_STEPS REAGENT_CONTEXT_GC_CONSOLIDATE
"$BIN" login                       # browser approval, including plan usage
"$BIN" auth status                 # permission/model check; never print tokens
caffeinate -i "$BIN" chat \
  --workspace "$SOURCE" \
  --model gpt-6.1-sol --reasoning-effort high --openai-auth chatgpt \
  --agents --in-run-compact --context-gc --model-retry-window 12h \
  --trace-dir "$ATTEMPT/traces"
```

Use **chat with one submitted translation task**, not `run`: only live chat
persists resumable checkpoints, `--trace-dir` is chat-only, and `run` accepts only
`--trace-file`. Record the printed Session ID immediately. Defaults leave root
step/call budgets unlimited; agents retain their own bounded tasks and need
explicit follow-up work. `caffeinate -i` prevents idle sleep while its command
runs, not shutdown, power loss or every form of lid sleep. Preserve the normal
user config/cache directories: plan credentials live in config and Session
checkpoints in cache. Do not redirect HOME to a disposable directory after login.

Submit the complete #141 task as one message (or paste it with `/edit`). Include:

- Translate into `rust/`; keep Go as the reference and do not modify the suite.
- Fork writing agents by module, with disjoint file ownership and agreed public
  interfaces. Require each writer to reread before edits; forked context does not
  copy file-write authorization. Use follow-up tasks when agent budgets expire.
- Maintain `rust/PORT_STATUS.md` before handing work off and after each build,
  parity batch, compaction checkpoint or blocker. Read it after a summary and
  record summary Steps and gaps; shrinking gaps signal excessive retained detail.
- Use the parallel-build isolation decision in the #140 readiness report. Until
  measured, serialize Cargo commands; separate targets isolate build outputs,
  not simultaneous incompatible source edits.
- Run `cargo build`, `cargo test`, the unchanged approved parity suite and
  `/review` before declaring success. Record failures, uncovered scope and the
  suite digest. Do not silently retry uncertain tool effects.
- Stop on a real blocker with exact evidence. Report wall time, Steps, input,
  cached input/output, compactions, tree size and parity progress. Sum root and
  descendant `run.finished.usage` once each; do not also add `tree_cost`.

### `rust/PORT_STATUS.md` format

Create it in the attempt worktree, not in the harness implementation checkout:

```markdown
# Rust port status
Reference: <Go commit>; harness: <commit + binary SHA-256>
Suite: <revision + manifest digest>; attempt: <ID>; Session: <ID>
Updated: <UTC timestamp>; stage: <current milestone>

## Ownership
| Module / files | Agent ID / name | State | Next checkpoint |
| --- | --- | --- | --- |

## Agreed interfaces and decisions
- <public names, data shapes, file layout, error/JSON conventions; why>

## Verification
| UTC | Rust commit or dirty checkpoint | Command | Passed / total | Evidence |
| --- | --- | --- | --- | --- |

## Summary cadence
- <recent summary Steps and gaps; link older detail rather than copying it>

## Blockers and uncertain effects
- <finding, affected paths, trace/call IDs, inspection needed; or none>

## Next work
- <ordered tasks, dependencies, agent follow-ups>
```

Keep this concise and factual. A progress claim is not verification; include
actual command results and artifact paths. Record the exact current source
checkpoint when checks run against uncommitted work.

## After a crash

Keep the pinned binary, source tree, traces and checkpoint cache. Do not start a
second process on the same Session. With the same authentication environment:

```sh
unset API_PROXY_URL API_PROXY_PROVIDER
caffeinate -i "$BIN" chat --resume <SESSION_ID> \
  --openai-auth chatgpt --trace-dir "$ATTEMPT/traces"
```

Resume restores saved workspace, model, budgets and features; do not repeat
those flags (`--resume` refuses them). Resume does **not** restart a pending
model request, tool or agent. Send a new instruction: inspect the worktree and
`rust/PORT_STATUS.md`, inspect any in-flight call's effects, reconcile files with
verification evidence, then continue the remaining task. Agents are marked
`ended at resume`; inspect their possible edits before creating replacements.
Reauthorize any additional workspace. Do not replay an interrupted exec merely
because its result is missing. If plan permission expired, use `"$BIN" login`
interactively before resuming; never fall back to API-key billing unnoticed.
A lost checkpoint requires a fresh Session with the task, progress file and
explicit filesystem/effects inspection, not a claim of transparent resumption.

A terminal `limit_exceeded` Run is different from a cancelled/provider-error
checkpoint: reopening its Session restores a **blocked** conversation. Inspect
the trace and files first, then use `/reset` and resubmit the task plus progress
file as fresh context (or start a fresh Session). Reset discards model history,
not files, and is not transparent continuation. The bounded #140 pilot observed
this refusal live. Keep root budgets unlimited for #141 as above; a finite
per-turn step budget also counts every summary and can stop before the audits.
