//! A host's registration on its relay: the control socket the relay
//! announces clients and pairing slots on, and a data socket per client.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::{SinkExt as _, StreamExt as _};
use nmt_remote_core::identity::DeviceId;
use nmt_remote_core::messages::RelayAccess;
use serde::{Deserialize, Serialize};
use tokio::select;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tracing::{Instrument as _, info, info_span, warn};

use crate::relay::{RelaySocket, open};

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Keep-alive for the host's control socket, answered by the relay without
/// waking its Durable Object.
const CONTROL_PING: Duration = Duration::from_secs(30);

/// Messages the relay and the host exchange on the control socket.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ControlMessage {
    Conn { id: String },
    Slot { slot: String, ttl: u32 },
    SlotOk { slot: String },
    SlotTaken { slot: String },
    SlotRelease { slot: String },
}

/// What the host service asks of its relay link.
pub(crate) enum RelayCommand {
    ClaimSlot(String),
    ReleaseSlot(String),
}

/// Keep a host registered on its relay: hold the control socket, reconnect
/// it with backoff, open a data socket for each client the relay announces,
/// and claim pairing slots on request. `serve` handles each data socket like
/// an accepted LAN connection.
pub(crate) async fn run_host_link(
    relay: RelayAccess,
    host: DeviceId,
    token: String,
    mut commands: UnboundedReceiver<RelayCommand>,
    serve: Arc<dyn Fn(RelaySocket) + Send + Sync>,
) {
    let mut backoff = MIN_BACKOFF;
    let mut claimed: Option<String> = None;

    loop {
        let control = open(&relay, &format!("/v1/host/{}", host.as_str()), Some(&token)).await;

        let mut control = match control {
            Ok(control) => control,
            Err(error) => {
                warn!(retry_secs = backoff.as_secs(), error = %format_args!("{error:#}"), "relay control socket failed");

                // Commands arriving meanwhile still update the wanted slot.
                select! {
                    () = sleep(backoff) => {}
                    command = commands.recv() => match command {
                        Some(RelayCommand::ClaimSlot(slot)) => claimed = Some(slot),
                        Some(RelayCommand::ReleaseSlot(_)) => claimed = None,
                        None => return,
                    },
                }

                backoff = (backoff * 2).min(MAX_BACKOFF);

                continue;
            }
        };

        info!(relay = %relay.url, "registered on the relay");

        backoff = MIN_BACKOFF;

        // A slot claimed before a reconnect is claimed again.
        if let Some(slot) = &claimed
            && let Err(error) = send_control(
                &mut control,
                &ControlMessage::Slot {
                    slot: slot.clone(),
                    ttl: 300,
                },
            )
            .await
        {
            warn!(error = %format_args!("{error:#}"), "restoring relay pairing registration failed");
        }

        loop {
            select! {
                message = control.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ControlMessage>(&text) {
                            Ok(ControlMessage::Conn { id }) => {
                                let relay = relay.clone();
                                let token = token.clone();
                                let path = format!("/v1/host/{}/accept/{id}", host.as_str());
                                let serve = Arc::clone(&serve);
                                let span = info_span!("relay_data", connection = %id);

                                tokio::spawn(async move {
                                    info!("opening relay data socket");
                                    match open(&relay, &path, Some(&token)).await {
                                        Ok(ws) => serve(ws),
                                        Err(error) => warn!(error = %format_args!("{error:#}"), "relay data socket failed"),
                                    }
                                }.instrument(span));
                            }
                            Ok(ControlMessage::SlotTaken { slot }) => {
                                warn!(%slot, "another host on this relay holds the pairing slot");
                            }
                            Err(error) => warn!(%error, "invalid relay control message"),
                            Ok(_) => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        info!(%error, "relay control socket dropped");

                        break;
                    }
                    None => {
                        info!("relay control socket closed");
                        break;
                    }
                },
                command = commands.recv() => {
                    let message = match command {
                        Some(RelayCommand::ClaimSlot(slot)) => {
                            claimed = Some(slot.clone());

                            ControlMessage::Slot { slot, ttl: 300 }
                        }
                        Some(RelayCommand::ReleaseSlot(slot)) => {
                            claimed = None;

                            ControlMessage::SlotRelease { slot }
                        }
                        None => {
                            let _ = control.close(None).await;

                            return;
                        }
                    };

                    if let Err(error) = send_control(&mut control, &message).await {
                        info!(error = %format_args!("{error:#}"), "relay control message send failed");

                        break;
                    }
                }

                () = sleep(CONTROL_PING) => {
                    if let Err(error) = control.send(Message::Text("ping".into())).await {
                        info!(%error, "relay keepalive send failed");

                        break;
                    }
                }
            }
        }

        info!(
            retry_secs = backoff.as_secs(),
            "reconnecting relay control socket"
        );

        sleep(backoff).await;
    }
}

async fn send_control(control: &mut RelaySocket, message: &ControlMessage) -> Result<()> {
    control
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .await?;

    Ok(())
}
