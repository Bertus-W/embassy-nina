//! Borrowed handle to a connected TCP socket.
//!
//! [`NinaTcpSocket`] is returned by [`crate::Nina::tcp_connect`]. It holds
//! a mutable borrow of the chip plus the firmware-side socket index.
//!
//! Implements [`embedded_io_async::Read`] and [`embedded_io_async::Write`],
//! so it plugs into anything in the `embedded-io-async` ecosystem
//! (`reqwless`, `rust-mqtt`, `embedded-tls`, …).

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;
use embedded_io_async::{ErrorType, Read, Write};

use crate::driver::{Nina, Socket};
use crate::error::Error;
use crate::proto::SockState;

/// Borrowed TCP socket — lives as long as the `Nina` borrow it was
/// minted from.
///
/// Reads block until data arrives or the peer closes. Writes block until
/// the firmware accepts the bytes. Drop does **not** close the socket — call
/// [`NinaTcpSocket::close`] explicitly (async cleanup can't happen in Drop).
pub struct NinaTcpSocket<'a, Bus, Cs, Ack, Rst, Boot> {
    chip: &'a mut Nina<Bus, Cs, Ack, Rst, Boot>,
    sock: Socket,
}

impl<'a, Bus, Cs, Ack, Rst, Boot, SpiErr> NinaTcpSocket<'a, Bus, Cs, Ack, Rst, Boot>
where
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
{
    pub(crate) fn new(chip: &'a mut Nina<Bus, Cs, Ack, Rst, Boot>, sock: Socket) -> Self {
        Self { chip, sock }
    }

    /// Firmware-side socket handle. Exposed for diagnostics; you should
    /// not need it in normal code.
    pub fn socket(&self) -> Socket {
        self.sock
    }

    /// Tear down the connection.
    pub async fn close(self) -> Result<(), Error<SpiErr>> {
        self.chip.tcp_close(self.sock).await
    }
}

impl<'a, Bus, Cs, Ack, Rst, Boot, SpiErr> ErrorType for NinaTcpSocket<'a, Bus, Cs, Ack, Rst, Boot>
where
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    Error<SpiErr>: embedded_io::Error,
{
    type Error = Error<SpiErr>;
}

impl<'a, Bus, Cs, Ack, Rst, Boot, SpiErr> Read for NinaTcpSocket<'a, Bus, Cs, Ack, Rst, Boot>
where
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    Error<SpiErr>: embedded_io::Error,
{
    /// Blocks until at least one byte is available or the peer closed.
    /// Returns `Ok(0)` on EOF.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            let n = self.chip.tcp_recv(self.sock, buf).await?;
            if n > 0 {
                return Ok(n);
            }
            let state = self.chip.tcp_state(self.sock).await?;
            if matches!(
                state,
                SockState::Closed
                    | SockState::CloseWait
                    | SockState::Closing
                    | SockState::LastAck
                    | SockState::TimeWait
            ) {
                return Ok(0);
            }
            embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
        }
    }
}

impl<'a, Bus, Cs, Ack, Rst, Boot, SpiErr> Write for NinaTcpSocket<'a, Bus, Cs, Ack, Rst, Boot>
where
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    Error<SpiErr>: embedded_io::Error,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let n = self.chip.tcp_send(self.sock, buf).await?;
        Ok(n as usize)
    }

    /// nina-fw buffers internally; no host-side flush is meaningful.
    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
