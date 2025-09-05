use compio_buf::{BufResult, IoBuf, IoBufMut};
use compio_io::{AsyncRead, AsyncWrite};
use compio_tls::TlsStream;
use std::io::Result as IoResult;

/// Stream that can be either plain TCP or TLS-encrypted
#[derive(Debug)]
pub enum MaybeTlsStream<S> {
    /// Plain, unencrypted stream
    Plain(S),
    /// TLS-encrypted stream
    Tls(TlsStream<S>),
}

impl<S> MaybeTlsStream<S> {
    pub fn plain(stream: S) -> Self {
        MaybeTlsStream::Plain(stream)
    }

    pub fn tls(stream: TlsStream<S>) -> Self {
        MaybeTlsStream::Tls(stream)
    }

    pub fn is_tls(&self) -> bool {
        matches!(self, MaybeTlsStream::Tls(_))
    }
}

impl<S> AsyncRead for MaybeTlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        match self {
            MaybeTlsStream::Plain(stream) => stream.read(buf).await,
            MaybeTlsStream::Tls(stream) => stream.read(buf).await,
        }
    }
}

impl<S> AsyncWrite for MaybeTlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        match self {
            MaybeTlsStream::Plain(stream) => stream.write(buf).await,
            MaybeTlsStream::Tls(stream) => stream.write(buf).await,
        }
    }

    async fn flush(&mut self) -> IoResult<()> {
        match self {
            MaybeTlsStream::Plain(stream) => stream.flush().await,
            MaybeTlsStream::Tls(stream) => stream.flush().await,
        }
    }

    async fn shutdown(&mut self) -> IoResult<()> {
        match self {
            MaybeTlsStream::Plain(stream) => stream.shutdown().await,
            MaybeTlsStream::Tls(stream) => stream.shutdown().await,
        }
    }
}

impl<S> Unpin for MaybeTlsStream<S> where S: Unpin {}
