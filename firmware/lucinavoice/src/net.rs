//! Wi-Fi station, DNS and TLS to [`HOST`], with one HTTP request per
//! connection (`Connection: close`).
//!
//! TLS: Mbed TLS (`mbedtls-rs`), chain and hostname verified against the
//! ISRG roots in `certs/isrg-roots.pem` (X1 and X2, taken from the macOS
//! system roots on 2026-11-01). The ESP32 has no wall clock, so certificate
//! dates are not checked.

use alloc::vec;
use alloc::vec::Vec;
use core::ffi::CStr;

use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Timer};
use embedded_io_async::{Read, Write};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Config, WifiController};
use lucinavoice_core::HOST;
use lucinavoice_core::http::{self, Body, ChunkedDecoder, HeadParser};
use lucinavoice_core::settings::Settings;
use mbedtls_rs::{
    Certificate, ClientSessionConfig, Session, SessionConfig, SessionError, TlsReference,
    TlsVersion, X509,
};

const CA_PEM: &str = concat!(include_str!("../certs/isrg-roots.pem"), "\0");
const HOST_C: &CStr = c"getsecretary.tech";
/// Every read or write waits at most this long (a voice turn's answer
/// needs transcription and Lucina's reply first).
const SOCKET_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug)]
pub enum NetError {
    Dns,
    Connect,
    Tls(SessionError),
    /// Malformed HTTP from the server.
    Http,
    /// The request head did not fit its buffer.
    TooLong,
}

impl From<SessionError> for NetError {
    fn from(e: SessionError) -> Self {
        NetError::Tls(e)
    }
}

/// Station settings from the saved Wi-Fi fields; `None` if they do not fit.
pub fn station_config(settings: &Settings) -> Option<Config> {
    let ssid = settings.ssid.as_str().try_into().ok()?;
    let auth = if settings.password.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(settings.password.as_str().try_into().ok()?)
    };
    Some(Config::Station(
        StationConfig::default()
            .with_ssid(ssid)
            .with_authentication(auth),
    ))
}

/// Keeps the station connected, retrying every 5 s.
#[embassy_executor::task]
pub async fn wifi_task(mut controller: WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                log::info!("wifi: connected");
                let _ = controller.wait_for_disconnect_async().await;
                log::warn!("wifi: disconnected");
            }
            Err(e) => log::warn!("wifi: connect failed: {e:?}"),
        }
        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
pub async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::Interface>) {
    runner.run().await
}

#[derive(Clone)]
pub struct Client {
    pub stack: Stack<'static>,
    tls: TlsReference<'static>,
    ca: Certificate<'static>,
}

pub type TlsStream<'s> = Session<'static, TcpSocket<'s>>;

impl Client {
    pub fn new(stack: Stack<'static>, tls: TlsReference<'static>) -> Self {
        let pem = CStr::from_bytes_with_nul(CA_PEM.as_bytes()).expect("CA PEM has one NUL");
        let ca = Certificate::new(X509::PEM(pem)).expect("ISRG roots parse");
        Self { stack, tls, ca }
    }

    pub fn is_up(&self) -> bool {
        self.stack.is_config_up()
    }

    /// A TLS connection to [`HOST`]:443, handshake done.
    pub async fn connect<'s>(
        &self,
        rx: &'s mut [u8],
        tx: &'s mut [u8],
    ) -> Result<TlsStream<'s>, NetError> {
        let addrs = self
            .stack
            .dns_query(HOST, DnsQueryType::A)
            .await
            .map_err(|_| NetError::Dns)?;
        let addr = *addrs.first().ok_or(NetError::Dns)?;
        let mut socket = TcpSocket::new(self.stack, rx, tx);
        socket.set_timeout(Some(SOCKET_TIMEOUT));
        socket
            .connect((addr, 443))
            .await
            .map_err(|_| NetError::Connect)?;
        let config = SessionConfig::Client(ClientSessionConfig {
            ca_chain: Some(self.ca.clone()),
            server_name: Some(HOST_C),
            min_version: TlsVersion::Tls1_2,
            ..ClientSessionConfig::new()
        });
        let mut session = Session::new(self.tls, socket, &config)?;
        session.connect().await?;
        Ok(session)
    }

    /// One small request with an optional JSON body. The response body
    /// (up to `out.len()`) goes to `out`; returns the status and its length.
    pub async fn exchange(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        json: Option<&str>,
        out: &mut [u8],
    ) -> Result<(u16, usize), NetError> {
        let mut rx = vec![0u8; 4096];
        let mut tx = vec![0u8; 2048];
        let mut session = self.connect(&mut rx, &mut tx).await?;
        let mut head = [0u8; 512];
        let body = json.map_or(Body::None, |j| Body::Length(j.len()));
        let ct = json.map(|_| "application/json");
        let head = http::request_head(&mut head, method, path, token, ct, body)
            .ok_or(NetError::TooLong)?;
        session.write_all(head).await?;
        if let Some(j) = json {
            session.write_all(j.as_bytes()).await?;
        }
        session.flush().await?;

        let mut buf = vec![0u8; 1024];
        let (parser, mut reader) = read_head(&mut session, &mut buf).await?;
        let mut n = 0;
        while let Some(chunk) = reader.next(&mut session, &mut buf).await? {
            let take = chunk.len().min(out.len() - n);
            out[n..n + take].copy_from_slice(&chunk[..take]);
            n += take;
        }
        let _ = session.close().await;
        Ok((parser.status, n))
    }
}

/// Reads the response head; the returned reader yields the body.
pub async fn read_head<S: Read>(
    stream: &mut S,
    buf: &mut [u8],
) -> Result<(HeadParser, BodyReader), NetError>
where
    NetError: From<S::Error>,
{
    let mut parser = HeadParser::new();
    loop {
        let n = stream.read(buf).await?;
        if n == 0 {
            return Err(NetError::Http);
        }
        let used = parser.feed(&buf[..n]).map_err(|_| NetError::Http)?;
        if parser.done {
            let reader = BodyReader {
                chunked: parser.chunked.then(ChunkedDecoder::default),
                remaining: if parser.chunked {
                    None
                } else {
                    parser.content_length
                },
                start: used,
                end: n,
                eof: false,
            };
            return Ok((parser, reader));
        }
    }
}

/// Body bytes as they arrive: chunked, `Content-Length` or until close.
pub struct BodyReader {
    chunked: Option<ChunkedDecoder>,
    remaining: Option<usize>,
    start: usize,
    end: usize,
    eof: bool,
}

impl BodyReader {
    fn finished(&self) -> bool {
        self.eof
            || self.chunked.as_ref().is_some_and(ChunkedDecoder::is_done)
            || self.remaining == Some(0)
    }

    /// The next piece of body, `None` at its end. `buf` must be the buffer
    /// passed to [`read_head`] (it holds the bytes read past the head).
    pub async fn next<'b, S: Read>(
        &mut self,
        stream: &mut S,
        buf: &'b mut [u8],
    ) -> Result<Option<&'b [u8]>, NetError>
    where
        NetError: From<S::Error>,
    {
        let range = loop {
            if self.finished() {
                return Ok(None);
            }
            if self.start == self.end {
                // A peer that closes without close_notify after a complete
                // `Connection: close` body is the end of the body.
                let n = match stream.read(buf).await {
                    Ok(n) => n,
                    Err(_) if self.chunked.is_none() && self.remaining.is_none() => 0,
                    Err(e) => return Err(e.into()),
                };
                if n == 0 {
                    if self.chunked.is_some() || self.remaining.is_some() {
                        return Err(NetError::Http);
                    }
                    self.eof = true;
                    return Ok(None);
                }
                self.start = 0;
                self.end = n;
            }
            let base = self.start;
            let input = &buf[base..self.end];
            match self.chunked.as_mut() {
                Some(d) => {
                    let step = d.step(input).map_err(|_| NetError::Http)?;
                    self.start += step.consumed;
                    if step.data.1 > step.data.0 {
                        break (base + step.data.0, base + step.data.1);
                    }
                }
                None => {
                    let take = self.remaining.map_or(input.len(), |r| r.min(input.len()));
                    if let Some(r) = self.remaining.as_mut() {
                        *r -= take;
                    }
                    self.start += take;
                    if take > 0 {
                        break (base, base + take);
                    }
                }
            }
        };
        Ok(Some(&buf[range.0..range.1]))
    }
}

/// Collects a small body into a `Vec` (error messages).
pub async fn read_small_body<S: Read>(
    stream: &mut S,
    reader: &mut BodyReader,
    buf: &mut [u8],
    max: usize,
) -> Vec<u8>
where
    NetError: From<S::Error>,
{
    let mut out = Vec::new();
    while let Ok(Some(chunk)) = reader.next(stream, buf).await {
        let take = chunk.len().min(max - out.len());
        out.extend_from_slice(&chunk[..take]);
        if out.len() == max {
            break;
        }
    }
    out
}
