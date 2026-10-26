//! The direct path to a peer behind a NAT: STUN gathering on a fresh UDP
//! socket, hole punching, and a QUIC connection on that socket. One QUIC
//! stream runs WebSocket framing, so the preface, the channel handshake, and
//! the pump work on it as on the LAN and the relay.
//!
//! Two peers behind one public address take the same QUIC path over their
//! private addresses instead: an office network that splits wired and
//! wireless clients into subnets blocks the LAN listener from the phone's
//! side while still routing from the computer's side.

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket as StdUdpSocket};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use nmt_remote_core::direct::{
    DirectOffer, NatKind, Side, binding_request, classify, dialer, mapped_address,
};
use quinn::rustls::RootCertStore;
use quinn::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use quinn::{
    ClientConfig, Connection, Endpoint, EndpointConfig, IdleTimeout, RecvStream, SendStream,
    ServerConfig, TokioRuntime, TransportConfig,
};
use rcgen::CertifiedKey;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{UdpSocket, lookup_host};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Role;
use tracing::{Instrument as _, debug, info};

#[cfg(feature = "lan")]
use crate::lan::lan_addresses;

/// Servers in mainland China come first: a network with one egress per
/// region shows servers abroad an address that peers in China never see.
const BUILTIN_STUN: &[&str] = &[
    "stun.miwifi.com:3478",
    "stun.chat.bilibili.com:3478",
    "stun.cloudflare.com:3478",
    "stun.l.google.com:19302",
];

/// Public servers abroad, one `host:port` per line, tried after the
/// built-in ones.
const MORE_STUN: &str = include_str!("../../../assets/stun.txt");

/// How many servers gathering asks at once. Two answers classify the NAT;
/// the rest of a batch covers servers that are down or blocked on this
/// network, and the next batch is asked only while fewer than two answered.
const STUN_BATCH: usize = 8;

/// How long one batch's names may take to resolve. A blocked name can stall
/// the system resolver for many seconds, and later batches cover it.
const STUN_RESOLVE_WAIT: Duration = Duration::from_secs(2);

/// How long one batch waits for answers, resending every [`STUN_RESEND`] to
/// cover a lost datagram.
const STUN_WAIT: Duration = Duration::from_millis(1500);

const STUN_RESEND: Duration = Duration::from_millis(500);

/// The name in each side's self-signed certificate. The dialer checks the
/// certificate itself, so the name only has to match on both sides.
const SERVER_NAME: &str = "direct.niumaterm.invalid";

/// How often the waiting side sends punch datagrams to the peer.
const PUNCH_INTERVAL: Duration = Duration::from_millis(250);

/// A zero first byte has the QUIC fixed bit clear, so the peer's endpoint
/// drops these datagrams without answering them.
const PUNCH: [u8; 4] = [0; 4];

/// How long either side waits for the QUIC connection. The dialer's Initial
/// packets that arrive before the waiting side's punch opened its NAT are
/// lost, and QUIC resends them with a growing interval.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

/// QUIC keep-alives hold the NAT mappings open on both sides: consumer NATs
/// drop idle UDP mappings after as little as 30 s.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// The channel pump probes a quiet link after 30 s and gives up after two
/// missed probes, so QUIC's own idle limit only has to outlast that.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// The STUN servers to ask: the configured list, or the built-in one.
pub(crate) fn stun_servers(configured: &[String]) -> Vec<String> {
    if !configured.is_empty() {
        return configured.to_vec();
    }

    BUILTIN_STUN
        .iter()
        .copied()
        .chain(MORE_STUN.lines().map(str::trim))
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A UDP socket with the public mappings STUN servers reported for it, its
/// addresses on this side's own network, and the certificate this side
/// presents if it ends up waiting.
pub(crate) struct Gathered {
    socket: StdUdpSocket,
    nat: NatKind,
    addrs: Vec<SocketAddr>,
    local: Vec<SocketAddr>,
    cert: CertifiedKey,
}

impl Gathered {
    pub(crate) fn offer(&self) -> DirectOffer {
        DirectOffer {
            nat: self.nat,
            addrs: self.addrs.iter().map(SocketAddr::to_string).collect(),
            local: self.local.iter().map(SocketAddr::to_string).collect(),
            cert: STANDARD.encode(self.cert.cert.der()),
        }
    }

    /// Whether the peer shares a public address with this side, which puts
    /// both behind one NAT. A NAT that does not hairpin drops a connection
    /// to the public address, so such peers meet over their private
    /// addresses with [`prepare_local`].
    pub(crate) fn same_nat(&self, peer: &DirectOffer) -> bool {
        peer.addrs
            .iter()
            .filter_map(|addr| addr.parse::<SocketAddr>().ok())
            .any(|peer| self.addrs.iter().any(|own| own.ip() == peer.ip()))
    }
}

/// Bind a UDP socket and ask STUN servers for its public mappings, a batch
/// at a time in list order, until two have answered or the list ends.
pub(crate) async fn gather(servers: &[String]) -> Result<Gathered> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let started = Instant::now();

    info!(local_socket = ?socket.local_addr().ok(), servers = servers.len(), "gathering direct candidates");

    let mut asked = HashSet::new();
    let mut mapped = Vec::new();
    let mut route_target = None;

    for (batch_index, batch) in servers.chunks(STUN_BATCH).enumerate() {
        let resolved = resolve(batch).await;
        let resolved_count = resolved.len();

        // Names that share an address answer for one server; asking it again
        // would count one mapping twice.
        let fresh: Vec<SocketAddr> = resolved
            .into_iter()
            .filter(|server| asked.insert(*server))
            .collect();

        if route_target.is_none() {
            route_target = fresh.first().copied();
        }

        let answers = query(&socket, &fresh).await?;

        info!(batch = batch_index + 1, names = batch.len(), resolved = resolved_count,
            queried = fresh.len(), answered = answers.len(), mappings = ?answers,
            elapsed_ms = started.elapsed().as_millis(), "STUN batch completed");

        mapped.extend(answers);

        if mapped.len() >= 2 {
            break;
        }
    }

    let nat = classify(&mapped).with_context(|| {
        format!(
            "fewer than two STUN servers answered ({} answers from {} endpoints)",
            mapped.len(),
            asked.len()
        )
    })?;

    let mut addrs = Vec::new();

    for addr in mapped {
        if !addrs.contains(&addr) {
            addrs.push(addr);
        }
    }

    let local = local_addresses(socket.local_addr()?.port(), route_target);

    info!(?nat, public_candidates = ?addrs, local_candidates = ?local, ?route_target,
        elapsed_ms = started.elapsed().as_millis(), "direct candidates gathered");

    let cert = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()])?;

    Ok(Gathered {
        socket: socket.into_std()?,
        nat,
        addrs,
        local,
        cert,
    })
}

/// The addresses a peer on this side's own network may reach `port` at.
/// The source address of the route toward a STUN server comes first: it is
/// the interface that faces the shared egress, and a client build without
/// interface enumeration has nothing else. Connecting a UDP socket sends
/// nothing; it only selects the route. The host adds every LAN address it
/// has, since a VPN that owns the default route hides the LAN interface
/// from the route lookup.
fn local_addresses(port: u16, route_target: Option<SocketAddr>) -> Vec<SocketAddr> {
    let mut ips: Vec<Ipv4Addr> = Vec::new();

    if let Some(target) = route_target
        && let Ok(probe) = StdUdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        && probe.connect(target).is_ok()
        && let Ok(SocketAddr::V4(source)) = probe.local_addr()
        && !source.ip().is_unspecified()
    {
        ips.push(*source.ip());
    }

    #[cfg(feature = "lan")]
    for ip in lan_addresses() {
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }

    ips.into_iter()
        .map(|ip| SocketAddr::new(IpAddr::V4(ip), port))
        .collect()
}

/// The IPv4 addresses of `batch`, skipping names that do not resolve in
/// time.
async fn resolve(batch: &[String]) -> Vec<SocketAddr> {
    let lookups = batch
        .iter()
        .map(|server| async move {
            match timeout(STUN_RESOLVE_WAIT, lookup_host(server.as_str())).await {
                Ok(Ok(mut addresses)) => {
                    let address = addresses.find(SocketAddr::is_ipv4);

                    if address.is_none() {
                        debug!(%server, "STUN name has no IPv4 address");
                    }

                    address
                }
                Ok(Err(error)) => {
                    debug!(%server, %error, "STUN name lookup failed");

                    None
                }
                Err(_) => {
                    debug!(%server, "STUN name lookup timed out");

                    None
                }
            }
        })
        .collect::<FuturesUnordered<_>>();

    let mut resolved: Vec<SocketAddr> = lookups
        .filter_map(|addr| async move { addr })
        .collect()
        .await;

    resolved.sort();
    resolved.dedup();

    resolved
}

/// Send binding requests to `servers` from `socket` and return the mapped
/// addresses that came back within [`STUN_WAIT`].
async fn query(socket: &UdpSocket, servers: &[SocketAddr]) -> Result<Vec<SocketAddr>> {
    let transactions: Vec<[u8; 12]> = servers
        .iter()
        .map(|_| {
            let mut transaction = [0u8; 12];

            getrandom::fill(&mut transaction).map(|()| transaction)
        })
        .collect::<Result<_, _>>()
        .map_err(|error| anyhow!("random source failed: {error}"))?;

    let mut answers: Vec<Option<SocketAddr>> = vec![None; servers.len()];

    let deadline = Instant::now() + STUN_WAIT;

    let mut resend = Instant::now();
    let mut buf = [0u8; 1024];
    let mut send_errors = 0;
    let mut receive_errors = 0;
    let mut last_error = None;

    while Instant::now() < deadline && answers.iter().any(Option::is_none) {
        if Instant::now() >= resend {
            for ((server, transaction), answer) in servers.iter().zip(&transactions).zip(&answers) {
                if answer.is_none()
                    && let Err(error) = socket.send_to(&binding_request(transaction), server).await
                {
                    send_errors += 1;
                    debug!(%server, %error, "STUN request send failed");
                    last_error = Some(error);
                }
            }

            resend = Instant::now() + STUN_RESEND;
        }

        let received = tokio::select! {
            received = socket.recv_from(&mut buf) => received,
            () = sleep_until(resend.min(deadline)) => continue,
        };

        // Windows reports an ICMP port-unreachable from an earlier datagram
        // as an error on the next receive; the server that sent it simply
        // stays unanswered.
        let (len, from) = match received {
            Ok(received) => received,
            Err(error) => {
                receive_errors += 1;
                last_error = Some(error);

                continue;
            }
        };

        if let Some(index) = servers.iter().position(|server| *server == from)
            && let Some(mapped) = mapped_address(&buf[..len], &transactions[index])
        {
            debug!(server = %from, %mapped, "STUN mapping received");
            answers[index] = Some(mapped);
        }
    }

    if let Some(error) = last_error {
        info!(send_errors, receive_errors, %error, "STUN socket errors during batch");
    }

    Ok(answers.into_iter().flatten().collect())
}

/// A direct path whose socket is ready: the waiting side punches and
/// accepts, the dialer has yet to dial.
pub(crate) struct Prepared {
    endpoint: Endpoint,
    peers: Vec<SocketAddr>,
    me: Side,
    dial: Option<ClientConfig>,
    punching: Option<JoinHandle<()>>,
}

impl Drop for Prepared {
    fn drop(&mut self) {
        if let Some(punching) = &self.punching {
            punching.abort();
        }
    }
}

/// Set up this side's QUIC endpoint for a direct path to `peer` through
/// its public addresses. The waiting side starts punching here, so a host
/// replies to the offer only once its NAT admits the peer.
pub(crate) fn prepare(gathered: Gathered, peer: &DirectOffer, me: Side) -> Result<Prepared> {
    let peers = candidates(&peer.addrs);

    info!(side = ?me, route = "public", local_nat = ?gathered.nat, peer_nat = ?peer.nat,
        peer_candidates = ?peers, "selecting direct route");

    if peers.is_empty() {
        bail!("the peer has no public address");
    }

    let (client_nat, host_nat) = match me {
        Side::Client => (gathered.nat, peer.nat),
        Side::Host => (peer.nat, gathered.nat),
    };

    let Some(dialer) = dialer(client_nat, host_nat) else {
        bail!("both sides are behind symmetric NATs");
    };

    setup(gathered, &peer.cert, peers, me, dialer)
}

/// Set up this side's QUIC endpoint for a direct path to `peer` through
/// its private addresses, for two sides behind one public address. The host
/// dials: office networks that split clients into subnets admit
/// connections from the wired side and drop those from the wireless side,
/// the same split that blocks the phone from the LAN listener.
/// No NAT stands between the two, so the NAT kinds do not matter.
pub(crate) fn prepare_local(gathered: Gathered, peer: &DirectOffer, me: Side) -> Result<Prepared> {
    let peers = candidates(&peer.local);

    info!(side = ?me, route = "local", local_nat = ?gathered.nat, peer_nat = ?peer.nat,
        peer_candidates = ?peers, "selecting direct route");

    if peers.is_empty() {
        bail!("the peer is behind the same NAT and offered no private address");
    }

    setup(gathered, &peer.cert, peers, me, Side::Host)
}

fn candidates(addrs: &[String]) -> Vec<SocketAddr> {
    addrs
        .iter()
        .filter_map(|addr| addr.parse().ok())
        .filter(SocketAddr::is_ipv4)
        .collect()
}

/// Build the endpoint toward `peers`: a client configuration trusting
/// `peer_cert` when this side dials, otherwise a server on this side's own
/// certificate plus punching toward every peer address.
fn setup(
    gathered: Gathered,
    peer_cert: &str,
    peers: Vec<SocketAddr>,
    me: Side,
    dialer: Side,
) -> Result<Prepared> {
    let Gathered { socket, cert, .. } = gathered;

    info!(side = ?me, ?dialer, local_socket = ?socket.local_addr().ok(), peer_candidates = ?peers,
        "preparing direct QUIC endpoint");

    let transport = Arc::new(transport_config()?);
    let runtime = Arc::new(TokioRuntime);

    if dialer == me {
        let peer_cert = CertificateDer::from(STANDARD.decode(peer_cert)?);

        let mut roots = RootCertStore::empty();

        roots.add(peer_cert)?;

        let mut client = ClientConfig::with_root_certificates(Arc::new(roots))?;

        client.transport_config(transport);

        let endpoint = Endpoint::new(EndpointConfig::default(), None, socket, runtime)?;

        return Ok(Prepared {
            endpoint,
            peers,
            me,
            dial: Some(client),
            punching: None,
        });
    }

    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    let mut server = ServerConfig::with_single_cert(vec![cert.cert.der().clone()], key)?;

    server.transport_config(transport);

    let punch_socket = socket.try_clone()?;

    let endpoint = Endpoint::new(EndpointConfig::default(), Some(server), socket, runtime)?;

    let punching = tokio::spawn(punch(punch_socket, peers.clone()).in_current_span());

    Ok(Prepared {
        endpoint,
        peers,
        me,
        dial: None,
        punching: Some(punching),
    })
}

impl Prepared {
    /// Complete the QUIC connection and open its stream. Returns the socket
    /// and the peer address that answered.
    pub(crate) async fn connect(mut self) -> Result<(DirectSocket, SocketAddr)> {
        let started = Instant::now();

        info!(side = ?self.me, dialing = self.dial.is_some(), timeout_ms = CONNECT_WAIT.as_millis(),
            "connecting direct QUIC endpoint");

        let connection = match self.dial.take() {
            Some(client) => timeout(CONNECT_WAIT, dial(&self.endpoint, &self.peers, &client))
                .await
                .map_err(|_| anyhow!("the peer did not answer the direct path"))??,
            None => timeout(CONNECT_WAIT, accept(&self.endpoint, &self.peers))
                .await
                .map_err(|_| anyhow!("the peer did not reach the direct path"))??,
        };

        if let Some(punching) = self.punching.take() {
            punching.abort();
        }

        let remote = connection.remote_address();

        info!(%remote, elapsed_ms = started.elapsed().as_millis(), "direct QUIC connected");

        // The client writes first on every path, which also makes the stream
        // visible to the host: QUIC announces a stream with its first data.
        let (send, recv, role) = match self.me {
            Side::Client => {
                let (send, recv) = connection.open_bi().await?;

                (send, recv, Role::Client)
            }
            Side::Host => {
                let (send, recv) = timeout(CONNECT_WAIT, connection.accept_bi())
                    .await
                    .map_err(|_| anyhow!("the client opened no stream"))??;

                (send, recv, Role::Server)
            }
        };

        let io = DirectIo {
            send,
            recv,
            _connection: connection,
            _endpoint: self.endpoint.clone(),
        };

        info!(%remote, elapsed_ms = started.elapsed().as_millis(), "direct stream ready");

        Ok((
            WebSocketStream::from_raw_socket(io, role, None).await,
            remote,
        ))
    }
}

fn transport_config() -> Result<TransportConfig> {
    let mut transport = TransportConfig::default();

    transport
        .keep_alive_interval(Some(KEEP_ALIVE))
        .max_idle_timeout(Some(IdleTimeout::try_from(IDLE_TIMEOUT)?));

    Ok(transport)
}

/// Send punch datagrams to every peer address until aborted. Each opens
/// this side's NAT for packets from that address.
async fn punch(socket: StdUdpSocket, peers: Vec<SocketAddr>) {
    let mut reported = HashSet::new();

    loop {
        for peer in &peers {
            // The socket is non-blocking; a full send buffer only skips one
            // round.
            if let Err(error) = socket.send_to(&PUNCH, peer)
                && error.kind() != io::ErrorKind::WouldBlock
                && reported.insert(*peer)
            {
                info!(%peer, %error, "direct punch send failed");
            }
        }

        sleep(PUNCH_INTERVAL).await;
    }
}

/// Connect to every peer address at once and keep the first connection.
async fn dial(
    endpoint: &Endpoint,
    peers: &[SocketAddr],
    client: &ClientConfig,
) -> Result<Connection> {
    let mut attempts: FuturesUnordered<_> = peers
        .iter()
        .map(|peer| {
            endpoint
                .connect_with(client.clone(), *peer, SERVER_NAME)
                .with_context(|| format!("starting direct QUIC connection to {peer} failed"))
                .map(|attempt| async move { (*peer, attempt.await) })
        })
        .collect::<Result<_, _>>()?;

    let mut last_error = anyhow!("no peer address to dial");

    while let Some((peer, result)) = attempts.next().await {
        match result {
            Ok(connection) => return Ok(connection),
            Err(error) => {
                info!(%peer, %error, "direct QUIC candidate failed");
                last_error = error.into();
            }
        }
    }

    Err(last_error)
}

/// Accept the first connection from one of the peer's addresses. Others are
/// refused: the port is open to anyone who learns it while punching runs.
async fn accept(endpoint: &Endpoint, peers: &[SocketAddr]) -> Result<Connection> {
    let mut reported_unexpected = false;

    loop {
        let incoming = endpoint
            .accept()
            .await
            .context("the direct endpoint closed")?;

        let from = incoming.remote_address();

        if !peers.iter().any(|peer| peer.ip() == from.ip()) {
            if !reported_unexpected {
                info!(%from, expected_candidates = ?peers, "refusing a direct connection from an unexpected address");
                reported_unexpected = true;
            }

            incoming.refuse();

            continue;
        }

        match incoming.await {
            Ok(connection) => return Ok(connection),
            Err(error) => info!(%from, %error, "incoming direct QUIC handshake failed"),
        }
    }
}

pub(crate) type DirectSocket = WebSocketStream<DirectIo>;

/// One QUIC stream as a byte stream. It holds the connection and endpoint
/// too: the endpoint's driver stops once no handle to it is left.
pub(crate) struct DirectIo {
    send: SendStream,
    recv: RecvStream,
    _connection: Connection,
    _endpoint: Endpoint,
}

impl AsyncRead for DirectIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for DirectIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}
