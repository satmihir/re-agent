use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{self, Read};

const TOP: &str = "re:agent: a small, readable agent harness\n\nusage:\n  reagent run  [flags] \"prompt\"    one task, then exit\n  reagent chat [flags]             a conversation, one turn per line\n  reagent help [run|chat]          this text, or a command's flags\n  reagent version                  the build's version\n  reagent login                    sign in with ChatGPT\n  reagent logout                   disconnect ChatGPT\n  reagent auth status              account, plan usage, expiry and models\n\nexamples:\n  reagent chat --workspace ./repo\n  reagent run --workspace ./repo \"Set the default timeout to 30s.\"\n  reagent run --workspace . --show-context \"Where is the budget?\" | jq .\n\nenvironment:\n  OPENAI_API_KEY, ANTHROPIC_API_KEY   credentials, read only for a live run\n  API_PROXY_URL, API_PROXY_PROVIDER   send OpenAI requests to this full URL instead,\n                                      with no key; the provider must be openai\n  TYPESAFE_API_KEY                    optional router key; read only when Auto is enabled\n  REAGENT_MODEL                       default model\n  REAGENT_OPENAI_AUTH                 auto, key or chatgpt (--openai-auth overrides)\n  REAGENT_OPENAI_BASE_URL             inference API base URL (default https://api.openai.com/v1)\n  REAGENT_OPENAI_AUTH_ISSUER          OAuth issuer (default https://auth.openai.com)\n  REAGENT_ANTHROPIC_BASE_URL          Messages API base URL (default https://api.anthropic.com/v1)\n  REAGENT_JEV_BASE_URL                Jev API base URL (default https://api.typesafe.ai/v1)\n  REAGENT_RESPONSE_HEADER_TIMEOUT     header timeout in whole seconds, 1–3600 (default 600)\n  REAGENT_RESPONSE_IDLE_TIMEOUT       body-idle timeout in whole seconds, 1–3600 (default 300)\n  REAGENT_WORKSPACE_GIT_TIMEOUT       workspace-snapshot Git probe timeout in whole seconds, 1–3600 (default 2)\n  REAGENT_CONTEXT_WINDOW_TOKENS       positive testing window cap; saved across resume\n  REAGENT_CONTEXT_GC_AGE_STEPS        GC experiment: 0 disables, integer > 3 enables\n  REAGENT_CONTEXT_GC_CONSOLIDATE      GC experiment: true, false, 1 or 0; default false\n                                      experiments are exclusive and require --context-gc\n                                      empty is unset; validated on resume, saved settings win\n  NO_COLOR                            turn off styling\n";

fn flags() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    vec![
        ("workspace", "DIR", "initial directory the tools may see", json!(".")),
        ("allow-workspace", "PATH", "preapprove an exact absolute workspace or prospective destination; repeatable", json!("")),
        ("provider", "NAME", "openai or anthropic; inferred from the model name when omitted", json!("")),
        ("openai-auth", "MODE", "auto, key or chatgpt; defaults to REAGENT_OPENAI_AUTH, then auto", json!("")),
        ("model", "NAME", "model to request; defaults to REAGENT_MODEL, then the provider's default", json!("")),
        ("reasoning-effort", "LEVEL", "reasoning effort to request; auto picks the provider's default, empty omits the parameter", json!("auto")),
        ("scripted", "FILE", "replay model responses from a JSON script instead of calling a provider", json!("")),
        ("show-context", "", "print the request the first step would send, then exit", json!(false)),
        ("read-only", "", "withhold write and exec tools; only allow reading", json!(false)),
        ("plan", "", "start in plan mode; model edits and commands are refused", json!(false)),
        ("auto", "", "opt in to TypeSafe routing; default fallback gpt-6-sol/medium", json!(false)),
        ("in-run-compact", "", "opt in to bounded compaction within a long run; default off", json!(false)),
        ("context-gc", "", "opt in to lossless context GC with recall; default off", json!(false)),
        ("model-retry-window", "DURATION", "keep retrying transient model failures within this window (e.g. 12h); 0 disables", json!(0)),
        ("agents", "", "offer concurrent agents with names, nesting and per-spawn read/write authority", json!(false)),
        ("no-project-instructions", "", "do not load the workspace root's AGENTS.md", json!(false)),
        ("recap", "", "show the completed run's operation recap", json!(false)),
        ("report-friction", "", "offer a trace-only harness friction reporter; at most 10 reports per process", json!(false)),
        ("git-do", "", "experimental: offer git_do, with recipes picked by jev or named by the model (recipe)", json!("")),
        ("prompt-file", "PATH", "read the prompt from this file, or - for stdin", json!("")),
        ("trace-file", "PATH", "write the trace here instead of the default cache location", json!("")),
        ("trace-dir", "DIR", "write each turn's trace under this directory", json!("")),
        ("resume", "ID", "resume a live chat by its session ID", json!("")),
        ("max-steps", "N", "maximum model requests in one run (0 means unlimited)", json!(0)),
        ("max-tool-calls", "N", "maximum accepted tool calls in one run (0 means unlimited)", json!(0)),
    ]
}

fn help(command: &str) -> String {
    let mut out = if command == "run" {
        "usage: reagent run [flags] \"prompt\"\n       reagent run [flags] --prompt-file PATH\n\nRuns one task to completion and exits. The reply goes to stdout; progress\nand tool activity go to stderr. The run summary always prints.\n--recap also shows the completed run's operation recap. Flags go before the prompt.\n".to_string()
    } else {
        "usage: reagent chat [flags]\n\nHolds a conversation, one turn per line typed. /help inside lists its commands.\n".to_string()
    };
    let mut groups = vec![
        (
            "Model",
            vec![
                "provider",
                "model",
                "reasoning-effort",
                "openai-auth",
                "auto",
            ],
        ),
        (
            "Authority",
            vec!["workspace", "allow-workspace", "read-only", "plan"],
        ),
        (
            "Input",
            if command == "run" {
                vec!["prompt-file", "no-project-instructions"]
            } else {
                vec!["no-project-instructions"]
            },
        ),
        (
            "Budgets",
            vec![
                "max-steps",
                "max-tool-calls",
                "model-retry-window",
                "in-run-compact",
                "context-gc",
                "agents",
            ],
        ),
        ("Output", vec!["recap"]),
        (
            "Tracing",
            vec![
                if command == "run" {
                    "trace-file"
                } else {
                    "trace-dir"
                },
                "report-friction",
            ],
        ),
        ("Experimental", vec!["git-do"]),
    ];
    if command == "chat" {
        groups.push(("Resume", vec!["resume"]));
    }
    groups.push((
        "Offline",
        if command == "run" {
            vec!["show-context", "scripted"]
        } else {
            vec!["scripted"]
        },
    ));
    let definitions = flags();
    let label = |name: &str| {
        let f = definitions.iter().find(|f| f.0 == name).unwrap();
        format!("--{}{}{}", name, if f.1.is_empty() { "" } else { " " }, f.1)
    };
    let width = groups
        .iter()
        .flat_map(|g| &g.1)
        .map(|n| label(n).len())
        .max()
        .unwrap();
    for (title, names) in groups {
        out.push_str(&format!("\n{title}\n"));
        for name in names {
            let f = definitions.iter().find(|f| f.0 == name).unwrap();
            let mut text = f.2.to_string();
            if let Some(default) = f.3.as_str() {
                if !default.is_empty() {
                    text.push_str(&format!(" (default {default})"));
                }
            }
            if name == "model-retry-window" {
                text.push_str(" (default 0s)");
            }
            let mut prefix = format!("  {:width$}  ", label(name));
            while !text.is_empty() {
                let limit = 80 - prefix.len();
                let cut = if text.len() <= limit {
                    text.len()
                } else {
                    text[..=limit]
                        .rfind(' ')
                        .filter(|n| *n > 0)
                        .unwrap_or(limit)
                };
                out.push_str(&prefix);
                out.push_str(&text[..cut]);
                out.push('\n');
                text = text[cut..].trim().to_string();
                prefix = " ".repeat(width + 4);
            }
        }
    }
    out
}

fn duration(raw: &str) -> Option<i64> {
    if raw == "0" {
        return Some(0);
    }
    let re = regex::Regex::new(r"([0-9]+(?:\.[0-9]*)?|\.[0-9]+)(ns|us|µs|μs|ms|s|m|h)").unwrap();
    let (negative, text) = if let Some(s) = raw.strip_prefix('-') {
        (true, s)
    } else {
        (false, raw.strip_prefix('+').unwrap_or(raw))
    };
    let mut end = 0;
    let mut ns = 0.0;
    for cap in re.captures_iter(text) {
        let m = cap.get(0)?;
        if m.start() != end {
            return None;
        }
        end = m.end();
        let scale = match &cap[2] {
            "ns" => 1.0,
            "us" | "µs" | "μs" => 1e3,
            "ms" => 1e6,
            "s" => 1e9,
            "m" => 60e9,
            "h" => 3600e9,
            _ => return None,
        };
        ns += cap[1].parse::<f64>().ok()? * scale;
    }
    if end != text.len() || end == 0 || ns > i64::MAX as f64 {
        return None;
    }
    Some((if negative { -ns } else { ns }) as i64)
}

fn parse(args: &[String]) -> Result<(Value, BTreeSet<String>, Vec<String>, bool), String> {
    let definitions = flags();
    let mut cfg = json!({"allow_workspaces":[]});
    for (name, _, _, default) in &definitions {
        cfg[name.replace('-', "_")] = default.clone();
    }
    let mut used = BTreeSet::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            return Ok((cfg, used, args[i + 1..].to_vec(), true));
        }
        if arg == "-" || !arg.starts_with('-') {
            break;
        }
        let flag = arg
            .strip_prefix("--")
            .or_else(|| arg.strip_prefix('-'))
            .unwrap();
        let (name, inline) = flag
            .split_once('=')
            .map(|(n, v)| (n, Some(v)))
            .unwrap_or((flag, None));
        if name == "h" || name == "help" {
            return Err("help".into());
        }
        if name.is_empty() || name.starts_with('-') {
            return Err(format!(
                "invalid flag name {}",
                serde_json::to_string(arg).unwrap()
            ));
        }
        let f = definitions
            .iter()
            .find(|f| f.0 == name)
            .ok_or_else(|| format!("unknown flag --{name}"))?;
        let expected = if f.3.is_boolean() {
            "a boolean (true or false)"
        } else if name == "model-retry-window" {
            "a duration (for example 30s or 12h)"
        } else if f.3.is_number() {
            "an integer"
        } else {
            "text"
        };
        let text = if let Some(v) = inline {
            v
        } else if f.3.is_boolean() {
            "true"
        } else {
            i += 1;
            args.get(i)
                .ok_or_else(|| format!("--{name} requires {expected}"))?
        };
        let value = if f.3.is_boolean() {
            match text {
                "true" | "TRUE" | "True" | "1" => Some(json!(true)),
                "false" | "FALSE" | "False" | "0" => Some(json!(false)),
                _ => None,
            }
        } else if name == "model-retry-window" {
            duration(text).map(Value::from)
        } else if f.3.is_number() {
            text.parse::<i64>().ok().map(Value::from)
        } else {
            Some(json!(text))
        }
        .ok_or_else(|| format!("--{name} expects {expected}"))?;
        if name == "allow-workspace" {
            cfg["allow_workspaces"]
                .as_array_mut()
                .unwrap()
                .push(value.clone());
        }
        cfg[name.replace('-', "_")] = value;
        used.insert(name.into());
        i += 1;
    }
    Ok((cfg, used, args[i..].to_vec(), false))
}

pub(crate) fn trim_bytes(bytes: &[u8]) -> &[u8] {
    let first = bytes.utf8_chunks().next().map_or("", |chunk| chunk.valid());
    let start = first.len() - first.trim_start().len();
    let bytes = &bytes[start..];
    let last = bytes.utf8_chunks().last();
    let trim = last
        .filter(|chunk| chunk.invalid().is_empty())
        .map_or(0, |chunk| {
            chunk.valid().len() - chunk.valid().trim_end().len()
        });
    &bytes[..bytes.len() - trim]
}

pub(crate) fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub(crate) fn yes(v: &Value, key: &str) -> bool {
    v[key].as_bool().unwrap_or(false)
}
pub(crate) fn number(v: &Value, key: &str) -> i64 {
    v[key].as_i64().unwrap_or(0)
}
fn usage(command: &str, message: &str) -> i32 {
    eprintln!("error: {message}\nreagent help {command} lists the flags");
    2
}
fn startup(message: &str) -> i32 {
    eprintln!("error: {message}");
    2
}

/// Main parses command flags and runs a task or conversation.
pub fn main(args: Vec<String>) -> i32 {
    main_input(args, None)
}

/// Main os preserves positional prompt bytes while parsing CLI syntax as text.
pub fn main_os(args: Vec<std::ffi::OsString>) -> i32 {
    use std::os::unix::ffi::OsStringExt;
    let raw: Vec<Vec<u8>> = args.into_iter().map(|arg| arg.into_vec()).collect();
    let args = raw
        .iter()
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    main_input(args, Some(raw))
}

fn main_input(args: Vec<String>, raw_args: Option<Vec<Vec<u8>>>) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        eprint!("{TOP}");
        return 2;
    };
    match command {
        "--help" | "-h" => {
            print!("{TOP}");
            return 0;
        }
        "help" => {
            if args.len() == 1 {
                print!("{TOP}");
                return 0;
            }
            if args.len() == 2 && matches!(args[1].as_str(), "run" | "chat") {
                print!("{}", help(&args[1]));
                return 0;
            }
            return usage("help", "help takes an optional command: run or chat");
        }
        "version" | "--version" => {
            println!(
                "reagent (devel) rust {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            return 0;
        }
        "run" | "chat" => {}
        "login" | "logout" | "auth" => return crate::auth::command(&args),
        _ => {
            eprintln!("error: unknown command {command}");
            eprint!("{TOP}");
            return 2;
        }
    }
    let (mut cfg, used, positional, terminated) = match parse(&args[1..]) {
        Ok(v) => v,
        Err(e) if e == "help" => {
            print!("{}", help(command));
            return 0;
        }
        Err(e) => return usage(command, &e),
    };
    if command == "run" && !terminated {
        for arg in &positional {
            if arg == "--" {
                break;
            }
            if arg.starts_with('-') && arg != "-" {
                let name = arg.trim_start_matches('-').split('=').next().unwrap();
                if matches!(name, "h" | "help") || flags().iter().any(|f| f.0 == name) {
                    return usage(command,&format!("--{name} comes after the prompt, where it would be read as prompt text; put flags before the prompt"));
                }
            }
        }
    }
    let joined = if let Some(raw) = raw_args.as_ref() {
        raw[args.len() - positional.len()..].join(&b' ')
    } else {
        positional.join(" ").into_bytes()
    };
    let mut prompt = trim_bytes(&joined).to_vec();
    let scripted = !text(&cfg, "scripted").is_empty();
    let preview = yes(&cfg, "show_context");
    let resume = !text(&cfg, "resume").is_empty();
    let bad = if command == "run" && used.contains("resume") {
        Some("--resume applies to chat only")
    } else if command == "chat" && used.contains("resume") && !resume {
        Some("--resume requires a session ID")
    } else if command == "run" && prompt.is_empty() && text(&cfg, "prompt_file").is_empty() {
        Some("a prompt is required, as an argument or with --prompt-file")
    } else if command == "run" && !prompt.is_empty() && !text(&cfg, "prompt_file").is_empty() {
        Some("give the prompt as an argument or with --prompt-file, not both")
    } else if command == "chat" && (!prompt.is_empty() || !text(&cfg, "prompt_file").is_empty()) {
        Some("chat reads its turns from stdin and takes no prompt")
    } else if command == "chat" && (preview || !text(&cfg, "trace_file").is_empty()) {
        Some("--show-context and --trace-file apply to run; chat writes one trace per turn")
    } else if command == "run" && !text(&cfg, "trace_dir").is_empty() {
        Some("--trace-dir applies to chat; run takes --trace-file")
    } else if number(&cfg, "model_retry_window") < 0 {
        Some("--model-retry-window must be nonnegative (0 disables overnight retry)")
    } else if number(&cfg, "model_retry_window") > 0 && (scripted || preview) {
        Some("--model-retry-window requires a live model request")
    } else if number(&cfg, "max_steps") < 0 || number(&cfg, "max_tool_calls") < 0 {
        Some("--max-steps and --max-tool-calls must be nonnegative (0 means unlimited)")
    } else if scripted && (!text(&cfg, "model").is_empty() || !text(&cfg, "provider").is_empty()) {
        Some("--scripted replays recorded responses, so it takes no --model or --provider")
    } else if yes(&cfg, "auto") && (scripted || preview) {
        Some("--auto cannot be combined with --scripted or --show-context")
    } else if scripted && yes(&cfg, "agents") {
        Some("--agents cannot be combined with --scripted")
    } else if scripted && preview {
        Some("--show-context previews a live request, so it cannot be combined with --scripted")
    } else {
        None
    };
    if let Some(e) = bad {
        return usage(command, e);
    }
    if let Err(e) = crate::provider::validate_environment() {
        return startup(&e);
    }
    if let Err(e) = environment(&mut cfg, resume) {
        return startup(&e);
    }
    if resume {
        if let Some(conflict) = used
            .iter()
            .filter(|n| !matches!(n.as_str(), "resume" | "recap" | "trace-dir" | "openai-auth"))
            .last()
        {
            return usage(
                command,
                &format!("--resume cannot be combined with --{conflict}"),
            );
        }
        match crate::checkpoint::load(text(&cfg, "resume")) {
            Ok(saved) => {
                let local = cfg;
                cfg = saved["config"].clone();
                for k in ["resume", "recap", "trace_dir", "openai_auth"] {
                    cfg[k] = local[k].clone();
                }
                cfg["restore"] = saved;
            }
            Err(e) => return startup(&e),
        }
    }
    if !text(&cfg, "prompt_file").is_empty() {
        let path = text(&cfg, "prompt_file");
        let raw = if path == "-" {
            let mut s = Vec::new();
            io::stdin().read_to_end(&mut s).map(|_| s)
        } else {
            std::fs::read(path)
        };
        let mut raw = match raw {
            Ok(v) => v,
            Err(e) => return startup(&format!("read prompt: {}", crate::errors::stable_io(&e))),
        };
        if raw.ends_with(b"\n") {
            raw.pop();
        }
        prompt = raw;
        if trim_bytes(&prompt).is_empty() {
            return startup(&format!("{path} contains no prompt"));
        }
    }
    let workspace = match crate::workspace::Workspace::open(text(&cfg, "workspace")) {
        Ok(w) => w,
        Err(e) => return startup(&e),
    };
    cfg["workspace"] = json!(workspace.root.to_string_lossy());
    match text(&cfg, "git_do") {
        "" | "recipe" => {}
        "jev" => {
            if std::env::var("TYPESAFE_API_KEY")
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return usage(command, "--git-do jev needs TYPESAFE_API_KEY");
            }
        }
        _ => return usage(command, "--git-do must be jev or recipe"),
    }
    let mut approved = Vec::new();
    for path in cfg["allow_workspaces"].as_array().unwrap_or(&Vec::new()) {
        match crate::tools::destination(path.as_str().unwrap_or(""), true) {
            Ok((root, _)) => approved.push(json!(root.to_string_lossy())),
            Err(bad) => {
                return usage(
                    command,
                    &format!("--allow-workspace: {}", text(&bad, "message")),
                )
            }
        }
    }
    cfg["allow_workspaces"] = json!(approved);
    if !resume {
        cfg["project_instructions"] = Value::Null;
        if !yes(&cfg, "no_project_instructions") {
            match crate::workspace::project_instructions(text(&cfg, "workspace")) {
                Ok(v) => cfg["project_instructions"] = json!(v),
                Err(e) => eprintln!("warning: AGENTS.md skipped: {e}"),
            }
        }
    }
    if yes(&cfg, "auto")
        && text(&cfg, "provider").is_empty()
        && text(&cfg, "model").is_empty()
        && std::env::var("REAGENT_MODEL")
            .unwrap_or_default()
            .is_empty()
    {
        cfg["model"] = json!("gpt-6-sol");
        if !used.contains("reasoning-effort") {
            cfg["reasoning_effort"] = json!("medium")
        }
    }
    if let Err(e) = crate::provider::configure(&mut cfg) {
        if e.starts_with("unknown provider")
            || e.starts_with("unknown model")
            || e.contains("does not match")
            || e.contains("selects the")
        {
            return usage(command, &e);
        }
        return startup(&e);
    }
    if preview {
        match crate::run_loop::preview_bytes(&cfg, &prompt) {
            Ok(body) => {
                if let Some(s) = body.as_str() {
                    println!("{s}")
                } else {
                    println!("{}", crate::types::json_string(&body))
                }
                return 0;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    if yes(&cfg, "auto") && !resume {
        let info = crate::catalog::model_info(text(&cfg, "model"));
        let effort = text(&cfg, "reasoning_effort");
        let accepted = info["efforts"].as_array().is_some_and(|efforts| {
            if efforts.is_empty() {
                effort.is_empty()
            } else {
                efforts.iter().any(|e| e == effort)
            }
        });
        if info["context_window"].as_u64().unwrap_or(0) == 0
            || info["provider"] != cfg["provider"]
            || !accepted
        {
            return usage(command, "Auto fallback needs a catalog model with a known window and supported explicit effort");
        }
    }
    cfg["_display"] = json!(true);
    if command == "chat" {
        return crate::chat::run(cfg);
    }
    let mut session = match crate::runtime::Session::new(cfg) {
        Ok(s) => s,
        Err(e) => return startup(&e),
    };
    crate::render::header(&session.config, false);
    let started = std::time::Instant::now();
    crate::render::begin_turn(&mut session.state);
    crate::cancellation::begin(&session.config);
    let guard = match crate::cancellation::install(&session.config) {
        Ok(guard) => guard,
        Err(error) => return startup(&error),
    };
    let result = session.submit_bytes(&prompt);
    drop(guard);
    crate::render::result(
        &result,
        started.elapsed(),
        &session.config,
        &session.state,
        false,
    );
    if text(&result, "status") == "completed" {
        0
    } else {
        1
    }
}

fn environment(cfg: &mut Value, resume: bool) -> Result<(), String> {
    let raw = std::env::var("REAGENT_CONTEXT_WINDOW_TOKENS").unwrap_or_default();
    if !raw.is_empty() {
        let n = decimal(&raw)
            .filter(|n| *n > 0)
            .ok_or("REAGENT_CONTEXT_WINDOW_TOKENS must be a positive integer")?;
        cfg["context_window_tokens"] = json!(n)
    }
    let raw = std::env::var("REAGENT_CONTEXT_GC_AGE_STEPS").unwrap_or_default();
    let age = if raw.is_empty() {
        0
    } else {
        decimal(&raw)
            .filter(|n| (*n == 0 || *n > 3) && *n <= i32::MAX as i64)
            .ok_or("REAGENT_CONTEXT_GC_AGE_STEPS must be 0 or an integer greater than 3")?
    };
    cfg["context_gc_age_steps"] = json!(age);
    let raw = std::env::var("REAGENT_CONTEXT_GC_CONSOLIDATE").unwrap_or_default();
    let consolidate = match raw.as_str() {
        "" | "0" | "false" => false,
        "1" | "true" => true,
        _ => return Err("REAGENT_CONTEXT_GC_CONSOLIDATE must be true, false, 1 or 0".into()),
    };
    cfg["context_gc_consolidate"] = json!(consolidate);
    if age > 0 && consolidate {
        return Err("choose only one context GC experiment".into());
    }
    if !resume && (age > 0 || consolidate) && !yes(cfg, "context_gc") {
        return Err("context GC experiments require --context-gc".into());
    }
    Ok(())
}
fn decimal(raw: &str) -> Option<i64> {
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        raw.parse().ok()
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_trimming_preserves_invalid_bytes_and_unicode_boundaries() {
        for (input, expected) in [
            (b" \t\r\n".as_slice(), b"".as_slice()),
            (b" \xe2\x80\x83hello\xe2\x80\x83 ", b"hello"),
            (b" \xe2\x80\x83\xff\xc0\x80\xe2\x80\x83 \r", b"\xff\xc0\x80"),
            (b"\xff hello \xfe", b"\xff hello \xfe"),
            (b" \xe2\x82", b"\xe2\x82"),
            (b"literal\xef\xbf\xbd ", b"literal\xef\xbf\xbd"),
        ] {
            assert_eq!(trim_bytes(input), expected);
        }
    }
    #[test]
    fn flag_boundaries() {
        let args = vec![
            "--read-only".into(),
            "--max-steps=3".into(),
            "hello".into(),
            "--plan".into(),
        ];
        let (c, _, p, _) = parse(&args).unwrap();
        assert!(yes(&c, "read_only"));
        assert_eq!(number(&c, "max_steps"), 3);
        assert_eq!(p, ["hello", "--plan"]);
        assert_eq!(duration("1h30m"), Some(5_400_000_000_000));
        assert_eq!(duration("1d"), None)
    }
    #[test]
    fn help_wraps() {
        assert!(help("run")
            .split_once("\nModel\n")
            .unwrap()
            .1
            .lines()
            .all(|s| s.len() <= 80));
        assert!(help("chat").contains("--resume ID"));
    }
}
