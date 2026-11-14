# Web search

Status: implemented in re:agent on `feat/web-search`; Covalent not yet. Written
2026-11-14 against `daecde22`.

As built, differing from the plan below: the preview reports `searching` but not
the query (OpenAI sends the query only after the search finishes); adjacent
Anthropic text blocks are joined into one Block with their citations, because
citations split a sentence into several blocks; and the CLI prints sources to
stderr so stdout stays the reply alone.

## Goal

Let the model search the web during a Run, on OpenAI and Anthropic models, in
the CLI and through the Session API, so a host such as Covalent can answer
questions about current events with sources.

Not in this design: fetching arbitrary URLs as a separate tool, domain allow
and block lists, location hints, a search backend of re:agent's own, inline
citation positions, or search for agents and BTW questions.

## Approach: the providers' hosted search

Both providers offer a search tool that runs on their side, inside one model
request. re:agent declares it; the provider searches and returns the queries,
the results, and the reply in the same response.

| | OpenAI Responses | Anthropic Messages |
|---|---|---|
| Declaration | `{"type":"web_search"}` | `{"type":"web_search_20250305","name":"web_search","max_uses":N}` |
| Search in the response | `web_search_call` output items with an `action` | `server_tool_use` and `web_search_tool_result` blocks |
| Citations | `url_citation` annotations on `output_text` | `citations` on `text` blocks |
| Continuing the conversation | send the output items back | send the blocks back unchanged, including `encrypted_content` |
| Extra stop state | none | `stop_reason: "pause_turn"` |
| Count for billing | number of `web_search_call` items | `usage.server_tool_use.web_search_requests` |

This is not a ToolCall. re:agent executes nothing, records no ToolOutcome, and
the adapter still runs no tools: the rule against executing a tool inside the
model adapter is untouched. A search is part of a Step's response.

The alternative, a harness-executed `web_search` tool calling a search service,
would give one behavior on every model but needs a third credential and a
search vendor, and loses the providers' own ranking and citations.

Anthropic's basic tool version is used on purpose. The newer versions run
searches from inside the provider's code execution by default, which adds block
types and a second billing surface for no benefit here.

## Probe results (2026-11-14)

One live request per provider with the declaration added and nothing else
changed. Both searched; both Runs then stopped with `protocol_error` because
the adapters reject the new item types, as expected.

**OpenAI, `gpt-6.1-sol` on a ChatGPT plan** (no API key needed):

- Stream events `response.web_search_call.in_progress`, `.searching`,
  `.completed`, and `response.output_text.annotation.added`.
- `response.completed` carried no `output`; the items come from
  `response.output_item.done`, which `openai::assemble` already uses.
- Items: `{"type":"web_search_call","status":"completed","action":{"type":"search","query":...,"queries":[...]}}`,
  then a `message` whose `output_text` has `annotations` of
  `{"type":"url_citation","start_index","end_index","url","title"}`.
- The reply text also inlines a markdown link to the source, so a host showing
  a sources list repeats it. Cited URLs carry `?utm_source=openai`.
- About 10,800 input tokens for one search, most of it search content counted
  as input. No count of searches in `usage`; count the `web_search_call` items.

**Anthropic, `claude-haiku-5-5` with an API key:**

- Blocks in order: `thinking`, `server_tool_use` (`input` arrives as
  `input_json_delta`), `web_search_tool_result` (all results, with
  `encrypted_content`, in `content_block_start`, plus `"caller":{"type":"direct"}`),
  `thinking`, then `text` blocks, one starting with `"citations":[]` and filled
  by a `citations_delta` of type `web_search_result_location`.
- `usage.server_tool_use` reports `web_search_requests` and `web_fetch_requests`.
- `stop_reason: "end_turn"`; no `pause_turn` for one search.

## What already works

Both adapters keep each response's native items byte for byte and send them
back on the next request, so search items and encrypted results continue
correctly without new storage. Checkpoints already save native items.

## What must change

1. **Request.** A Session setting `web_search` adds the provider's declaration
   to the request's tools, next to re:agent's own tools or alone in a tool-free
   Session. Only for the root conversation: summaries, BTW questions and agents
   do not get it. Only for models the catalog marks as supporting it.
2. **Accepting the response.** Both adapters reject item and block types they
   do not know. They learn the search types above, and the Anthropic stream
   assembler learns `server_tool_use`, `web_search_tool_result` and
   `citations_delta`.
3. **A new Block kind, `web_search`,** carrying the query and the result titles
   and URLs, in the position the search happened. Text Blocks gain an optional
   list of cited sources (URL and title). The checkpoint Block schema gains
   those fields; older checkpoints are unchanged.
4. **`pause_turn`.** Anthropic may stop a long search turn early. The loop
   accepts that response and sends the next request with no new input, as
   another Step of the same Run, at most five times in a row.
5. **A search error is not a Run error.** Anthropic returns search failures
   inside a 200 response; the Block records the error code and the model
   carries on.
6. **Limits.** A fixed cap of searches per request on both providers. Searches
   do not count toward `--max-tool-calls`, because they are not ToolCalls.
7. **Reporting.** A Run's result lists its sources (deduplicated URL and
   title, in first-cited order) and its search count. Traces already record
   request and response bodies.
8. **Other readers of Blocks.** Model switch with Carry, compaction input, and
   `read_entries` render a `web_search` Block as a labelled line (query and
   sources). Provider-encrypted results are dropped on a provider change, as
   native items already are.
9. **Reply preview.** The preview reports that a search is running and its
   query, so a host can show it. Reply text keeps streaming as now.

## Turning it on

| Surface | Setting | Default |
|---|---|---|
| Session API | `SessionOptions.web_search: bool`, saved in the checkpoint | off |
| CLI | `--web-search` on `run` and `chat` | off |

Off by default because search is a way out of the machine. A Session that can
read files can put what it read into a search query, and a page it reads can
tell it to. In read-only and plan modes it would be the only such channel, so
the flag is an explicit choice. A tool-free Session has no files to leak, only
its own conversation.

## CLI display

A status line while searching ("searching the web: QUERY"), and a short
"Sources" list after a reply that cited any. Covered by a new `bench/tty`
scenario.

## Covalent

Covalent turns `web_search` on for new chats. While a reply is pending it shows
"Searching the web…" with the query, and under a finished reply it lists the
sources as links, which both providers require when search results are shown to
users. Its prompt stops saying it cannot browse. Chats created earlier keep
their saved setting. Details go in Covalent's own docs when that work starts.

## Verification

1. **Live probes** (done; see above). Offline fixtures are written by hand
   from those shapes, without real encrypted content.
2. Offline tests against fake providers: request encoding per provider and
   mode; normalizing recorded search responses, including an Anthropic search
   error and a split-across-blocks cited reply; stream assembly; `pause_turn`
   continuation and its cap; checkpoint round trip; resume and a second turn
   that sends search items back; model switch with Carry.
3. `make check`, `make tty-check`, and one live Run per provider.

## Order of work

1. Live probes and recorded fixtures.
2. Adapters and the new Block (accept, normalize, assemble, encode).
3. Loop: `pause_turn`, limits, result sources.
4. Session API option, checkpoint, preview, `read_entries`.
5. CLI flag and display.
6. Covalent.

Steps 1 to 5 are one re:agent pull request, or two if it reads better split
after step 3.

## Open points

1. **Default in the CLI.** Off, as designed, or on for chat when not read-only.
2. **Agents.** Root only, as designed, or children inherit the setting.
3. **Inline citations.** A sources list first, as designed. Positions inside
   the reply differ per provider and can follow.
4. **Search cap.** A fixed number per request (8 is the working value) or a
   flag.
