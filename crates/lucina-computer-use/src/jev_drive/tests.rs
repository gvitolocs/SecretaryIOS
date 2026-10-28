use super::*;

// Trimmed from a real get_window_state of Lucina's Chromium (2026-10-28).
const DESKTOP: &str = r#"- [0] frame "Log ind – Google Konti - Chromium" [actions=[doDefault,showContextMenu]]
  - [1] panel "￼￼" [actions=[doDefault,showContextMenu]]
    - [2] panel "￼￼￼￼￼" [actions=[doDefault,showContextMenu]]
      - [3] push button "Minimize" [actions=[press,showContextMenu]]
      - [5] push button "Close" [actions=[press,showContextMenu]]
      - [6] panel "￼￼￼" [actions=[doDefault,showContextMenu]]
          - [8] tool bar "￼￼￼" [actions=[doDefault,showContextMenu]]
            - [9] push button "Back" (description "Press to go back") [actions=[press,showContextMenu]]
            - push button = "Forward" (disabled)
            - [13] entry "Address and search bar" [actions=[activate,showContextMenu]]
        - [18] panel "￼￼" [actions=[doDefault,showContextMenu]]
            - [21] document web "Log ind – Google Konti" [actions=[doDefault,showContextMenu]]
              - [26] heading "Log ind" [actions=[clickAncestor,showContextMenu]]
                - static = "Log ind"
              - [33] entry "Mailadresse eller telefonnummer" [actions=[activate,showContextMenu]]
              - [38] push button "Har du glemt mailadressen?" [actions=[press,showContextMenu]]
                - static = "Har du glemt mailadressen?"
              - [44] push button "Næste" [actions=[press,showContextMenu]]
              - [50] push button "Opret konto" [actions=[open,showContextMenu]]
              - [66] link "Privatlivspolitik (eksternt)" [actions=[jump,showContextMenu]]
              - [67] push button "Send" [actions=[press,showContextMenu]]
              - [68] push button "Accept all" [actions=[press,showContextMenu]]
              - [69] section "￼" [actions=[clickAncestor,showContextMenu]]
        - [72] panel "￼￼￼￼" [actions=[doDefault,showContextMenu]]
            - [76] page tab list "￼￼" [actions=[doDefault,showContextMenu]]
              - [77] page tab "Mail - Giuseppe Vitolo - Outlook" [actions=[doDefault,showContextMenu]]
                - [78] push button "Close" [actions=[press,showContextMenu]]
          - [80] push button "New Tab" (description "New tab") [actions=[press,showContextMenu]]
"#;

#[test]
fn desktop_offers_page_elements_and_tabs_but_not_chrome_or_risky_buttons() {
    let s = parse_desktop(DESKTOP, "s00000007", "Log ind – Google Konti - Chromium");
    let labels: Vec<&str> = s.elements.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Back",
            "Mailadresse eller telefonnummer",
            "Har du glemt mailadressen?",
            "Næste",
            "Opret konto",
            "Privatlivspolitik (eksternt)",
            "Mail - Giuseppe Vitolo - Outlook",
        ]
    );
    let entry = &s.elements[1];
    assert_eq!(entry.key, "e33");
    assert!(entry.editable);
    assert_eq!(entry.handle, "s00000007:33");
    assert!(!s.elements.iter().any(|e| e.label == "Close" || e.label == "Minimize"));
    assert!(s.text.contains("# Page: Log ind – Google Konti"));
    assert!(s.text.contains("# Log ind"));
    assert!(s.text.contains("Har du glemt mailadressen?"));
}

#[test]
fn risky_labels_are_matched_as_words() {
    for l in ["Send", "Invia ora", "Køb nu", "Accept all", "Sign out", "Elimina", "Reply all"] {
        assert!(risky(l), "{l}");
    }
    for l in ["Posta in arrivo", "Sender: Anna", "Acceptance criteria doc", "Inbox", "Næste"] {
        assert!(!risky(l), "{l}");
    }
}

const ANDROID: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.android.settings" content-desc="" clickable="false" enabled="true" password="false" bounds="[0,0][720,1280]"><node index="0" text="" resource-id="com.android.settings:id/search_action_bar" class="android.widget.LinearLayout" package="com.android.settings" content-desc="" clickable="true" enabled="true" password="false" bounds="[32,96][688,192]"><node index="0" text="Search settings" resource-id="" class="android.widget.TextView" package="com.android.settings" content-desc="" clickable="false" enabled="true" password="false" bounds="[100,120][400,170]" /></node><node index="1" text="About emulated device" resource-id="android:id/title" class="android.widget.TextView" package="com.android.settings" content-desc="" clickable="true" enabled="true" password="false" bounds="[0,1100][720,1180]" /><node index="2" text="" resource-id="com.x:id/query" class="android.widget.EditText" package="com.android.settings" content-desc="Search &amp; find" clickable="true" enabled="true" password="false" bounds="[0,200][720,260]" /><node index="3" text="Factory reset" resource-id="" class="android.widget.Button" package="com.android.settings" content-desc="" clickable="true" enabled="true" password="false" bounds="[0,300][720,360]" /></node></hierarchy>"#;

#[test]
fn android_labels_containers_by_their_texts_and_drops_risky_ones() {
    let s = parse_android(ANDROID);
    assert_eq!(s.title, "com.android.settings");
    let got: Vec<(&str, &str, bool)> = s
        .elements
        .iter()
        .map(|e| (e.label.as_str(), e.handle.as_str(), e.editable))
        .collect();
    assert_eq!(
        got,
        [
            ("Search settings", "360 144", false),
            ("About emulated device", "360 1140", false),
            ("Search & find", "360 230", true),
        ]
    );
    assert!(s.text.contains("About emulated device"));
}

#[test]
fn android_text_is_quoted_for_the_device_shell() {
    assert_eq!(android_text("caffè Netto"), "'caffè%sNetto'");
    assert_eq!(android_text("it's; rm -rf /"), r"'it'\''s;%srm%s-rf%s/'");
}

#[test]
fn step_questions_fit_jev_limits_and_only_offer_typing_with_a_field() {
    let s = parse_desktop(DESKTOP, "s1", "t");
    let qs = step_questions(Where::Desktop, &s, &[]);
    let names: Vec<&str> = qs.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["operation", "click_target", "done", "stuck"]);
    let Question::Choice { options, .. } = &qs[0].1 else {
        panic!()
    };
    assert!(options.iter().any(|(k, _)| k == "type"));
    let no_fields = Screen {
        elements: s.elements.iter().filter(|e| !e.editable).cloned().collect(),
        ..s.clone()
    };
    let ops = operations(Where::Desktop, &no_fields);
    assert!(!ops.iter().any(|(k, _)| *k == "type"));
    let android = operations(Where::Android, &no_fields);
    assert!(android.iter().any(|(k, _)| *k == "open_app"));
    assert!(!android.iter().any(|(k, _)| *k == "open_url"));
}

#[test]
fn dumps_travel_gzipped() {
    use base64::Engine as _;
    use std::io::Write as _;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(ANDROID.as_bytes()).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap());
    assert_eq!(gunzip_base64(&b64), ANDROID);
    assert_eq!(gunzip_base64("not base64!"), "");
}

#[test]
fn account_switching_is_never_offered_and_accounts_are_named() {
    for l in ["Switch account", "Skift konto", "Cambia account", "Use another account"] {
        assert!(risky(l), "{l}");
    }
    let s = Screen {
        title: "Takeout".into(),
        text: "Signed in as gvitolofoto0@gmail.com".into(),
        elements: vec![Element {
            key: "e1".into(),
            role: "push button".into(),
            label: "Google-konto: Giuseppe (givi@au.dk)".into(),
            editable: false,
            handle: "s1:1".into(),
        }],
    };
    assert_eq!(accounts(&s), ["givi@au.dk", "gvitolofoto0@gmail.com"]);
}

#[test]
fn only_sandbox_hiccups_are_retried() {
    assert!(transient(&Error::Sandbox("HTTP 503 sandbox_unavailable".into())));
    assert!(!transient(&Error::Sandbox("tool reported an error: stale element token".into())));
    assert!(!transient(&Error::OwnerDriving));
    assert!(!transient(&Error::Sandbox("{\"code\":\"background_unavailable\"} HTTP 503".into())));
}
