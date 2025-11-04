use compio_io::{AsyncRead, AsyncWrite};
use compio_net::TcpStream;
use compio_tls::TlsConnector;
use rustls::{ClientConfig, RootCertStore};

use tungstenite::client::{uri_mode, IntoClientRequest};
use tungstenite::handshake::client::{Request, Response};
use tungstenite::stream::Mode;
use tungstenite::Error;

use std::sync::Arc;

use crate::stream::MaybeTlsStream;
use crate::{client_async_with_config, domain, WebSocketConfig, WebSocketStream};

pub type AutoStream<S> = MaybeTlsStream<S>;

pub type Connector = TlsConnector;

async fn wrap_stream<S>(
    socket: S,
    domain: String,
    connector: Option<Connector>,
    mode: Mode,
) -> Result<AutoStream<S>, Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match mode {
        Mode::Plain => Ok(MaybeTlsStream::Plain(socket)),
        Mode::Tls => {
            let stream = {
                let connector = if let Some(connector) = connector {
                    connector
                } else {
                    let mut root_store = RootCertStore::empty();

                    #[cfg(feature = "rustls-native-certs")]
                    {
                        match rustls_native_certs::load_native_certs() {
                            Ok(certs) => {
                                let (added, ignored) = root_store.add_parsable_certificates(certs);
                                log::debug!(
                                    "Added {added} native root certificates (ignored {ignored})"
                                );
                            }
                            Err(e) => {
                                return Err(Error::Io(std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    e,
                                )));
                            }
                        }
                    }

                    #[cfg(all(feature = "webpki-roots", not(feature = "rustls-native-certs")))]
                    {
                        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                    }

                    TlsConnector::from(Arc::new(
                        ClientConfig::builder()
                            .with_root_certificates(root_store)
                            .with_no_client_auth(),
                    ))
                };

                connector
                    .connect(&domain, socket)
                    .await
                    .map_err(Error::Io)?
            };
            Ok(MaybeTlsStream::Tls(stream))
        }
    }
}

pub async fn client_async_tls_with_connector_and_config<R, S>(
    request: R,
    stream: S,
    connector: Option<Connector>,
    config: Option<WebSocketConfig>,
) -> Result<(WebSocketStream<AutoStream<S>>, Response), Error>
where
    R: IntoClientRequest + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
    AutoStream<S>: Unpin,
{
    let request: Request = request.into_client_request()?;

    let domain = domain(&request)?;

    let mode = uri_mode(request.uri())?;

    let stream = wrap_stream(stream, domain, connector, mode).await?;
    client_async_with_config(request, stream, config).await
}

pub async fn client_async_tls<R, S>(
    request: R,
    stream: S,
) -> Result<(WebSocketStream<AutoStream<S>>, Response), Error>
where
    R: IntoClientRequest + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
    AutoStream<S>: Unpin,
{
    client_async_tls_with_connector_and_config(request, stream, None, None).await
}

pub async fn client_async_tls_with_config<R, S>(
    request: R,
    stream: S,
    config: Option<WebSocketConfig>,
) -> Result<(WebSocketStream<AutoStream<S>>, Response), Error>
where
    R: IntoClientRequest + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
    AutoStream<S>: Unpin,
{
    client_async_tls_with_connector_and_config(request, stream, None, config).await
}

pub async fn client_async_tls_with_connector<R, S>(
    request: R,
    stream: S,
    connector: Option<Connector>,
) -> Result<(WebSocketStream<AutoStream<S>>, Response), Error>
where
    R: IntoClientRequest + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
    AutoStream<S>: Unpin,
{
    client_async_tls_with_connector_and_config(request, stream, connector, None).await
}

pub type ConnectStream = AutoStream<TcpStream>;

pub async fn connect_async<R>(
    request: R,
) -> Result<(WebSocketStream<ConnectStream>, Response), Error>
where
    R: IntoClientRequest + Unpin,
{
    connect_async_with_config(request, None, false).await
}

pub async fn connect_async_with_config<R>(
    request: R,
    config: Option<WebSocketConfig>,
    disable_nagle: bool,
) -> Result<(WebSocketStream<ConnectStream>, Response), Error>
where
    R: IntoClientRequest + Unpin,
{
    let request: Request = request.into_client_request()?;

    let domain = domain(&request)?;
    let port = port(&request)?;

    let socket = TcpStream::connect((domain.as_str(), port))
        .await
        .map_err(Error::Io)?;

    if disable_nagle {
        socket.set_nodelay(true).map_err(Error::Io)?;
    }

    client_async_tls_with_connector_and_config(request, socket, None, config).await
}

pub async fn connect_async_with_tls_connector<R>(
    request: R,
    connector: Option<Connector>,
) -> Result<(WebSocketStream<ConnectStream>, Response), Error>
where
    R: IntoClientRequest + Unpin,
{
    connect_async_with_tls_connector_and_config(request, connector, None).await
}

pub async fn connect_async_with_tls_connector_and_config<R>(
    request: R,
    connector: Option<Connector>,
    config: Option<WebSocketConfig>,
) -> Result<(WebSocketStream<ConnectStream>, Response), Error>
where
    R: IntoClientRequest + Unpin,
{
    let request: Request = request.into_client_request()?;

    let domain = domain(&request)?;
    let port = port(&request)?;

    let socket = TcpStream::connect((domain.as_str(), port))
        .await
        .map_err(Error::Io)?;
    client_async_tls_with_connector_and_config(request, socket, connector, config).await
}

#[inline]
#[allow(clippy::result_large_err)]
fn port(request: &Request) -> Result<u16, Error> {
    request
        .uri()
        .port_u16()
        .or_else(|| match uri_mode(request.uri()).ok()? {
            Mode::Plain => Some(80),
            Mode::Tls => Some(443),
        })
        .ok_or(Error::Url(
            tungstenite::error::UrlError::UnsupportedUrlScheme,
        ))
}
