/// Serialized values for approval-policy selection (`AskForApproval` serializes
/// kebab-case).
pub const APPROVAL_OPTIONS: [&str; 3] = ["untrusted", "on-request", "never"];
/// Serialized values for choosing who handles eligible approval requests.
pub const APPROVAL_REVIEWER_OPTIONS: [&str; 2] = ["user", "auto_review"];
/// `(serialized value, display label)` for sandbox selection (`SandboxPolicy` uses a
/// camelCase `type` tag).
pub const SANDBOX_OPTIONS: [(&str, &str); 3] = [
    ("readOnly", "read-only"),
    ("workspaceWrite", "workspace-write"),
    ("dangerFullAccess", "full-access"),
];
/// Serialized values for reasoning effort (`ReasoningEffort` serializes lowercase).
pub const EFFORT_OPTIONS: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
