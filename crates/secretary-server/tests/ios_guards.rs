//! Guards on the iOS app's source that a Swift unit test can't check.

/// The approval Live Activity has exactly one intent button, Deny. On iOS 27 the Live Activity renderer resolved every
/// tap on a card with two intent buttons to one of them (the owner's Deny taps arrived as Approve), so Approve is a
/// link to the app's card, where Face ID is asked.
#[test]
fn the_approval_card_has_one_intent_button_deny() {
    let src = include_str!("../../../ios/Extensions/SecretaryWidgets/ApprovalLiveActivity.swift");
    let buttons: Vec<&str> = src.lines().filter(|l| l.contains("Button(intent:")).collect();
    assert_eq!(buttons.len(), 1, "one intent button on the approval card: {buttons:#?}");
    assert!(buttons[0].contains("DenyApprovalActivityIntent"), "the card's intent button is Deny: {}", buttons[0]);
    assert!(!src.contains("ApproveApprovalActivityIntent"), "Approve is a link, never an intent on the card");
}
