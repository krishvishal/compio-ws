use std::io::{self, Read, Write};

use compio_buf::{BufResult, IntoInner, IoBuf};
use compio_io::{AsyncRead, AsyncWrite};

#[derive(Debug)]
pub struct CompioStream<S> {
    inner: S,
    read_buf: Vec<u8>,
    read_pos: usize,
    write_buf: Vec<u8>,
    eof: bool,
    base_capacity: usize,
    max_buffer_size: usize,
}

impl<S> CompioStream<S> {
    const DEFAULT_BASE_CAPACITY: usize = 8 * 1024; // 8KB base
    const DEFAULT_MAX_BUFFER: usize = 64 * 1024 * 1024; // 64MB max

    pub fn new(stream: S) -> Self {
        Self::with_capacity(Self::DEFAULT_BASE_CAPACITY, stream)
    }

    pub fn with_capacity(base_capacity: usize, stream: S) -> Self {
        Self {
            inner: stream,
            read_buf: Vec::with_capacity(base_capacity),
            read_pos: 0,
            write_buf: Vec::with_capacity(base_capacity),
            eof: false,
            base_capacity,
            max_buffer_size: Self::DEFAULT_MAX_BUFFER,
        }
    }

    pub fn with_limits(base_capacity: usize, max_buffer_size: usize, stream: S) -> Self {
        Self {
            inner: stream,
            read_buf: Vec::with_capacity(base_capacity),
            read_pos: 0,
            write_buf: Vec::with_capacity(base_capacity),
            eof: false,
            base_capacity,
            max_buffer_size,
        }
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    pub fn into_inner(self) -> S {
        self.inner
    }

    pub fn is_eof(&self) -> bool {
        self.eof
    }

    fn available_read(&self) -> &[u8] {
        &self.read_buf[self.read_pos..]
    }

    fn consume_read(&mut self, amt: usize) {
        self.read_pos += amt;

        if self.read_pos >= self.read_buf.len() {
            self.read_pos = 0;

            if self.read_buf.capacity() > self.base_capacity * 4 {
                self.read_buf = Vec::with_capacity(self.base_capacity);
            } else {
                self.read_buf.clear();
            }
        }
    }
}

impl<S> Read for CompioStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.available_read();

        if available.is_empty() && !self.eof {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "need to fill read buffer",
            ));
        }

        let to_read = available.len().min(buf.len());
        buf[..to_read].copy_from_slice(&available[..to_read]);
        self.consume_read(to_read);

        Ok(to_read)
    }
}

impl<S> Write for CompioStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Check if we should flush first
        if self.write_buf.len() > self.base_capacity * 2 / 3 && !self.write_buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "need to flush write buffer",
            ));
        }

        if self.write_buf.len() + buf.len() > self.max_buffer_size {
            let space = self.max_buffer_size - self.write_buf.len();
            if space == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "write buffer full, need to flush",
                ));
            }
            self.write_buf.extend_from_slice(&buf[..space]);
            return Ok(space);
        }

        self.write_buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // For tungstenite compatibility: return Ok even if there's buffered data
        // The actual async flush happens when flush_write_buf() is called
        // This prevents tungstenite's send() from failing after successfully buffering
        Ok(())
    }
}

impl<S: AsyncRead> CompioStream<S> {
    pub async fn fill_read_buf(&mut self) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }

        if self.read_pos > 0 && self.read_pos < self.read_buf.len() {
            let buf_len = self.read_buf.len();
            let remaining = buf_len - self.read_pos;
            self.read_buf.copy_within(self.read_pos..buf_len, 0);
            unsafe {
                self.read_buf.set_len(remaining);
            }
            self.read_pos = 0;
        } else if self.read_pos >= self.read_buf.len() {
            self.read_pos = 0;
            if self.read_buf.capacity() > self.base_capacity * 4 {
                self.read_buf = Vec::with_capacity(self.base_capacity);
            } else {
                self.read_buf.clear();
            }
        }

        let current_len = self.read_buf.len();
        let capacity = self.read_buf.capacity();
        let available_space = capacity - current_len;

        let target_space = self.base_capacity;
        if available_space < target_space {
            let new_capacity = current_len + target_space;
            self.read_buf.reserve(target_space);
        }

        let capacity = self.read_buf.capacity();
        let len = self.read_buf.len();

        unsafe {
            self.read_buf.set_len(capacity);
        }

        let buf = std::mem::take(&mut self.read_buf);

        let read_slice = IoBuf::slice(buf, len..);

        let BufResult(result, mut buf) = self.inner.read(read_slice).await.into_inner();

        match result {
            Ok(n) => {
                if n == 0 {
                    self.eof = true;
                    unsafe {
                        buf.set_len(len);
                    }
                } else {
                    unsafe {
                        buf.set_len(len + n);
                    }
                }
                self.read_buf = buf;
                Ok(n)
            }
            Err(e) => {
                unsafe {
                    buf.set_len(len);
                }
                self.read_buf = buf;
                Err(e)
            }
        }
    }
}

impl<S: AsyncWrite> CompioStream<S> {
    pub async fn flush_write_buf(&mut self) -> io::Result<usize> {
        if self.write_buf.is_empty() {
            return Ok(0);
        }

        let total = self.write_buf.len();
        let mut buf = std::mem::take(&mut self.write_buf);
        let mut flushed = 0;

        while flushed < total {
            let write_slice = IoBuf::slice(buf, flushed..);

            let BufResult(result, returned_buf) = self.inner.write(write_slice).await.into_inner();
            buf = returned_buf;

            match result {
                Ok(0) => {
                    self.write_buf = buf[flushed..].to_vec();
                    return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0"));
                }
                Ok(n) => {
                    flushed += n;
                }
                Err(e) => {
                    self.write_buf = buf[flushed..].to_vec();
                    return Err(e);
                }
            }
        }

        self.write_buf = Vec::with_capacity(self.base_capacity);

        self.inner.flush().await?;

        Ok(flushed)
    }
}
