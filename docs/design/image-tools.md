# OpenAI image generation and viewing

Status: implemented with offline boundary tests. API-key generation/editing and
OpenAI multimodal input have not been live-verified; ChatGPT-first inference
remains separate from image billing.

## Goal

Support a generate → inspect → revise workflow when using OpenAI models. Image
viewing is part of that workflow, not the whole feature. The conversational model
calls explicit tools; a separate image-generation model produces the artifact.

The initial implementation supports local image viewing, generation, and editing
or composition from explicit reference images. It does not add image tools to
Anthropic Sessions, terminal image rendering, remote image URL fetching, masks,
batches, animation, or an attachment framework.

## Authentication decisions and remaining gates

The user approved this policy and the `image` crate (PNG, JPEG, WebP):

- Prefer a usable ChatGPT login for OpenAI inference in automatic auth mode,
  including child Sessions, summaries, BTW, model switches, and Auto destinations.
  Explicit auth choices and configured proxies retain their existing behavior.
  Anthropic and TypeSafe retain their own credentials; ChatGPT login does not
  authenticate either service.
- Use `OPENAI_API_KEY` only for the separate Images API generation/editing call
  when ordinary inference is using ChatGPT. Do not change Session inference auth
  or resend a failed plan request with the API key. Image generation has separate
  API billing, which the tool description must make clear.
- A missing image key must not prevent startup, inference, or viewing. Keep the
  generation tool discoverable when otherwise permitted. At execution, return
  `credentials_required`, `effect: none`, before HTTP or file publication, asking
  the user to create an OpenAI API key and set `OPENAI_API_KEY` in the environment
  that launches re:agent, then restart/resume. Never ask them to paste it in chat.
  The model should relay this politely and continue without repeated attempts.
- Do not send ChatGPT OAuth tokens to image endpoints, fall back to another
  credential after a generation failure, or derive image URLs from a Responses
  proxy URL. Provider selection alone is not a capability check.

A live probe on 2026-11-13 made one request to
`https://api.openai.com/v1/images/generations`, using the saved ChatGPT OAuth token
and `gpt-image-2.5-flare`. It returned HTTP 401,
`hardened_oauth_rule_missing`: "This ChatPass credential is not authorized for the
requested operation." No retries, API-key fallback, or artifact publication
occurred. This establishes rejection of that credential/operation, not a blanket
claim about all ChatGPT integrations.

The implementation uses the documented public Images API contract with
`gpt-image-2.5-flare`, JSON generation, multipart editing, and PNG output. The
approved dependency is `image` 0.25.10 with PNG, JPEG, and WebP enabled. Local HTTP
fixtures verify those shapes and structured image continuation; they do not
establish live model/account access. Codex source is evidence of its own contract,
not proof that it applies here.

## Tool contracts

### `view_image`

```json
{"path": "assets/reference.png"}
```

`path` is required and workspace-relative. Read and validate a regular image
file, prepare a bounded preview, and return its pixels to the model with the
original path, SHA-256, format, dimensions, and byte length. Report preview
resizing explicitly. Viewing does not modify the workspace file.

A preview is not evidence that the model inspected every original pixel or
metadata field. Do not extend implicit text-edit authorization based on viewing.
An explicit original-file digest can still be used under existing deletion
rules; recall does not grant fresh write authority.

### `generate_image`

```json
{
  "prompt": "Create a product illustration using this reference",
  "output_path": "assets/product.png",
  "reference_image_paths": ["assets/reference.png"]
}
```

`prompt` and `output_path` are required. `reference_image_paths` is optional;
when present it contains one to four workspace-relative paths. Reject unknown
fields and empty prompts.

Without references, generate a new image. With references, request an edit or
composition. References are snapshotted and validated before submission; the
request must use those bytes rather than reopening mutable files later.

Produce one PNG artifact per call. Require an existing destination directory and
create a new file only. Do not overwrite files, follow destination symlinks, or
infer references from "the last image." The output format and backend defaults
must be verified before implementation; do not silently relabel another format
as PNG.

Return the saved artifact's path, digest, dimensions, format, and byte length,
plus a model-visible preview in the same ToolResult. A successful call therefore
supports inspection on the next model request without another `view_image` call.
The model may then revise the result by naming the saved artifact as a reference
and choosing a new output path.

Keep size, quality, and image-model selection harness-owned initially. Do not
expose speculative backend options in the tool schema.

## Availability and authority

| Context | View | Generate/edit |
| --- | --- | --- |
| Supported OpenAI model, normal mode | Yes | With a supported generation route |
| OpenAI, read-only or plan mode | Yes | No |
| OpenAI read-authority child | Yes | No |
| OpenAI write-authority child | Yes | Subject to inherited route and authority |
| Anthropic | No | No |
| Tool-free BTW request | No | No |

Use explicit capabilities for image input and generation-route support. Include
these in the tool-spec cache key, enforce them again at dispatch, and update the
child allowlist and its authority description. Recompute availability after a
model switch and for child configuration; do not inherit an OpenAI capability
blindly into an Anthropic child.

Generation is a paid remote operation and a workspace write. It must be classified
as a write even if no local artifact is ultimately published. Historical images
may be inherited by a read-only child without granting new filesystem authority.

## Backend and HTTP behavior

Use a separate generation request path through the existing low-level HTTP
client. The model adapter encodes model requests; it does not execute tools.

- API key: the only initial generation/editing route. Read `OPENAI_API_KEY` at
  tool execution, not as a requirement for an ordinary ChatGPT-backed Session.
  Use the verified Images API contract and the trusted OpenAI API base; the image
  request authenticates with the key, not the Session's OAuth headers.
- ChatGPT: remains the preferred ordinary OpenAI inference route. Its OAuth token
  is never submitted for generation/editing in this implementation.
- Proxy: inference proxy behavior is unchanged. `API_PROXY_URL` is a complete
  Responses endpoint, not an image destination. Image calls use the explicit
  API-key route independently; do not append to or replace the proxy path.

Do not reuse the inference retry loop or its streaming assumptions. Submit each
ToolCall once. Never automatically repeat generation after a timeout, connection
loss, authentication response, cancellation, or local publication failure.
Transport recovery before any request bytes are written is distinct from
resubmitting a possibly accepted request and must remain provably safe.

Retain cancellation, deadlines, endpoint validation, redirect refusal, and
credential redaction. Bound response bytes while reading, before JSON parsing or
base64 decoding. Validate response cardinality, image format, dimensions, and
encoded size before publication. Do not fetch provider-returned URLs in the
initial implementation; select a backend contract returning image bytes.

Record the image model and provider request ID when available. Track image usage
separately from conversational token usage; report cost as unknown when reliable
usage or pricing is unavailable.

## Original artifact and model preview

Original bytes and model-visible pixels have different purposes:

- The generated artifact retains the full-quality provider result.
- A bounded preview is prepared for conversational image input.
- Viewing an existing image never rewrites its source file.
- Reference preparation must be distinct from preview preparation. Do not
  silently send a low-resolution conversational preview to the editing backend.

Proposed initial local limits, subject to validation against the selected backend:

| Resource | Limit/policy |
| --- | --- |
| Input formats | PNG, JPEG, WebP; reject animation |
| Compressed input | 20 MiB per image |
| Decoded image | 40 megapixels; 128 MiB decoded/decoder allocation limits |
| References | At most four, plus an aggregate request-byte limit |
| Preview dimensions | At most 2,048 pixels on the longest side |
| Preview bytes | 512 KiB PNG, iteratively resized if needed |
| Output | One validated PNG, with bounded response and decoded sizes |

Check contents rather than extensions. Apply orientation consistently and strip
unneeded metadata from previews; preserve original artifacts. Reject malformed,
unsupported, or oversized images before remote submission where possible. Limit
both dimensions and allocation: compressed size alone does not prevent a decoder
from exhausting memory.

At most eight previews are projected per request, with an 8,192-token allowance
per image and the existing 10 MiB full wire cap. Image requests do not train the
text-byte token ratio. Account for base64 expansion before allocating a full
request. Decoder allocation limits are best-effort library limits, not a hard
process-memory ceiling.

## Accepted outcomes and image encoding

Keep the existing ToolOutcome envelope. Store compact image metadata in
`outcome.data`: original provenance and SHA-256, MIME type, dimensions, byte
length, and a separate immutable preview reference with its own hash and sizes.
Do not put base64 or expiring URLs in accepted history.

The OpenAI encoder must translate recognized image results into structured
function outputs containing an `input_text` metadata item and an `input_image`
preview item. Ordinary text tools retain their existing string output encoding.
Verify this function-output shape for each supported route before enabling it.
Base64 inside an ordinary JSON string is not vision input.

Resolve only validated image-tool results and explicit trusted projections,
including authorized recall wrappers. Never recursively scan arbitrary tool
output or user prose for hashes to turn into image access. A hash identifies
bytes; it does not itself authorize access to them.

## Immutable storage and persistence

Persist image previews by content hash before accepting their ToolResult. Use a
private, content-addressed store shared by Sessions so admitted references remain
usable across child lifetime and model handoffs. Checkpoint metadata refers to
these blobs; it does not duplicate their bytes.

An accepted image observation must not depend on a mutable workspace file.
Changing or deleting the source after a view must not change historical pixels.
Never reconstruct a missing observation by rereading the current path, and never
regenerate an image automatically to repair history.

A missing or corrupt blob should not prevent checkpoint restore. Project an
explicit "pixels unavailable" marker and show a diagnostic identifying the
reference; do not send substituted bytes or imply that the model saw the image.
Viewing a current workspace file again is a new observation. Editing requires
readable, validated explicit reference files before a provider request.

Cache-backed persistence is not a portable, self-contained checkpoint. Backups
or transfers must include referenced blobs. Use the existing platform cache
location policy and private permissions. Avoid automatic blob eviction in the
first version; document disk growth, retention, and the effect of manual cache
removal. A general blob garbage collector or export system is outside this scope.

## Context lifecycle

### GC and recall

Add image observations to context GC. Dropping a preview from active context does
not delete its accepted metadata or stored bytes. Retain generation effect
metadata even when pixels are dropped. Authorized `recall` restores the preview
from the original outcome nested inside the recall result; it does not grant
access to unrelated blob hashes or fresh write authority.

### Model switching and children

OpenAI-to-OpenAI handoffs carry admitted image references structurally alongside
historical text. Apply the same rule to selected Entries and cross-model forks;
stringifying metadata alone would lose visual context. Same-model forks preserve
admitted native history and its references.

Anthropic destinations and BTW captures receive metadata only, explicitly labeled
as lacking pixels. Preserve reference provenance in trusted history where it is
carried so later OpenAI projection can use admitted references again. Do not
infer image access by parsing the handoff's prose.

Scope child references to the Entries actually admitted into that child's
context, following existing recall inheritance. Neither a child nor the model
gets a general blob-store lookup tool. Child replies can report artifact paths
and effects; the parent can explicitly view a resulting workspace file.

Auto routing must not imply that a text-only routing packet contains visual
evidence. Keep its packet metadata-only and apply destination capability and
handoff rules before admitting the next request.

### Compaction

For the first version, follow existing history-loss semantics: compaction removes
visual observations and their recallability. Saved workspace artifacts remain
available for a new `view_image`. State this explicitly; a prose summary is not a
replacement for pixels or a guaranteed exact image manifest.

Both manual and in-run compaction must follow this rule. A compaction request that
itself receives images still obeys image capability and request-budget checks.
Deterministic manifests and reactivation after compaction are deferred rather
than introducing another retrieval mechanism in the first implementation.

## Wire and context budgets

Retain the existing 10 MiB complete serialized OpenAI request limit. Measure exact
wire bytes, including base64 expansion, separately from context admission.

Estimate text tokens from text bytes and add a conservative, model-specific image
allowance based on preview dimensions and detail. Do not train the text
`tokens_per_byte` ratio on mixed image requests unless image usage can be
separated reliably. An unsupported image-token model needs a conservative
validated allowance, not an assumption that base64 behaves like text.

Use the same accounting for ordinary requests, model switches, child admission,
GC savings, and compaction. Bound individual previews and their aggregate before
constructing the complete body. If context or wire limits are exceeded, retire
eligible previews through explicit GC or return an actionable error. Never
silently omit visual evidence from an otherwise successful projection.

## Publication, cancellation, and uncertain effects

Validate all local inputs, authority, destination, and budgets before submitting.
Require an existing destination directory initially. Revalidate workspace/root
identity and destination safety before publication under the existing publication
lock. Use create-only atomic publication; reject an existing destination even if
it appeared while generation was running. Do not hold the publication lock across
the remote request.

| Situation | Outcome |
| --- | --- |
| Local validation failure or cancellation before submission | No generation attempted |
| Explicit provider rejection | Failed; no artifact; record known provider response |
| Timeout, disconnect, or cancellation after possible submission | Remote effect unknown; no automatic retry |
| Generation completed but publication failed | Failed tool result with applied remote generation and no saved artifact |
| Successful publication | Applied effect with artifact path and digest |

Distinguish remote completion from local publication in outcome data. A save
failure is not a harmless no-effect failure and must not invite automatic
regeneration. Do not promise recovery of returned bytes unless they were actually
persisted. Unknown remote effects use the existing stop/uncertainty behavior;
this design does not add automatic remote job reconciliation.

Persist enough execution state to preserve uncertainty if the process exits
between submission, receipt, publication, and accepted-result checkpointing.
Resume must not replay the call. Include generation paths and operations in effect
records and child effect reporting, without treating binary images as text diffs.

Existing path validation is not a claim of a race-free filesystem sandbox. Review
symlink ancestors and publication races explicitly rather than assuming the
ordinary text-file helper solves every binary-file hazard.

## Display and traces

Initially show activity, saved paths, dimensions, resizing notices, and failures
in the terminal. Do not add terminal graphics protocols or launch an external
viewer automatically. "Viewing" here means providing pixels to the model.

Keep image payloads out of tool traces, model request/response trace bodies,
checkpoint metadata, summaries, and activity rendering. Request tracing needs an
image-aware projection; omitting base64 only from ToolOutcome is insufficient.
Record hashes, sizes, formats, request IDs, and usage where available. Preserve
existing credential redaction and identify any omitted wire content explicitly
in trace documentation.

## Implementation ownership

Before coding, confirm these boundaries and narrow the concrete edit list to the
chosen backend. This is a cross-cutting multimodal change, not just two handlers.

| Files | Responsibility |
| --- | --- |
| `rust/src/tool_images.rs` (new) | Tool validation and orchestration |
| `rust/src/images.rs` (new) | Decode, validate, prepare previews, store and resolve immutable blobs |
| `rust/src/image_generation.rs` (new) | Verified generation/editing HTTP contract and response validation |
| `rust/src/lib.rs`, `rust/Cargo.toml`, `rust/Cargo.lock` | Module registration and approved image dependency |
| `rust/src/tool_specs.json`, `rust/src/schemas.rs`, image schemas | Tool descriptions and argument validation |
| `rust/src/tools.rs`, `rust/src/catalog.rs`, `rust/src/provider.rs` | Capability gates, cache keys, dispatch, authority, and backend selection |
| `rust/src/openai.rs` | Structured image output encoding and wire-size checks |
| `rust/src/workspace.rs`, `rust/src/run_loop.rs` | Binary publication, accepted outcomes, effects, execution uncertainty, and token calibration |
| `rust/src/context.rs`, `rust/src/context_gc.rs`, `rust/src/compact.rs` | Budgeting, preview retirement/recall, and explicit compaction semantics |
| `rust/src/checkpoint.rs` | Reference validation and persistence compatibility |
| `rust/src/cli/model_switch.rs`, `rust/src/agents/context.rs`, `rust/src/agents/specs.json` | Scoped visual handoff and accurate child authority contracts |
| `rust/src/session_api/btw_capture.rs`, `rust/src/routing_packet.rs`, `rust/src/auto.rs` | Explicit metadata-only consumers and routing admission |
| `rust/src/transport.rs`, relevant trace/rendering code | No-retry HTTP integration, image-aware trace omission, and activity summaries |
| `docs/reagent-overview.md`, `docs/reagent-trace-format.md` | Implemented behavior and trace contract, updated when implementation lands |
| Boundary tests and `bench/tty/check.py` | End-to-end continuation and real-terminal coverage |

Do not add a new crate, vendor SDK, general attachment framework, or speculative
backend abstraction. Reuse the existing HTTP and publication primitives where
their semantics fit; do not broaden text-file limits to accommodate images.

The overview describes the implemented tools and inference defaults. Terminal
coverage exercises viewing, real structured continuation, and missing image-key
guidance; API-key generation/editing use offline HTTP fixtures, not paid calls.

## Verification plan

All ordinary tests run offline with local HTTP listeners and temporary files.
Use observable synchronization rather than sleeps.

- Availability and direct dispatch: OpenAI/Anthropic, model switch, read-only,
  plan, child authority, and tool-free requests.
- Tool arguments: missing/unknown fields, empty prompts, reference counts,
  withheld paths, directories, symlinks, destination collisions, and unsupported
  or misleading file extensions.
- Decoding: malformed files, animation, decompression/allocation limits,
  orientation, transparency, preview resizing, and aggregate byte bounds.
- Complete workflow: generate → atomically save → next model request includes
  pixels → edit from that saved artifact → view the edited output.
- Persistence: resume after source mutation/deletion, missing/corrupt blobs,
  checkpoint round trips, and interruption at each remote/publication boundary.
- Context: GC → recall, compaction → continuation, same-model fork, cross-model
  fork, selected-Entry isolation, model/provider switches, Auto, and BTW.
- Budgets: base64 wire expansion, aggregate overflow, separate text/image token
  estimates, and no contamination of text calibration.
- Failures: provider rejection, malformed or oversized response, delayed response,
  cancellation, post-submission disconnect, and publication failure. Assert one
  submission and truthful effect reporting, including on resume.
- Privacy: no image payloads or credentials in traces, checkpoint metadata,
  activity, text handoffs, or errors.
- Platforms: Linux and macOS cache paths, symlinked temporary roots,
  canonical/literal paths, case-insensitive destination collisions, publication
  behavior, and accepted-socket blocking mode.

Run `make check` and `make tty-check` after implementation, adding real pty
scenarios for the new activity and failure display. Live provider generation and
editing checks are explicit opt-in verification for each supported authentication
route; offline fixtures do not establish live backend compatibility.

## Source basis

The proposal was prepared against the Astra context-window correction branch.
Relevant pre-implementation source anchors (line numbers move as implementation lands):

- `rust/src/tools.rs:136–179`: tool-spec cache has no image capability key;
  `219–244`: child allowlist and read-only filtering.
- `rust/src/provider.rs:85–112,163–217`: shared ChatGPT-first inference auth
  selection, proxy endpoint validation, and credential availability checks.
- `rust/src/openai.rs:101–112`: tool outcomes currently become JSON strings;
  `148–155`: serialized request-size enforcement.
- `rust/src/run_loop.rs:795–884`: outcome envelope, effect records, trace/history
  publication, and checkpointing; `588–612`: unknown-effect stop behavior.
- `rust/src/transport.rs:240–268`: inference streaming/retry setup;
  `301`: request-body tracing that must not retain image payloads.
- `rust/src/workspace.rs:308–328`: byte-oriented create-only/replace publication.
- `rust/src/context_gc.rs:612–648`: recall wraps the original outcome in data.
- `rust/src/compact.rs:108–128`: compaction replaces history and resets GC.
- `rust/src/cli/model_switch.rs:149–186` and
  `rust/src/agents/context.rs:50–108`: text handoffs versus native history copies.

These were implementation observations, not evidence of live compatibility.
The image implementation now accompanies this document, with offline validation.
The only live image probe remains the OAuth rejection recorded above; no successful
paid generation/editing or live multimodal continuation is claimed.
