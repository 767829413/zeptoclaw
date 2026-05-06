//! AG-UI custom event names used by ZeptoClaw.
//!
//! See docs/agui-custom-events.md (must stay aligned with client constants).

pub const FILE_ARTIFACT: &str = "ui:file_artifact";
pub const THINKING_STATUS: &str = "ui:thinking_status";
pub const TOOL_CALL: &str = "ui:tool_call";
pub const APPROVAL_REQUEST: &str = "ui:approval_request";
pub const APPROVAL_RESOLVED: &str = "ui:approval_resolved";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_use_ui_namespace() {
        for name in [
            FILE_ARTIFACT,
            THINKING_STATUS,
            TOOL_CALL,
            APPROVAL_REQUEST,
            APPROVAL_RESOLVED,
        ] {
            assert!(
                name.starts_with("ui:"),
                "AG-UI custom event must use ui:* namespace: {name}"
            );
        }
    }
}
