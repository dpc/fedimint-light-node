use std::io::{self, Write};

/// Allocation-free serialization budget, rejecting a write before overrun.
pub(crate) struct EncodedSizeLimit {
    /// Bytes still allowed by the effective serialization limit.
    remaining: usize,
}

impl EncodedSizeLimit {
    /// Create a sink that accepts at most the specified number of bytes.
    pub(crate) fn new(remaining: usize) -> Self {
        Self { remaining }
    }
}

impl Write for EncodedSizeLimit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("block serialization limit exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
