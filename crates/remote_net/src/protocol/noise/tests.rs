use crate::protocol::noise::*;

/// Run both sides of a handshake in memory until finished.
pub(crate) fn complete(
    mut initiator: Handshake,
    mut responder: Handshake,
) -> (SecureChannel, SecureChannel) {
    let mut to_responder = Some(initiator.write_message().unwrap());
    loop {
        let to_initiator = handshake_step(&mut responder, to_responder.as_deref()).unwrap();
        to_responder = handshake_step(&mut initiator, to_initiator.as_deref()).unwrap();
        if initiator.is_finished() && responder.is_finished() {
            return (
                initiator.into_transport().unwrap(),
                responder.into_transport().unwrap(),
            );
        }
    }
}

#[test]
fn ik_handshake_and_transport() {
    let host = generate_keypair().unwrap();
    let client = generate_keypair().unwrap();
    let initiator = Handshake::initiator_ik(&client.private, &host.public).unwrap();
    let responder = Handshake::responder_ik(&host.private).unwrap();
    let (mut client_chan, mut host_chan) = complete(initiator, responder);

    assert_eq!(host_chan.remote_static(), Some(client.public.as_slice()));
    let ct = client_chan.seal(b"dir\r").unwrap();
    assert_eq!(host_chan.open(&ct).unwrap(), b"dir\r");
    let ct = host_chan.seal(b"output").unwrap();
    assert_eq!(client_chan.open(&ct).unwrap(), b"output");
}

#[test]
fn xx_handshake_reveals_both_statics() {
    let host = generate_keypair().unwrap();
    let client = generate_keypair().unwrap();
    let initiator = Handshake::initiator_xx(&client.private).unwrap();
    let responder = Handshake::responder_xx(&host.private).unwrap();
    let (client_chan, host_chan) = complete(initiator, responder);
    assert_eq!(client_chan.remote_static(), Some(host.public.as_slice()));
    assert_eq!(host_chan.remote_static(), Some(client.public.as_slice()));
}
