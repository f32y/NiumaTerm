//! Security-property tests for the Noise channel: these encode the spec's
//! hard requirements (tampering, replay, unauthorized device) and must fail
//! if the crypto wiring ever regresses.

use nmt_remote_protocol::{
    ClientBound, Frame, Handshake, HostBound, NoiseError, SecureChannel, generate_keypair,
    handshake_step,
};

fn complete(mut initiator: Handshake, mut responder: Handshake) -> (SecureChannel, SecureChannel) {
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

fn ik_pair() -> (SecureChannel, SecureChannel) {
    let host = generate_keypair().unwrap();
    let client = generate_keypair().unwrap();
    let initiator = Handshake::initiator_ik(&client.private, &host.public).unwrap();
    let responder = Handshake::responder_ik(&host.private).unwrap();
    complete(initiator, responder)
}

#[test]
fn tampering_one_byte_fails_decryption() {
    let (mut client, mut host) = ik_pair();
    let mut ciphertext = client.seal(b"secret input").unwrap();
    // Flip one bit in every position in turn — no byte may be malleable.
    for i in 0..ciphertext.len() {
        let mut tampered = ciphertext.clone();
        tampered[i] ^= 0x01;
        assert!(
            matches!(host.open(&tampered), Err(NoiseError::Snow(_))),
            "tampered byte {i} was accepted"
        );
        // A fresh channel per attempt: a decrypt failure poisons the nonce
        // state by design, matching the "treat as fatal, drop connection" rule.
        let (c, h) = ik_pair();
        (client, host) = (c, h);
        ciphertext = client.seal(b"secret input").unwrap();
    }
}

#[test]
fn replayed_frame_fails_decryption() {
    let (mut client, mut host) = ik_pair();
    let ciphertext = client.seal(b"type a command").unwrap();
    assert_eq!(host.open(&ciphertext).unwrap(), b"type a command");
    // The relay (or anyone on the path) re-sends the identical ciphertext:
    // snow's receive nonce has advanced, so this must fail.
    assert!(matches!(host.open(&ciphertext), Err(NoiseError::Snow(_))));
}

#[test]
fn unauthorized_client_static_key_is_visible_and_rejectable_before_data_flows() {
    let host = generate_keypair().unwrap();
    let authorized = generate_keypair().unwrap();
    let intruder = generate_keypair().unwrap();

    let mut initiator = Handshake::initiator_ik(&intruder.private, &host.public).unwrap();
    let mut responder = Handshake::responder_ik(&host.private).unwrap();

    // Host reads message 1 and must already see the client's static key,
    // before it has sent anything back or any application data exists.
    let msg1 = initiator.write_message().unwrap();
    responder.read_message(&msg1).unwrap();
    let remote = responder
        .remote_static()
        .expect("IK reveals initiator static in msg1");
    assert_eq!(remote, intruder.public.as_slice());
    assert_ne!(remote, authorized.public.as_slice());
    // Policy layer: not in the allowlist → the host drops the connection here.
    // No transport channel ever comes into existence for the intruder.
}

#[test]
fn frames_survive_the_encrypted_channel() {
    let (mut client, mut host) = ik_pair();

    let request = Frame::control(&HostBound::ListSessions).unwrap();
    let ct = client.seal(&request.encode().unwrap()).unwrap();
    let Frame::Control(payload) = Frame::decode(&host.open(&ct).unwrap()).unwrap() else {
        panic!("expected control frame");
    };
    assert_eq!(
        Frame::parse_control::<HostBound>(&payload).unwrap(),
        HostBound::ListSessions
    );

    let reply = Frame::control(&ClientBound::SessionList(Vec::new())).unwrap();
    let ct = host.seal(&reply.encode().unwrap()).unwrap();
    let Frame::Control(payload) = Frame::decode(&client.open(&ct).unwrap()).unwrap() else {
        panic!("expected control frame");
    };
    assert_eq!(
        Frame::parse_control::<ClientBound>(&payload).unwrap(),
        ClientBound::SessionList(Vec::new())
    );

    let output = Frame::Output {
        session_id: 1,
        seq: 7,
        data: b"C:\\>".to_vec(),
    };
    let ct = host.seal(&output.encode().unwrap()).unwrap();
    assert_eq!(Frame::decode(&client.open(&ct).unwrap()).unwrap(), output);
}
