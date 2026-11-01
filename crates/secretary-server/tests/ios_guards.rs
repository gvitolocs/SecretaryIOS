//! Guards on the iOS app's source that a Swift unit test can't check.

/// The approval Live Activity has exactly two intent buttons, Deny then Approve (Deny first in the source: the left
/// one), and Approve is never a link. Every tap is logged by intent first thing in the app, so a renderer that routes a
/// tap to the wrong intent shows in the log. Details, if present, is a link below the button row.
#[test]
fn the_approval_card_has_two_intent_buttons_deny_then_approve() {
    let src = include_str!("../../../ios/Extensions/SecretaryWidgets/ApprovalLiveActivity.swift");
    let buttons: Vec<&str> = src
        .lines()
        .filter(|l| l.contains("Button(intent:"))
        .collect();
    assert_eq!(
        buttons.len(),
        2,
        "two intent buttons on the approval card: {buttons:#?}"
    );
    assert!(
        buttons[0].contains("DenyApprovalActivityIntent("),
        "the left button is Deny: {}",
        buttons[0]
    );
    assert!(
        buttons[1].contains("ApproveApprovalActivityIntent("),
        "the right button is Approve: {}",
        buttons[1]
    );

    // The button row: from the first intent button to the end of its HStack. No link in it.
    let start = src.find("Button(intent:").unwrap();
    let row = &src[start..];
    let row = &row[..row.find(".font(").expect("the row's modifiers follow it")];
    assert!(!row.contains("Link("), "no link in the button row:\n{row}");
    for line in src.lines().filter(|l| l.contains("Link(")) {
        assert!(
            !line.contains("approve"),
            "Approve is an intent, never a link: {line}"
        );
    }
}

/// Each card intent logs its tap (intent and request id) before anything else.
#[test]
fn card_taps_are_logged_first() {
    let src = include_str!("../../../ios/Secretary/Shared/ApprovalActivity.swift");
    for intent in ["Deny", "Approve"] {
        let decl = format!("struct {intent}ApprovalActivityIntent");
        let body = &src[src.find(&decl).unwrap_or_else(|| panic!("{decl} missing"))..];
        let perform = &body[body.find("func perform()").expect("perform")..];
        let first = perform.lines().nth(1).unwrap_or_default();
        assert!(
            first.contains(&format!("card tap intent={intent} id=")),
            "{intent}'s perform logs the tap first: {first}"
        );
    }
}
