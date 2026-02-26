use agent_client_protocol::{PermissionOption, PermissionOptionId, PermissionOptionKind};

use super::*;

#[test]
fn select_allow_option_prefers_allow_once() {
    let options = vec![
        PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
        PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
    ];

    let selected = select_permission_option(&options, true).expect("should pick allow");
    assert_eq!(selected.0.as_ref(), "allow");
}

#[test]
fn select_reject_option_prefers_reject_once() {
    let options = vec![
        PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
    ];

    let selected = select_permission_option(&options, false).expect("should pick reject");
    assert_eq!(selected.0.as_ref(), "reject");
}

#[test]
fn fallback_to_none_when_no_kind_matches() {
    let options = vec![PermissionOption::new(
        PermissionOptionId::new("custom"),
        "custom",
        PermissionOptionKind::AllowAlways,
    )];

    let selected = select_permission_option(&options, false);
    assert!(selected.is_none());
}
