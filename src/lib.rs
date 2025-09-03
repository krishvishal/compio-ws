use std::io::ErrorKind;

use compio::io::compat::SyncStream;
use compio_io::{AsyncRead, AsyncWrite};
use tungstenite::{
    client::IntoClientRequest,
    handshake::server::{Callback, NoCallback},
    protocol::{CloseFrame, Role, WebSocketConfig},
    Error as WsError, HandshakeError, Message, WebSocket,
};

pub struct WebSocketStream<S> {
    inner: WebSocket<SyncStream<S>>,
}

#[derive(Debug, Clone, Copy)]
enum BufferOperation {
    /// Try to fill read buffer first (for read operations)
    FillFirst,
    /// Try to flush write buffer first (for write operations)
    FlushFirst,
    /// Use the standard pattern: flush first, then fill if nothing was flushed
    Standard,
}

impl<S> WebSocketStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug,
{
    fn new(stream: S, role: Role) -> Self {
        let sync_stream = SyncStream::new(stream);
        Self {
            inner: WebSocket::from_raw_socket(sync_stream, role, None),
        }
    }

    fn with_config(stream: S, role: Role, config: Option<WebSocketConfig>) -> Self {
        let sync_stream = SyncStream::new(stream);
        Self {
            inner: WebSocket::from_raw_socket(sync_stream, role, config),
        }
    }

    async fn handle_would_block<T, F>(
        &mut self,
        mut operation: F,
        buffer_hint: BufferOperation,
    ) -> Result<T, WsError>
    where
        F: FnMut(&mut WebSocket<SyncStream<S>>) -> Result<T, WsError>,
    {
        loop {
            match operation(&mut self.inner) {
                Ok(result) => return Ok(result),
                Err(WsError::Io(ref e)) if e.kind() == ErrorKind::WouldBlock => {
                    let sync_stream = self.inner.get_mut();

                    match buffer_hint {
                        BufferOperation::FillFirst => {
                            // For read operations: try fill first, then flush if needed
                            match sync_stream.fill_read_buf().await {
                                Ok(_) => continue, // Successfully filled, retry operation
                                Err(_) => {
                                    // Fill failed or nothing to fill, try flush
                                    sync_stream
                                        .flush_write_buf()
                                        .await
                                        .map_err(|e| WsError::Io(e))?;
                                }
                            }
                        }
                        BufferOperation::FlushFirst => {
                            // For write operations: try flush first, then fill if needed
                            match sync_stream.flush_write_buf().await {
                                Ok(0) => {
                                    // Nothing was flushed, try fill
                                    sync_stream
                                        .fill_read_buf()
                                        .await
                                        .map_err(|e| WsError::Io(e))?;
                                }
                                Ok(_) => {
                                    // Successfully flushed something, continue
                                }
                                Err(_) => {
                                    // Flush failed, try fill as fallback
                                    sync_stream
                                        .fill_read_buf()
                                        .await
                                        .map_err(|e| WsError::Io(e))?;
                                }
                            }
                        }
                        BufferOperation::Standard => {
                            if sync_stream
                                .flush_write_buf()
                                .await
                                .map_err(|e| WsError::Io(e))?
                                == 0
                            {
                                sync_stream
                                    .fill_read_buf()
                                    .await
                                    .map_err(|e| WsError::Io(e))?;
                            }
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn send(&mut self, message: Message) -> Result<(), WsError> {
        // Send the message using optimized buffer handling
        let result = self
            .handle_would_block(|ws| ws.send(message.clone()), BufferOperation::FlushFirst)
            .await?;

        // CRITICAL: Ensure the message is actually sent over the network
        let flushed = self
            .inner
            .get_mut()
            .flush_write_buf()
            .await
            .map_err(|e| WsError::Io(e))?;
        println!("[SEND] Flushed {} bytes to network", flushed);

        Ok(result)
    }

    pub async fn read(&mut self) -> Result<Message, WsError> {
        self.handle_would_block(|ws| ws.read(), BufferOperation::FillFirst)
            .await
    }

    pub async fn close(&mut self, close_frame: Option<CloseFrame>) -> Result<(), WsError> {
        self.handle_would_block(
            |ws| ws.close(close_frame.clone()),
            BufferOperation::Standard,
        )
        .await
    }

    pub fn get_ref(&self) -> &S {
        self.inner.get_ref().get_ref()
    }

    pub fn get_mut(&mut self) -> &mut S {
        self.inner.get_mut().get_mut()
    }
}

pub async fn accept_async<S>(stream: S) -> Result<WebSocketStream<S>, WsError>
where
    S: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug,
{
    accept_hdr_async(stream, NoCallback).await
}

pub async fn accept_hdr_async<S, C>(stream: S, callback: C) -> Result<WebSocketStream<S>, WsError>
where
    S: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug,
    C: Callback,
{
    accept_hdr_with_config_async(stream, callback, None).await
}

pub async fn accept_with_config_async<S>(
    stream: S,
    config: Option<WebSocketConfig>,
) -> Result<WebSocketStream<S>, WsError>
where
    S: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug,
{
    accept_hdr_with_config_async(stream, NoCallback, config).await
}

pub async fn accept_hdr_with_config_async<S, C>(
    stream: S,
    callback: C,
    config: Option<WebSocketConfig>,
) -> Result<WebSocketStream<S>, WsError>
where
    S: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug,
    C: Callback,
{
    let sync_stream = SyncStream::new(stream);
    let mut handshake_result = tungstenite::accept_hdr_with_config(sync_stream, callback, config);

    loop {
        match handshake_result {
            Ok(mut websocket) => {
                // Ensure any remaining data is flushed
                let _ = websocket
                    .get_mut()
                    .flush_write_buf()
                    .await
                    .map_err(|e| WsError::Io(e))?;
                return Ok(WebSocketStream { inner: websocket });
            }
            Err(HandshakeError::Interrupted(mut mid_handshake)) => {
                let sync_stream = mid_handshake.get_mut().get_mut();

                // For handshake: always try both operations
                let _ = sync_stream
                    .flush_write_buf()
                    .await
                    .map_err(|e| WsError::Io(e))?;

                let _ = sync_stream
                    .fill_read_buf()
                    .await
                    .map_err(|e| WsError::Io(e))?;

                handshake_result = mid_handshake.handshake();
            }
            Err(HandshakeError::Failure(error)) => {
                return Err(error);
            }
        }
    }
}

pub async fn client_async<R, S>(
    request: R,
    stream: S,
) -> Result<(WebSocketStream<S>, tungstenite::handshake::client::Response), WsError>
where
    R: IntoClientRequest,
    S: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug,
{
    client_with_config_async(request, stream, None).await
}

pub async fn client_with_config_async<R, S>(
    request: R,
    stream: S,
    config: Option<WebSocketConfig>,
) -> Result<(WebSocketStream<S>, tungstenite::handshake::client::Response), WsError>
where
    R: IntoClientRequest,
    S: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug,
{
    let sync_stream = SyncStream::new(stream);
    let mut handshake_result =
        tungstenite::client::client_with_config(request, sync_stream, config);

    loop {
        match handshake_result {
            Ok((mut websocket, response)) => {
                // Ensure any remaining data is flushed
                let _ = websocket
                    .get_mut()
                    .flush_write_buf()
                    .await
                    .map_err(|e| WsError::Io(e))?;
                return Ok((WebSocketStream { inner: websocket }, response));
            }
            Err(HandshakeError::Interrupted(mut mid_handshake)) => {
                let sync_stream = mid_handshake.get_mut().get_mut();

                // For handshake: always try both operations
                let _ = sync_stream
                    .flush_write_buf()
                    .await
                    .map_err(|e| WsError::Io(e))?;

                let _ = sync_stream
                    .fill_read_buf()
                    .await
                    .map_err(|e| WsError::Io(e))?;

                handshake_result = mid_handshake.handshake();
            }
            Err(HandshakeError::Failure(error)) => {
                return Err(error);
            }
        }
    }
}
