use crate::windows::notifier::*;

#[test]
fn toast_xml_escapes_text_attributes_and_uses_protocol_activation() {
    let xml = toast_xml(&NativeNotification {
        title: "A < B & C".into(),
        body: "say > \"yes\"".into(),
        activation_url: "nmt://action/focus_notification?route=a&notification_id=\"b\"".into(),
        tag: "tag".into(),
        group: "group".into(),
    });
    assert!(xml.contains(r#"activationType="protocol""#));
    assert!(xml.contains("A &lt; B &amp; C"));
    assert!(xml.contains("route=a&amp;notification_id=&quot;b&quot;"));
    assert!(xml.contains("say &gt; \"yes\""));
}

#[test]
fn empty_title_falls_back_to_niuma_term_identity() {
    let xml = toast_xml(&NativeNotification {
        title: String::new(),
        body: String::new(),
        activation_url: String::new(),
        tag: "tag".into(),
        group: "group".into(),
    });
    assert!(xml.contains("<text>NiumaTerm</text>"));
    assert!(!xml.contains("activationType"));
}

#[test]
#[ignore = "shows a real Windows Toast"]
fn windows_toast_smoke() {
    register_identity(&env::current_exe().unwrap())
        .expect("smoke executable should register its native identity");
    let notification = NativeNotification {
        title: "NiumaTerm Toast smoke test".into(),
        body: "If you see this, native notification identity works.".into(),
        activation_url: "nmt://action/activate".into(),
        tag: "niuma-term-smoke".into(),
        group: "manual-test".into(),
    };
    show(&notification).expect("Windows should accept the Toast");
    remove(&notification.tag, &notification.group)
        .expect("Windows should remove the Toast by tag and group");
}

#[test]
#[ignore = "leaves a real Windows Toast in Notification Center"]
fn windows_toast_visual_smoke() {
    register_identity(&env::current_exe().unwrap())
        .expect("smoke executable should register its native identity");
    show(&NativeNotification {
        title: "NiumaTerm visual smoke test".into(),
        body: "This notification is intentionally not removed.".into(),
        activation_url: "nmt://action/activate".into(),
        tag: "visual-smoke".into(),
        group: "NiumaTerm".into(),
    })
    .expect("Windows should accept the visual Toast");
}
