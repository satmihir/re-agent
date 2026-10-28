use serde_json::Value;

/// Session owns a conversation and its runtime bindings.
pub struct Session {
    pub config: Value,
    pub history: Vec<Value>,
    pub tools: crate::tools::ToolState,
    pub provider: crate::provider::Provider,
    pub trace: crate::trace::Trace,
    pub region: Option<crate::terminal::RegionHandle>,
    pub state: Value,
}
