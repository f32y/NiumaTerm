use crate::channel::NetError;

#[test]
fn only_remote_rejections_stop_reconnect_attempts() {
    let rejection = NetError::Protocol("session was killed".into());
    assert_eq!(
        rejection.permanent_reconnect_reason(),
        Some("session was killed")
    );
    assert_eq!(
        NetError::Internal("runtime unavailable".into()).permanent_reconnect_reason(),
        None
    );
    assert_eq!(NetError::Timeout.permanent_reconnect_reason(), None);
}
