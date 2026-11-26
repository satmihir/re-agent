---
name: add-model
description: Add a model to re:agent's catalog, or change what a listed model accepts (reasoning efforts, default effort, context window, image viewing, Fast). Use when asked to add, offer, enable, re-verify or retire a model.
---

# Add or change a catalog model

A model is one row in `rust/src/catalog.rs` and a few facts kept beside it. The
edits are small once the facts are known, so collect every fact before editing.
`AGENTS.md` still applies in full; this only orders the work.

This covers a model from a provider re:agent already speaks to. The provider is
read from the id's prefix in `provider::infer_provider` (`claude-`, `gemini-`,
otherwise OpenAI). A new prefix means a new provider, which is a different and
much larger change: stop and say so.

## 1. Collect the facts

| Fact | Where it comes from |
|---|---|
| id, context window in tokens | The provider's model page. |
| family and version | The name people say: `gpt-6.2-sol` is Sol 6.2. |
| efforts accepted | `make probe-model MODEL=<id>`. It sends 8 small paid requests, so say so and get a yes first. |
| default effort | The level the provider documents for a request that sets none. Use it as the row's effort, so the model behaves as it does elsewhere, unless the user asks for another. |
| verbosity | OpenAI only. Copy the newest listed model of the same family; empty sends nothing. |
| image viewing, Fast | Only what the provider documents or a live request has shown. |
| note | A few words for the picker, in the style of the rows around it. |

Whatever was not verified stays off, and the pull request says which facts those
are. Do not turn a capability on because a sibling model has it.

## 2. Edit

- **A new model** goes at the end of `models()`. A row's position is its
  `/model N` number, which people type, so no existing row moves.
- **Image viewing and Fast** are the name lists in `supports_images` and
  `supports_fast`. Add the id only where verified.
- **Leave the defaults alone** unless asked: the startup models in
  `provider::configure` and Auto's routes in `auto.rs`.
- **Retiring a model** renumbers every row after it. Ask before removing a row.

## 3. Tests

The two full checks take about three and five minutes. Run each once, at the
end, and use the quick commands below while editing.

- **A new model** needs the id list in
  `model_positions_and_model_specific_efforts_are_fixed`, its facts asserted
  beside the other rows in that module, and a line in
  `fast_support_is_explicit_and_provider_specific`. Check them as you go with
  `cd rust && cargo +1.99.1 test --locked --lib catalog::tests`, which takes
  seconds. A new row should break no test outside that module; the one
  `make check` at the end confirms it.
- **Changed facts of a listed model** also break tests that use it as an example
  of those facts. Expect `rust/tests/cli_flags.rs`,
  `rust/tests/session_api_controls.rs`, `rust/tests/session_api_model_switch.rs`,
  `rust/tests/provider_auth_selection.rs` and the tests in `rust/src/jev.rs`.
  Only for this case, list every failure in one run, since `make check` stops at
  the first failing file:

  ```bash
  cd rust && cargo +1.99.1 test --locked --no-fail-fast 2>&1 | grep -E "FAILED|^test result"
  ```

  Move those tests to the new facts. If one guards a promise in
  `docs/reagent-session-api.md` that the change would break, such as what a
  saved checkpoint resumes as, stop and ask.
- **The terminal picker** lists every row, so add one `screen_has` line for the
  new row to the `model-picker` scenario in `bench/tty/check.py`. A model does
  not need a scenario of its own. Check it with `make build` and then
  `python3 bench/tty/check.py ./reagent model-picker`. The picker's digit keys
  reach only 9; a scenario picks a later row with `/model N` or by name.

## 4. Documents

Add to the Providers paragraph of `docs/reagent-overview.md` what a reader could
not guess: the picker number, window, efforts, default effort, and what is off.
Change `docs/reagent-session-api.md` only if a promise in it changed.

## 5. Finish

Run `make check`, then `make tty-check`, once each, and paste the first's output
and the second's verdict. On macOS put `caffeinate -i` in front of both: an idle
laptop that sleeps stops them until someone comes back. If one terminal
scenario fails, rerun it by name (`python3 bench/tty/check.py ./reagent NAME`)
before treating it as real or repeating the full run.

The pull request says what
was verified live and how many requests that took, what came from documentation
only, and what was not checked (Linux, the ChatGPT plan login, the other
providers).

Hosts pin re:agent by commit, so they see the model only after their pin moves.
Covalent's is the `rev` in its `Cargo.toml`: after the merge, point it at the
merge commit on `main`, never at a branch commit, and run its tests.
