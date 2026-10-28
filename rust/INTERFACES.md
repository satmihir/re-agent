# Initial Rust module contracts
All writers read this plus PORT_STATUS.md and their relevant Go source first. Own only assigned files. Request interface changes via root. Cross-module data use serde_json::Value with exact Go JSON tags; do not silently rename shapes. Value objects preserve insertion order (serde_json preserve_order); manually sort Go map keys where needed. Model-facing serialized JSON must mimic Go escaping (types::json_string).

## Shared runtime (root owns runtime.rs)
```
pub struct Session {
 pub config: Value,
 pub history: Vec<Value>,
 pub tools: crate::tools::ToolState,
 pub provider: crate::provider::Provider,
 pub trace: crate::trace::Trace,
 pub state: Value,
}
```
Config is a JSON object using snake_case names from Go Config: provider, auth_mode, model, reasoning_effort, workspace, read_only, plan, max_steps, max_tool_calls, model_retry_window (nanoseconds), in_run_compact, context_window_tokens, context_gc, context_gc_age_steps, context_gc_consolidate, agents, no_project_instructions, project_instructions (string or null), report_friction, proxied, git_do, scripted (file path), trace_file, trace_dir, session_id. Additional real fields may be added with coordination. `state` holds mutable session internals using snake_case keys. Public APIs take &Value where practical.

## types / context / trace / catalog agent
- types::ModelError { pub status: String, pub message: String, pub usage: Value }; implement Display/Error. types::usage_zero(known:bool)->Value, usage_add(&mut Value,&Value), new_id()->String, json_string(&Value)->String (Go json.Marshal escaping), canonical_json(&Value)->String (no HTML escaping).
- context::build_context(config:&Value,scope:Value,history:&[Value],tools:Value)->Value: Go ModelRequest exact shape. context::instructions(config:&Value)->String. Copy instruction text constants into Rust-owned files under src/prompts/ if useful (authorized as your ownership); no runtime reference to Go tree.
- catalog::model_info(model:&str)->Value (provider, context_window, effort info etc); catalog::models()->Value.
- trace::Trace::new(path:Option<&str>,session_id:&str,run_id:&str)->Result<Self,String>; Trace::disabled()->Self; Trace::write(&mut self,event:&str,step:usize,data:Value). Trace envelope exact Go trace.go. Expose path if useful.

## providers agent
- provider::Provider::new(config:&Value)->Result<Self,String>; provider::Provider::generate(&mut self,request:&Value)->Result<Value,types::ModelError>; provider::Provider::name(&self)->String.
- provider::validate_environment()->Result<(),String>; provider::configure(config:&mut Value)->Result<(),String> resolves provider/model/credentials and flags to match CLI. It must never contact real providers in tests.
- provider::encode_request(config:&Value,request:&Value)->Result<Value,String> for preview/admission.
- Modules provider.rs, openai.rs, anthropic.rs, transport.rs, auth.rs, jev.rs; you may add provider-specific files under src/provider/. Scripted belongs here.

## files/tools/workspace agent
- workspace::Workspace::open(path:&str)->Result<Self,String>; pub root:PathBuf; observations ledger internal. workspace::snapshot(config:&Value)->Value matches workspace_state record; workspace::project_instructions(root:&str)->Result<Option<String>,String>.
- tools::ToolState { pub workspace:Workspace, ... }; ToolState::new(config:&Value)->Result<Self,String>; tools::specs(config:&Value)->Value sorted ToolSpecs, include agents specs via agents::specs and git_do etc. tools::execute(state:&mut ToolState,config:&Value,name:&str,arguments:&str)->Result<Value,String> routes file tools, exec/Git. Session-specific agent tools routed in run_loop before tools::execute.
- tools::outcome(ok:bool,code:&str,message:&str,data:Value,effect:&str)->Value and tools::fail(code:&str,message:&str)->Value. Failure workspace handling matches Go.
- ToolState may expose mutable auxiliary Value as `pub state:Value` for command module state. Coordinate additions. files owns tools.rs/workspace.rs/tool_files.rs, plus tools specs data file.

## commands agent
- tool_exec::execute(workspace:&mut workspace::Workspace,config:&Value,args:&Value)->Result<Value,String>.
- tool_git::inspect(workspace:&mut Workspace,config:&Value,args:&Value)->Result<Value,String>.
- tool_git::git_do(workspace:&mut Workspace,config:&Value,args:&Value)->Result<Value,String>.
- tool_exec::spec()->Value; tool_git::inspect_spec()->Value; tool_git::git_do_spec(config:&Value)->Value.
- Decode argument string and common validation happens in tools; validate tool-specific keys/types here. Own tool_exec.rs/tool_git.rs and additional src/git/ files if warranted.

## loop agent
- Implement Session::new(config:Value)->Result<Self,String> and Session::submit(&mut self,text:&str)->Value in run_loop.rs. Return Go RunResult JSON. Session struct in runtime.rs root-owned.
- Build scopes and requests via context, call provider, dispatch tools. stdio rendering belongs CLI, not loop. Store terminal output in result reply; tools outcomes and trace exact Go.
- pub fn preview(config:&Value,prompt:&str)->Result<Value,String> builds exact show-context output; coordinate CLI.
- Session-specific tool handling `agents::execute(self,name,arguments)->Option<Result<Value,String>>` before generic tools.
- checkpoint::save(self,phase:&str,reason:&str)->Result<(),String> hook.
- Keep state keys discoverable in code; coordinate CLI access. compact.rs/context_gc.rs yours.

## CLI agent
- cli::main(args:Vec<String>)->i32; main.rs root calls it with std::env::args().skip(1).
- Parse flags into config Value, provider::configure and environment validation, workspace instructions, scripted, --show-context using run_loop::preview, Session::new/submit. Run stdout/stderr exact Go, inspect Go formatting.
- chat::run(config:Value)->i32; checkpoint::load(id:&str)->Result<Value,String> gives {config,history,state} initially (agent coordinate richer schema).
- Own cli.rs/chat.rs/terminal.rs/render.rs and embedded help text files under src/cli/ if useful.

## agents/checkpoints agent
- agents::specs()->Vec<Value>; agents::execute(session:&mut Session,name:&str,args:&str)->Option<Result<Value,String>>. Implement child sessions with shared explicit state, no provider execution from inside provider itself. You may add src/agents/ files.
- checkpoint::save(session:&Session,phase:&str,reason:&str)->Result<(),String>; checkpoint::load(id:&str)->Result<Value,String>. Preserve exact Go v2 envelope/state schema; returned Value has config/history/state for CLI restore as well as raw checkpoint if needed.
- No checkpoint for scripted. CLI and loop will need explicit coordination, ask root as needed.

## Work rules
Do not edit Go, parity, suite checkout, cache, root progress file, manifest or lib.rs. Root handles commits. Before changing existing files reread them (fork does not authorize writes). Cargo build/test permitted only with CARGO_TARGET_DIR=target/agents/YOURNAME; initial root fetch/lock first, wait for root notice. No parity runs by agents. User target is unchanged suite 4130 pass / 10 pending / 0 fail. Never use Go delegation or fixture-output shortcuts; translate behavior.

## Opaque JSON contract (root decision after first parity batches)
- `raw.rs` root-owned provides `raw::new(&[u8])->Value`, `raw::bytes(&Value)->Option<Vec<u8>>`, `raw::parse(&Value)->Result<Value,String>`, `raw::compact(&[u8],escape_html:bool)->Vec<u8>`.
- RawMessage fields may carry an internal singleton object `{ "__reagent_raw_json_v1": "<base64>" }`. This is NEVER an external wire/schema field; the types encoder recognizes it and writes compacted original JSON bytes, not the wrapper. Base64 is needed because Go opaque native JSON can contain invalid UTF-8.
- Providers should retain native.items as these opaque wrappers directly from their raw response token spans. `raw::parse` supplies semantic access when needed. OpenAI input continuation carries wrappers unchanged. Anthropic encoders inspect with raw::parse and use wrappers for unmodified content. Request bodies use `types::json_bytes(&Value)->Vec<u8>` once types agent implements it. Trace calls json_string and repairs invalid-byte runs only in that trace copy.
- Checkpoint persistence turns wrappers into base64 of raw::bytes, and load creates wrappers from base64 instead of discarding original lexical bytes. Ordinary unwrapped Values continue to work for locally constructed workspace/outcome data. No global raw sidecar or process-global output matching.
- Types owns serializer changes; providers owns extraction/HTTP; agents owns checkpoint conversion. Root owns raw.rs and integration.
