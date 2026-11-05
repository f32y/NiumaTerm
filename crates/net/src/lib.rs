//! The proxy the application's own network requests go through. Shells and
//! agents a terminal starts are child processes with their own environment;
//! this covers only what NiumaTerm itself sends: the relay, update checks
//! and downloads, push notifications, and usage queries.
//!
//! The setting is process-wide and read when a client or socket is made, so
//! a change applies to the next request or reconnect, not to sockets that
//! are already open.

#[cfg(test)]
mod tests;

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use hyper_util::client::proxy::matcher::{Intercept, Matcher};
use nmt_config::system::ProxyMode;
use parking_lot::RwLock;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout};
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async_tls};
use tracing::{info, warn};

pub type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How long opening a WebSocket may take: the TCP connection, the proxy
/// handshake, TLS, and the upgrade. Without it a blackholed address holds
/// the caller until the operating system gives up, about 21 seconds on
/// Windows.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A proxy's reply to `CONNECT` is a short status line and a few headers;
/// anything longer is not a proxy speaking HTTP.
const MAX_CONNECT_REPLY: usize = 8 * 1024;

/// Where the application's connections go.
enum Route {
    Direct,
    System,
    Proxy(String),
}

static ROUTE: RwLock<Route> = RwLock::new(Route::System);

/// Apply the proxy setting. An HTTP or SOCKS mode without a URL uses the
/// system proxy, since an empty field is more likely unfinished than a
/// request to go direct.
pub fn set_proxy(mode: ProxyMode, url: &str) {
    let url = url.trim();

    *ROUTE.write() = match mode {
        ProxyMode::Off => Route::Direct,
        ProxyMode::System => Route::System,
        _ if url.is_empty() => Route::System,
        ProxyMode::Http | ProxyMode::Socks if url.contains("://") => Route::Proxy(url.to_owned()),
        ProxyMode::Http => Route::Proxy(format!("http://{url}")),
        // `socks5h` resolves the destination at the proxy: a proxy is often
        // there because local DNS for that destination is unreliable.
        ProxyMode::Socks => Route::Proxy(format!("socks5h://{url}")),
    };
}

/// A `reqwest` client builder that uses the configured proxy. reqwest reads
/// the system proxy itself unless told otherwise.
pub fn http_client() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();

    match &*ROUTE.read() {
        Route::Direct => builder.no_proxy(),
        Route::System => builder,
        Route::Proxy(url) => match reqwest::Proxy::all(url) {
            Ok(proxy) => builder.proxy(proxy),
            Err(error) => {
                warn!(%error, "ignoring the proxy setting");

                builder
            }
        },
    }
}

/// Open a WebSocket through the configured proxy, as
/// `tokio_tungstenite::connect_async` would directly.
pub async fn connect_websocket<R>(request: R) -> Result<(WebSocket, Response)>
where
    R: IntoClientRequest + Unpin,
{
    let request = request.into_client_request()?;
    let started = Instant::now();

    let mut stage = "tcp_proxy";

    timeout(CONNECT_TIMEOUT, async {
        info!(stage, "WebSocket connection stage started");

        let tcp = dial(request.uri())
            .await
            .context("opening TCP or proxy connection")?;

        info!(
            stage,
            elapsed_ms = started.elapsed().as_millis(),
            "WebSocket connection stage completed"
        );

        stage = "tls_websocket_upgrade";

        let handshake_started = Instant::now();

        info!(stage, "WebSocket connection stage started");

        let result = client_async_tls(request, tcp)
            .await
            .context("TLS or WebSocket upgrade failed");

        info!(
            stage,
            elapsed_ms = handshake_started.elapsed().as_millis(),
            succeeded = result.is_ok(),
            "WebSocket connection stage ended"
        );

        result
    })
    .await
    .map_err(|_| {
        anyhow!(
            "connecting timed out during {stage} after {} seconds",
            CONNECT_TIMEOUT.as_secs()
        )
    })?
}

/// Open the TCP connection a WebSocket to `uri` runs over, through a proxy
/// when the setting names one for it.
async fn dial(uri: &Uri) -> Result<TcpStream> {
    let host = uri.host().context("the URL has no host")?;
    let secure = uri.scheme_str() == Some("wss");
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });

    // Proxy rules are keyed by HTTP schemes; a WebSocket upgrade is an
    // HTTP request, so it follows the rule for its scheme's counterpart.
    let probe: Uri = format!("{}://{host}:{port}", if secure { "https" } else { "http" })
        .parse()
        .context("the URL has an invalid host")?;

    let proxy = match &*ROUTE.read() {
        Route::Direct => None,
        Route::System => Matcher::from_system().intercept(&probe),
        Route::Proxy(url) => Matcher::builder()
            .all(url.as_str())
            .build()
            .intercept(&probe),
    };

    let Some(proxy) = proxy else {
        return Ok(TcpStream::connect((host, port)).await?);
    };

    match proxy.uri().scheme_str() {
        Some("socks5" | "socks5h") => socks5(&proxy, host, port).await,
        Some("http") => http_connect(&proxy, host, port).await,
        _ => bail!("unsupported proxy {}", proxy.uri()),
    }
}

fn proxy_address(proxy: &Intercept, default_port: u16) -> Result<(String, u16)> {
    let uri = proxy.uri();
    let host = uri.host().context("the proxy URL has no host")?;

    Ok((host.to_owned(), uri.port_u16().unwrap_or(default_port)))
}

async fn socks5(proxy: &Intercept, host: &str, port: u16) -> Result<TcpStream> {
    let (proxy_host, proxy_port) = proxy_address(proxy, 1080)?;
    let address = (proxy_host.as_str(), proxy_port);

    let stream = match proxy.raw_auth() {
        Some((user, password)) => {
            Socks5Stream::connect_with_password(address, (host, port), user, password).await
        }
        None => Socks5Stream::connect(address, (host, port)).await,
    }
    .context("the SOCKS proxy refused the connection")?;

    Ok(stream.into_inner())
}

/// Ask an HTTP proxy for a tunnel with `CONNECT`; the TLS and WebSocket
/// handshakes then run inside it as on a direct connection.
async fn http_connect(proxy: &Intercept, host: &str, port: u16) -> Result<TcpStream> {
    let (proxy_host, proxy_port) = proxy_address(proxy, 80)?;

    let mut tcp = TcpStream::connect((proxy_host.as_str(), proxy_port)).await?;

    let mut request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");

    if let Some(auth) = proxy.basic_auth() {
        let auth = auth
            .to_str()
            .context("the proxy credentials are not text")?;

        request.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }

    request.push_str("\r\n");

    tcp.write_all(request.as_bytes()).await?;

    // Read the reply one byte at a time: whatever follows its blank line
    // already belongs to the tunnel, and buffering past it would lose the
    // start of the TLS handshake.
    let mut reply = Vec::new();

    while !reply.ends_with(b"\r\n\r\n") {
        if reply.len() >= MAX_CONNECT_REPLY {
            bail!("the HTTP proxy sent an oversized reply");
        }

        reply.push(tcp.read_u8().await?);
    }

    let status = String::from_utf8_lossy(&reply);
    let status = status.lines().next().unwrap_or_default();

    match status.split_whitespace().nth(1) {
        Some("200") => Ok(tcp),
        _ => bail!("the HTTP proxy refused the tunnel: {status}"),
    }
}
