# re:agent JSONL traces (v0 §7)

Source: `internal/reagent/trace.go` and the event writers in `loop.go`, `transport.go`, `session.go`, `run_compact.go`, `model_switch.go`, `auto.go`, `child.go`, and `tool_git_do.go`; payload types are in `types.go`, `tool_exec.go`, and `tool_git_do.go`. These are **private records**: prompts, file contents, tool arguments and API bodies may appear. Do not publish a raw trace or credentials.

## Location and envelope

By default each run writes `<user cache>/reagent/runs/<run_id>/events.jsonl` (`os.UserCacheDir`: macOS `~/Library/Caches`, Linux `$XDG_CACHE_HOME` or `~/.cache`, Windows `%LocalAppData%`). `run --trace-file PATH` uses that exact file instead; `chat --trace-dir DIR` uses `DIR/<run_id>/events.jsonl` for each turn, compaction or model switch. Child runs have their **own** default-cache trace even if the parent used an override. Files are exclusive (not overwritten), directories mode 0700 and files 0600; recording is best effort. A scripted CLI run, a test using a scripted model, or a run in a temporary workspace can also write to the default cache. Do not assume every cached trace is a live session or belongs to this checkout.

Each JSON line has `schema_version` (envelope version, currently 1), `seq` (per-file order, starting at 1), `session_id`, `run_id`, `time` (UTC RFC3339Nano), `type`, `step` (model step, 0 before one), and `data` (event-specific JSON). Use `seq`, not timestamps, for order. An event's `data` can be a complete request/response or a small map; it is **not** wrapped in another `data` field except inside a tool outcome. Optional fields are omitted; some slices may be `null`.

## Event data

The table lists emitted keys; `?` means conditional or omitted. Dotted paths indicate nesting, and `[]` indicates an array element. Model requests and responses below are normalized harness objects, not raw HTTP bodies.

| Type | `data` fields |
| --- | --- |
| `run.started` | `model`, `provider`, `configured`, `build`, `workspace`, `approved_workspaces[]`, `max_steps`, `max_tool_calls`, `in_run_compact`, `prompt`, `instructions`, `tools[]` (`name`, `description`, `input_schema`, `effect`), `initial_history[]` (accepted entries; see below). |
| `model.requested` | `scope` (`session_id`, `run_id`, `step`), `model`, `reasoning_effort?`, `instructions`, `history[]`, `tools[]` (same tool spec as above). |
| `model.accepted` | `response_id?`, `model?`, `blocks[]` (`kind`: `text`, `refusal`, or `tool_call`; `text?`; `call?` with `call_id`, `name`, `arguments`), `native` (`provider?`, `items?[]` provider JSON), `usage` (`known`, `input_tokens`, `cached_input_tokens`, `output_tokens`, `reasoning_tokens`, `unreported_attempts?`; the accepted response's usage excludes failed retries). `call.arguments` is a **JSON string**, not an object: use `fromjson?` when inspecting it. |
| `model.failed` | `error`; `response?` (same response shape as `model.accepted` if a response failed validation; absent on model/transport error). |
| `api.attempt.started` | `attempt` (1-based), `endpoint` (URL with password redacted), `request_body` (exact prepared bytes as a string), `request_sha256`. |
| `api.attempt.finished` | `attempt`, `http_status`, `duration_ms`; on a received body: `request_id`, `response_body` (UTF-8 replacement if needed), `body_utf8_replaced`; on failure: `error` instead of those three fields. Retries produce separate attempts. |
| `api.retry.scheduled` | `attempt` (just failed), `reason`, `delay_ms` (next wait). Only with an opt-in retry window. |
| `api.retry.exhausted` | `attempt`, `http_status`; the logical request's retry window elapsed. |
| `tool.started` | `call_id`, `name`, `arguments` (same JSON string), `workspace`. Only emitted when a real implementation starts. |
| `tool.finished` | `call_id`, `name`, `outcome` (`workspace?`, `ok`, `code`, `message`, `data`, `truncated`, `effect`). Emitted also for denied/unavailable/not-executed calls without `tool.started`. `outcome.data` is tool-specific JSON (or `null`), not a string. |
| `run.finished` | `status`, `reason?`, `reply?`, `steps`, `tool_calls`, `usage` (same keys as response usage, includes failed retries; a lower bound if `unreported_attempts` is nonzero), `router_usage?`, `child_usage?`, `child_steps?`, `child_calls?`, `child_attempts?`, `children?`, `trace_path?`, `effects?[]` (`workspace?`, `step`, `call_id`, `tool`, `summary`, `effect`), `resumable?` (present only when true). Child/router usage is separate from `usage`. |
| `compaction.requested` | Same shape as `model.requested`; temporary summary prompt is in `history[]`. |
| `compaction.finished` | `summary`, `replaced_entries`. |
| `compaction.failed` | `reason`. |
| `compaction.skipped` | `reason` (within-run compaction only). |
| `model.switch.requested` | `source_model`, `source_provider`, `source_effort`, `destination_model`, `destination_provider`, `destination_effort`, `fresh`. |
| `model.switch.finished` | Above plus `request_bytes`, `latency_ms`; if a handoff was needed: `handoff_entries`, `handoff_bytes`, `omitted_native_items`, `handoff_sha256`. |
| `model.switch.failed` | Requested fields plus `request_bytes`, `latency_ms`, `reason`. |
| `auto.route` | Routing boundary: `boundary_reason`, `source_model`, `source_effort`, `rule`, `threshold`, `action`; optionally `reason`, `candidates[]`, `excluded[]`, `selected_route`, `omitted_earlier_entries`, `omitted_native_items`, `omitted_earlier_user_requests`, `previews`, `jev_model`, `router_usage`, `router_http_status`, `router_latency_ms`, `packet_bytes`, `packet_sha256`, `confidence`, `probabilities`, `ladder`, `cumulative`, `generation_request_bytes`, `handoff_entries`, `handoff_native_items`. For `action: post_switch_usage` instead: `switch_step`, `model`, `effort`, `usage_known`, `input_tokens`, `cached_input_tokens`. |

`history[]` and `initial_history[]` entries have `kind` and one matching member: `user` (`text`, `workspace?`, `plan?`), `assistant` (response shape above), `tool` (tool.finished shape), `shell` (`workspace?`, `kind`, `command`, `exit_code`, `signal`, `interrupted`, `output`, `output_bytes_seen`, `output_truncated`, `encoding_replaced`, `duration_ms`), or `summary` (`text`, `replaced_entries`, `model`). A child run's `run.started` is its own snapshot workspace and task; the parent only records the child tool call and bounded outcome, not the child's history or HTTP bodies.

For `exec`, `tool.finished.data.outcome.data` is one command's result when no `then` was given: `argv[]`, `resolved_executable`, `cwd`, `exit_code`, `signal`, `stdout`, `stderr`, `stdout_bytes_seen`, `stderr_bytes_seen`, `stdout_truncated`, `stderr_truncated`, `output_may_be_incomplete`, `encoding_replaced`, `duration_ms`, `timeout_ms`, `termination_reason`. With `then`, the same location contains `steps[]` of these command results in execution order, and `not_run?[][]` for later commands skipped after a failure. Inspect `outcome.effect` for applied/unknown effects, not just the exit code.

For `git_do`, `tool.finished.data.outcome.data` has `recipe`, `chosen_by`, `confidence?`, `status` (`done`, `declined`, `failed`, `unverified`, `timeout`), `reason?`, `output?`, `steps[]` (`argv[]`, `exit_code`, `output?`), `checks?[]` (`check`, `ok`, `evidence`), `jev_calls?`, `jev_tokens?`, `jev_ms?`, and `decision` (`offered[]`, `probabilities?` (nested maps), `chosen_by?`, `threshold?`, `named_recipe?`, `named_agreed?`, `jev_outcome?`, `declined_by?`, `snapshot` (`branch`, `default_branch`, `upstream?`, `ahead_of_upstream`, `behind_default`, `ahead_of_default`, `changed[]`, `changed_count?`, `recent_commits[]`, `pull_request?`, `pull_request_comments?`, `finished_pull_request?`, `finished_pull_request_state?`)). `outcome.code` is the tool envelope status (`ok`, `declined`, `step_failed`, `unverified`, `timeout`), not the git_do result's `status`. Older traces before #105 have no `decision`; early runs also lack `workspace` on run/tool records. Treat missing fields as missing, not as evidence they were empty.

## Recipes

Set `run=~/Library/Caches/reagent/runs/<run_id>/events.jsonl` (or a trace path from `run.finished.data.trace_path`). These commands display only selected fields; inspect output before sharing, since even an intent can contain private content.

List every `git_do` call and its status in one trace, including unfinished or refused calls (repeat over `*/events.jsonl` for multiple runs):

```sh
jq -r -s '. as $events | ($events | map(select(.type=="tool.finished" and .data.name=="git_do") | {key:.data.call_id,value:(.data.outcome.data.status // .data.outcome.code)}) | from_entries) as $status | $events[] | select(.type=="model.accepted") | .run_id as $run | .data.blocks[] | select(.call.name=="git_do") | [$run, (.call.arguments | fromjson? | .intent // .recipe // ""), ($status[.call.call_id] // "unfinished")] | @tsv' "$run"
```

Show failed tool calls with their codes for one run (including calls that never started):

```sh
jq -r 'select(.type=="tool.finished" and (.data.outcome.ok | not)) | [.data.call_id, .data.name, .data.outcome.code] | @tsv' "$run"
```

Sum per-run model usage from `run.finished` (do **not** sum `model.accepted` as well; router and child totals are separate, and `known:false` means the sum is incomplete):

```sh
jq -r 'select(.type=="run.finished") | [.run_id, .data.usage.known, .data.usage.input_tokens, .data.usage.output_tokens, (.data.usage.input_tokens + .data.usage.output_tokens)] | @tsv' "$run"
```

Python worked example (includes unfinished calls and handles missing historical `decision`):

```sh
python3 bench/git/harvest_git_do.py "$run"
```
