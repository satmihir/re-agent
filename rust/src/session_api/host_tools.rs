use super::*;
use crate::tools::{fail, outcome};

/// HostToolHandler answers host tool calls, on the Session's worker thread while its Run waits.
pub type HostToolHandler = dyn Fn(&HostToolCall) -> HostToolOutcome + Send + Sync;

impl SessionApi {
    /// Set_host_tools makes `handler` answer this opening's host tool calls, replacing any earlier
    /// handler. Until it is set, and after this opening ends, a host tool call fails at once.
    pub fn set_host_tools(
        &self,
        handler: impl Fn(&HostToolCall) -> HostToolOutcome + Send + Sync + 'static,
    ) {
        self.client.shared.state.lock().unwrap().host_tools = Some(Arc::new(handler));
    }
}

/// check refuses host tools a model could not be offered or would confuse with re:agent's own.
pub(super) fn check(tools: &[HostTool], tool_free: bool) -> Result<(), ApiError> {
    if tool_free && !tools.is_empty() {
        return Err(error(
            "invalid_argument",
            "host tools need a Session with tools, not a tool-free one",
        ));
    }
    for (index, tool) in tools.iter().enumerate() {
        let name = &tool.name;
        let valid = !name.is_empty()
            && name.len() <= 64
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid {
            return Err(error(
                "invalid_argument",
                format!("host tool name {name:?} must be 1 to 64 lowercase ASCII letters, digits or underscores, starting with a letter"),
            ));
        }
        if crate::tools::builtin(name) || tools[..index].iter().any(|t| &t.name == name) {
            return Err(error(
                "invalid_argument",
                format!("host tool name {name} is already a tool's name"),
            ));
        }
        if tool.description.trim().is_empty() || !tool.input_schema.is_object() {
            return Err(error(
                "invalid_argument",
                format!("host tool {name} needs a description and an object input_schema"),
            ));
        }
    }
    Ok(())
}

/// bind installs the API-owned adapter before the Session reaches its exclusive worker.
pub(super) fn bind(session: &mut Session, shared: &Arc<Shared>) {
    let shared = Arc::downgrade(shared);
    session.tools.host_tool = Some(Box::new(move |name, arguments, config| {
        // The handler is cloned out so the State lock is not held while the host works.
        let handler = shared
            .upgrade()
            .and_then(|shared| shared.state.lock().unwrap().host_tools.clone());
        let Some(handler) = handler else {
            return fail(
                "host_unavailable",
                &format!("{name} is answered by the application that opened this Session, which is not connected"),
            );
        };
        let call = HostToolCall {
            name: name.into(),
            arguments,
            cancelled: crate::cancellation::token(config),
        };
        let answer = handler(&call);
        if !answer.ok {
            return outcome(
                false,
                "host_tool_failed",
                &answer.message,
                answer.data,
                "none",
            );
        }
        let read_only = config["host_tools"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|tool| tool["name"] == name && tool["read_only"] == true);
        let effect = if read_only { "none" } else { "applied" };
        outcome(true, "", "", answer.data, effect)
    }));
}
