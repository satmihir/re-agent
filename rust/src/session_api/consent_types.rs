use super::RunRef;
use serde::{Deserialize, Serialize};

/// WorkspaceApprovalRef identifies one bounded, one-use approval in an exact Run and opening.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceApprovalRef {
    /// approval_id is opaque and unique to this live request.
    pub approval_id: String,
    /// run binds consent to its exact Run and Session opening.
    pub run: RunRef,
    /// deadline_at_ms is the request deadline in Unix milliseconds.
    pub deadline_at_ms: u64,
}

/// WorkspaceConsentRequest contains trusted-local human content, never model configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConsentRequest {
    /// approval identifies the exact one-use request.
    pub approval: WorkspaceApprovalRef,
    /// path retains exact validated canonical or prospective destination bytes.
    pub path: Vec<u8>,
    /// missing marks a reservation for this exact destination, not permission to create it.
    pub missing: bool,
    /// plan is the current planning restriction at the tool boundary.
    pub plan: bool,
    /// read_only is the effective launch restriction at the tool boundary.
    pub read_only: bool,
    /// created_at_ms is the request creation time in Unix milliseconds.
    pub created_at_ms: u64,
    /// timeout_ms is the human owner's bounded response duration.
    pub timeout_ms: u64,
}

/// WorkspaceDecision records a human answer, not proof that filesystem revalidation or a grant succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceDecision {
    /// Allow permits the waiting tool to revalidate this exact destination.
    Allow,
    /// Deny refuses this request without changing workspace grants.
    Deny,
}

/// WorkspaceConsentState describes bounded request discovery without disclosing its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceConsentState {
    /// Pending is waiting for its registered human owner.
    Pending,
    /// Allowed means the human answer was registered, not that access was granted.
    Allowed,
    /// Denied means the human refused this request.
    Denied,
    /// TimedOut means no usable answer arrived within the bounded response duration.
    TimedOut,
    /// Cancelled means its exact Run was cancelled or ended.
    Cancelled,
    /// Unavailable means its human owner or opening became unavailable.
    Unavailable,
}
