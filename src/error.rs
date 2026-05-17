//! Driver error type used throughout the crate.

/// Driver-level error.
///
/// The SPI bus error is plumbed through; pin errors are collapsed to
/// [`Error::Pin`] since on a real MCU `OutputPin::set_high()` etc. never
/// actually fails — surfacing the inner type would only force the user to
/// name it.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error<SpiErr> {
    /// Underlying SPI bus error.
    Spi(SpiErr),
    /// A digital pin operation returned an error.
    Pin,
    /// Response framing did not match the protocol (missing START/END,
    /// wrong reply opcode, etc.).
    Framing,
    /// NINA returned the `ERR_CMD` (0xEF) frame.
    Nina,
    /// Polled the bus for `START_CMD` more than [`crate::proto::START_TIMEOUT_ITERS`]
    /// times without seeing it.
    Timeout,
    /// Response payload is larger than the user-supplied buffer.
    BufferTooSmall,
    /// Operation not supported by the firmware (e.g. IPv6 DNS).
    Unsupported,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Self {
        Error::Spi(e)
    }
}

impl<E: core::fmt::Debug> core::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

impl<E: core::fmt::Debug> core::error::Error for Error<E> {}

impl<E: core::fmt::Debug> embedded_io::Error for Error<E> {
    fn kind(&self) -> embedded_io::ErrorKind {
        use embedded_io::ErrorKind;
        match self {
            Error::Spi(_) => ErrorKind::Other,
            Error::Pin => ErrorKind::Other,
            Error::Framing => ErrorKind::InvalidData,
            Error::Nina => ErrorKind::Other,
            Error::Timeout => ErrorKind::TimedOut,
            Error::BufferTooSmall => ErrorKind::OutOfMemory,
            Error::Unsupported => ErrorKind::Unsupported,
        }
    }
}
