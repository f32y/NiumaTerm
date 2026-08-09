use snow::{Builder, HandshakeState, TransportState};

/// ChaChaPoly avoids pulling an AES implementation and is constant-time in
/// pure Rust; BLAKE2s matches the 32-byte curve keys. IK is used once the
/// client already knows the host's static key (normal connections); XX is
/// used for first-contact pairing where both sides learn each other's static
/// key during the handshake.
const PATTERN_IK: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const PATTERN_XX: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Noise caps a single transport message at 65535 bytes (ciphertext incl.
/// 16-byte tag); handshake messages are far smaller.
const MSG_BUF: usize = 65535 + 128;

#[derive(Debug, thiserror::Error)]
pub enum NoiseError {
    /// Covers tampered ciphertext, replayed ciphertext (nonce counter
    /// mismatch), and handshake failures alike — callers treat any of these
    /// as fatal for the channel.
    #[error("noise protocol failure: {0}")]
    Snow(#[from] snow::Error),
    #[error("handshake not finished")]
    HandshakeNotFinished,
}

pub struct StaticKeypair {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

pub fn generate_keypair() -> Result<StaticKeypair, NoiseError> {
    let keypair = Builder::new(PATTERN_IK.parse().expect("valid pattern")).generate_keypair()?;
    Ok(StaticKeypair {
        private: keypair.private,
        public: keypair.public,
    })
}

/// Message-driven handshake state machine, transport-agnostic: the caller
/// shuttles the returned byte blobs over whatever socket it owns.
pub struct Handshake {
    state: HandshakeState,
}

impl Handshake {
    /// Client side of a normal connection: requires the host's static public
    /// key learned during pairing.
    pub fn initiator_ik(local_private: &[u8], remote_public: &[u8]) -> Result<Self, NoiseError> {
        let state = Builder::new(PATTERN_IK.parse().expect("valid pattern"))
            .local_private_key(local_private)?
            .remote_public_key(remote_public)?
            .build_initiator()?;
        Ok(Self { state })
    }

    /// Host side of a normal connection. The client's static key becomes
    /// available via `remote_static()` after reading the first message, which
    /// is when the host checks its authorized-device list.
    pub fn responder_ik(local_private: &[u8]) -> Result<Self, NoiseError> {
        let state = Builder::new(PATTERN_IK.parse().expect("valid pattern"))
            .local_private_key(local_private)?
            .build_responder()?;
        Ok(Self { state })
    }

    /// Client side of a pairing connection (host static key not yet trusted).
    pub fn initiator_xx(local_private: &[u8]) -> Result<Self, NoiseError> {
        let state = Builder::new(PATTERN_XX.parse().expect("valid pattern"))
            .local_private_key(local_private)?
            .build_initiator()?;
        Ok(Self { state })
    }

    /// Host side of a pairing connection.
    pub fn responder_xx(local_private: &[u8]) -> Result<Self, NoiseError> {
        let state = Builder::new(PATTERN_XX.parse().expect("valid pattern"))
            .local_private_key(local_private)?
            .build_responder()?;
        Ok(Self { state })
    }

    /// Produce the next handshake message to send to the peer.
    pub fn write_message(&mut self) -> Result<Vec<u8>, NoiseError> {
        let mut buf = vec![0u8; MSG_BUF];
        let len = self.state.write_message(&[], &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Consume a handshake message received from the peer.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), NoiseError> {
        let mut buf = vec![0u8; MSG_BUF];
        self.state.read_message(message, &mut buf)?;
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// The peer's static public key, once the handshake has revealed it
    /// (IK responder: after message 1; XX responder: after message 3; XX
    /// initiator: after message 2).
    pub fn remote_static(&self) -> Option<&[u8]> {
        self.state.get_remote_static()
    }

    pub fn into_transport(self) -> Result<SecureChannel, NoiseError> {
        if !self.state.is_handshake_finished() {
            return Err(NoiseError::HandshakeNotFinished);
        }
        Ok(SecureChannel {
            state: self.state.into_transport_mode()?,
        })
    }
}

/// Post-handshake encrypted channel. Snow maintains a per-direction nonce
/// counter internally, so a replayed or reordered ciphertext fails `open()` —
/// callers must treat any decrypt error as fatal and drop the connection.
pub struct SecureChannel {
    state: TransportState,
}

impl SecureChannel {
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let mut buf = vec![0u8; plaintext.len() + 16];
        let len = self.state.write_message(plaintext, &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }

    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let mut buf = vec![0u8; ciphertext.len()];
        let len = self.state.read_message(ciphertext, &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }

    pub fn remote_static(&self) -> Option<&[u8]> {
        self.state.get_remote_static()
    }
}

/// Drive `initiator` and `responder` to completion in lockstep. Shared by the
/// host/client connection setup (over real sockets they exchange the same
/// messages asynchronously) and by tests.
pub fn handshake_step(
    handshake: &mut Handshake,
    inbound: Option<&[u8]>,
) -> Result<Option<Vec<u8>>, NoiseError> {
    if let Some(message) = inbound {
        handshake.read_message(message)?;
    }
    if handshake.is_finished() {
        return Ok(None);
    }
    Ok(Some(handshake.write_message()?))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
