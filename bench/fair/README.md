# FAIR Go → Rust migration

One task, three independent runs: `solo`, `agents`, `agents-ask`. Correctness is a clean Rust rebuild followed by FAIR's held-out conformance check, not the model's reply. Keep the same model, effort, re:agent binary, sandbox image (including dependency cache), timeout, and root budgets across modes. Record binary/image digests alongside the results. Rotate mode order for repeats. These are equal wall-time limits, not equal tree token budgets.

The runner does **not** create a security boundary. Provision the following stages first. Never build or execute a submission on a credentialed host, or give the model a checkout containing the grader. Use disposable unprivileged sandboxes with time, PID, memory, disk and output limits; no host home, Docker socket, SSH agent, metadata endpoint, or shared writable caches. Run Docker control commands against a dedicated sandbox worker. Only the trusted controller should be able to control Docker.

## 1. Prepare (trusted controller, offline)

```sh
R=bench/fair/run.py
python3 "$R" prepare "$FAIR" "$GRADER_COMMIT" bundle
```

`FAIR` is a trusted local FAIR repo containing source commit `d798aed0737dff1d9416fe64be84afb56a75c1f6` and the separately supplied **full commit ID** of the reviewed grader. Until that grader is committed there is intentionally no default. The bundle contains a `git archive` of the source (no working-tree changes or `.git`), only `conformance/PROTOCOL.md` from the grader, the fixed task prompt, and controller-side pins. Keep pins and evaluation artifacts outside `/testbed`.

## 2. Attempt (optional, paid)

For **each** mode, use a fresh running named container on the sandbox worker with an empty writable `/testbed`, Go, Rust, and the same `reagent` on PATH. Allow only a rate-, time- and spend-limited inference broker; it holds the provider credential outside the container. Don't forward host API keys. Name the container in `$BOX` and select `$MODE` and a new `$OUT` directory:

```sh
docker exec -i "$BOX" tar -xf - -C /testbed < bundle/source.tar
docker exec "$BOX" mkdir /testbed/conformance
docker cp bundle/PROTOCOL.md "$BOX:/testbed/conformance/PROTOCOL.md"
docker cp bundle/prompt.md "$BOX:/tmp/prompt.md"
python3 "$R" attempt bundle "$BOX" "$OUT" --mode "$MODE" --model "$MODEL" --effort "$EFFORT"
```

The controller clocks `docker exec` and stops the container, even on timeout. Retain `attempt.json`, reply, progress, and `/tmp/events.jsonl` from the stopped container as `$OUT/events.jsonl`. Treat replies, traces and submission files as untrusted/private. Transfer bounded regular-file bytes only; unpack any untrusted filesystem archives **inside** disposable sandboxes, never on the controller. Keep each workspace snapshot and its trace with the corresponding attempt; don't mount `$OUT` writable by the model. A missing or partial trace does not change the conformance verdict.

## 3. Rebuild and score (offline)

Copy the submitted sources, not a prebuilt binary or target directory, to a **fresh build sandbox**, with no credentials, grader, broker, or network. Starting with a fresh Cargo target directory, run:

```sh
CARGO_TARGET_DIR=/tmp/build cargo build --release --locked --offline --manifest-path rust/Cargo.toml --bin fair-conformance
```

Transfer only `/tmp/build/release/fair-conformance` into a separate minimal runtime sandbox matching the build ABI. Neither build scripts nor the binary may access the grader. Pin dependency availability before the experiment. Distinguish infrastructure failures (e.g. broken sandbox or toolchain) from a submission that fails its build.

In a **separately protected pristine** FAIR checkout at `$GRADER_COMMIT`, the trusted evaluator runs `python3 conformance/check.py /absolute/path/to/candidate-binary`, exit 0/1. Supply a trusted executable launcher as that path: it must run **only the rebuilt binary inside the runtime sandbox**, forwarding stdin/stdout, arguments and exit status. For example, for a dedicated networkless runtime container named `fair-runtime`, a controller-owned shell launcher can `exec docker exec -i fair-runtime /opt/fair-conformance "$@"`. Do not pass a raw submission executable or submission-provided launcher to the controller. Apply an external wall-clock limit to evaluation; tear down runtime processes on exit, timeout or cancellation.

```sh
python3 "$R" score "$PROTECTED_FAIR" "$OUT" --launcher /trusted/path/to/launcher > "$OUT/score.json"
# If the clean build failed instead:
python3 "$R" score "$PROTECTED_FAIR" "$OUT" --build-failed > "$OUT/score.json"
```

`score` returns 0 for pass and 1 for a recorded failure. Treat an absent JSON result as an infrastructure error, not a verdict. Compare verdicts, elapsed agent seconds (excludes build and grading), and trace `input_tokens`, `cached_input_tokens`, and `output_tokens` **including failed attempts**. Cached input and reasoning are subsets, not extra tokens to add. `usage_source`, `usage_known` and `unreported_attempts` distinguish whole-tree measurements from incomplete or unknown cost. Sandbox-produced traces are telemetry, not tamper-proof billing records.

Offline smoke test: `python3 bench/fair/run_test.py` (no Docker, provider, or FAIR checkout required).
