# remote-transport-protocol

End-to-end encrypted Host-to-Client transport and binary frame format in `crates/remote_protocol`.

## ADDED Requirements

### Requirement: Establish a Noise end-to-end encrypted channel
The system SHALL establish an end-to-end encrypted channel between the Host and Client with the Noise Protocol from the `snow` crate. Normal connections SHALL use IK mode, in which the Client already knows the Host's static public key. The first pairing connection SHALL use XX mode to exchange static public keys. The system SHALL reject all application frames until the handshake completes.

#### Scenario: IK handshake succeeds
- **WHEN** a Client that knows the Host's static public key starts an IK handshake and its own static public key is in the Host's authorized-device list
- **THEN** both sides enter the transport phase and can exchange encrypted frames

#### Scenario: Unauthorized device is rejected
- **WHEN** the Client's static public key in an IK handshake is absent from the Host's authorized-device list
- **THEN** the Host terminates the handshake and closes the connection without exposing session data

#### Scenario: Modified frame is rejected
- **WHEN** an intermediary changes any byte of an encrypted frame during the transport phase
- **THEN** decryption fails, and the recipient discards the frame and closes the channel

#### Scenario: Replayed frame is rejected
- **WHEN** an intermediary resends a previously valid encrypted frame
- **THEN** decryption fails because the nonce counter does not match, and the recipient discards the frame and closes the channel

### Requirement: Encode binary frames
The system SHALL use binary frames within the Noise channel, with one frame per transport message. One WS binary message carries one Noise ciphertext and decrypts to one frame, so no additional length prefix is needed. The first byte SHALL identify the frame type: `0x00` for a control frame containing a postcard-serialized enum, `0x01` for Output as `[session_id u64 LE][seq u64][bytes]`, `0x02` for Input as `[session_id][bytes]`, `0x03` for Resize as `[session_id][cols u16][rows u16]`, and `0x04` for Exited as `[session_id][seq u64]`. Encoding and decoding SHALL round-trip without loss.

#### Scenario: Encode and decode round trip
- **WHEN** any valid frame is encoded and then decoded
- **THEN** the result equals the original frame

#### Scenario: Frame type is unknown
- **WHEN** the decoder encounters an undefined first byte
- **THEN** it returns an error instead of panicking, allowing the caller to close the channel

### Requirement: Encode and decode pairing codes
The system SHALL encode and decode one-time pairing codes as base32 representations of `{relay_url, host_id, host_static_pubkey, pairing_token(16B)}` that users can transcribe manually.

#### Scenario: Pairing-code round trip
- **WHEN** the Host creates a pairing code and the Client parses it
- **THEN** the Client obtains the same relay_url, host_id, public key, and token created by the Host

#### Scenario: Pairing code is damaged
- **WHEN** the user enters a truncated or mistyped pairing code
- **THEN** parsing returns a clear error without panicking
