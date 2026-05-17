//! `embedded-nal-async` facade.
//!
//! Wraps a [`crate::Nina`] in a mutex so callers can share it across
//! multiple sockets (or just call from a `&self` context, which the
//! `embedded-nal-async` traits require). Implements:
//!
//! - [`embedded_nal_async::TcpConnect`] — drop-in for `reqwless`, etc.
//! - [`embedded_nal_async::Dns`] — IPv4 lookups via the NINA-side resolver.
//!
//! Wire it up:
//!
//! ```ignore
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use embassy_sync::mutex::Mutex;
//! use embassy_nina::{Nina, NinaStack};
//!
//! let mut chip = Nina::new(spi, cs, ack, rst, boot);
//! chip.init().await?;
//! chip.connect_wpa(b"SSID", b"PSK").await?;
//! // poll status until Connected ...
//!
//! let chip = Mutex::<NoopRawMutex, _>::new(chip);
//! let stack = NinaStack::new(&chip);
//! // `stack` is now a `TcpConnect + Dns` you can pass into reqwless.
//! ```

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};

use crate::driver::{Nina, Socket};
use crate::error::Error;
use crate::proto::{SockState, TCP_MODE, TLS_MODE};

/// `true` if `state` indicates the peer has closed (or we have).
fn is_closed(state: SockState) -> bool {
    matches!(
        state,
        SockState::Closed
            | SockState::CloseWait
            | SockState::Closing
            | SockState::LastAck
            | SockState::TimeWait
    )
}

/// `embedded-nal-async`-compatible facade around a [`Nina`].
///
/// Holds a reference to a `Mutex<Nina>` so multiple sockets can share the
/// underlying chip serially. The transport mode (plain TCP vs the NINA's
/// onboard TLS) is selected at construction time.
pub struct NinaStack<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
{
    chip: &'a Mutex<M, Nina<Bus, Cs, Ack, Rst, Boot>>,
    mode: u8,
}

impl<'a, M, Bus, Cs, Ack, Rst, Boot> NinaStack<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
{
    /// Wrap a shared, mutex-guarded [`Nina`] as a plain-TCP stack.
    pub fn new(chip: &'a Mutex<M, Nina<Bus, Cs, Ack, Rst, Boot>>) -> Self {
        Self {
            chip,
            mode: TCP_MODE,
        }
    }

    /// Wrap a shared, mutex-guarded [`Nina`] as a TLS stack — the NINA does
    /// the handshake and crypto on-chip using its baked-in CA bundle.
    /// Use with [`Self::connect_hostname`] to drive TLS to a real CDN-fronted
    /// site (SNI is required for those, and `embedded-nal-async::TcpConnect`
    /// only exposes `SocketAddr`).
    pub fn new_tls(chip: &'a Mutex<M, Nina<Bus, Cs, Ack, Rst, Boot>>) -> Self {
        Self {
            chip,
            mode: TLS_MODE,
        }
    }
}

impl<'a, M, Bus, Cs, Ack, Rst, Boot, SpiErr> NinaStack<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    Error<SpiErr>: embedded_io::Error,
{
    /// Connect by hostname rather than `SocketAddr`. The hostname is sent
    /// to the chip alongside the resolved IP so it can include the SNI
    /// extension in the TLS ClientHello — without this, modern CDN-fronted
    /// HTTPS servers either return the wrong cert or close the connection.
    ///
    /// DNS resolution happens host-side via the chip's resolver. The TCP
    /// connection itself uses the resolved IP; the hostname is only used
    /// for SNI.
    ///
    /// Timeout is 20 s — TLS handshake against typical sites takes 2-5 s
    /// on the NINA.
    pub async fn connect_hostname(
        &self,
        hostname: &str,
        port: u16,
    ) -> Result<NinaSocket<'a, M, Bus, Cs, Ack, Rst, Boot>, Error<SpiErr>> {
        let mut chip = self.chip.lock().await;
        let ip = chip.dns_lookup(hostname.as_bytes()).await?;
        let sock = chip.tcp_open_socket().await?;
        chip.tcp_start_client_hostname(sock, hostname.as_bytes(), ip, port, self.mode)
            .await?;
        for _ in 0..80 {
            if chip.tcp_state(sock).await? == SockState::Established {
                drop(chip);
                return Ok(NinaSocket {
                    chip: self.chip,
                    sock,
                });
            }
            embassy_time::Timer::after(embassy_time::Duration::from_millis(250)).await;
        }
        let _ = chip.tcp_close(sock).await;
        Err(Error::Timeout)
    }
}

/// A TCP connection minted by [`NinaStack::connect`]. Read/write operations
/// lock the underlying `Nina` briefly per call, releasing it between awaits
/// so other sockets (or DNS, etc.) can interleave.
pub struct NinaSocket<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
{
    chip: &'a Mutex<M, Nina<Bus, Cs, Ack, Rst, Boot>>,
    sock: Socket,
}

impl<'a, M, Bus, Cs, Ack, Rst, Boot, SpiErr> ErrorType
    for NinaSocket<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    Error<SpiErr>: embedded_io::Error,
{
    type Error = Error<SpiErr>;
}

impl<'a, M, Bus, Cs, Ack, Rst, Boot, SpiErr> Read for NinaSocket<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    Error<SpiErr>: embedded_io::Error,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Block until either data arrives or the peer closes. The chip's
        // recv buffer is drained one chunk per call; the caller's loop
        // pulls successive chunks until we return `Ok(0)`.
        loop {
            {
                let mut chip = self.chip.lock().await;
                let n = chip.tcp_recv(self.sock, buf).await?;
                if n > 0 {
                    return Ok(n);
                }
                // No bytes available. Only declare EOF if the socket is
                // genuinely in a close-side state; otherwise wait and retry.
                if is_closed(chip.tcp_state(self.sock).await?) {
                    return Ok(0);
                }
            }
            embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
        }
    }
}

impl<'a, M, Bus, Cs, Ack, Rst, Boot, SpiErr> Write for NinaSocket<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    Error<SpiErr>: embedded_io::Error,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut chip = self.chip.lock().await;
        let n = chip.tcp_send(self.sock, buf).await?;
        Ok(n as usize)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'a, M, Bus, Cs, Ack, Rst, Boot, SpiErr> TcpConnect
    for NinaStack<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex + 'a,
    Bus: SpiBus<u8, Error = SpiErr> + 'a,
    Cs: OutputPin + 'a,
    Ack: Wait + InputPin + 'a,
    Rst: OutputPin + 'a,
    Boot: OutputPin + 'a,
    SpiErr: core::fmt::Debug + 'a,
    Error<SpiErr>: embedded_io::Error,
{
    type Error = Error<SpiErr>;
    type Connection<'b>
        = NinaSocket<'b, M, Bus, Cs, Ack, Rst, Boot>
    where
        Self: 'b;

    async fn connect<'b>(
        &'b self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'b>, Self::Error> {
        let ip = match remote {
            SocketAddr::V4(v4) => v4.ip().octets(),
            SocketAddr::V6(_) => return Err(Error::Unsupported),
        };
        let port = remote.port();

        let mut chip = self.chip.lock().await;
        let sock = chip.tcp_open_socket().await?;
        chip.tcp_start_client(sock, ip, port, self.mode).await?;
        for _ in 0..40 {
            if chip.tcp_state(sock).await? == SockState::Established {
                drop(chip);
                return Ok(NinaSocket {
                    chip: self.chip,
                    sock,
                });
            }
            embassy_time::Timer::after(embassy_time::Duration::from_millis(250)).await;
        }
        let _ = chip.tcp_close(sock).await;
        Err(Error::Timeout)
    }
}

impl<'a, M, Bus, Cs, Ack, Rst, Boot, SpiErr> Dns for NinaStack<'a, M, Bus, Cs, Ack, Rst, Boot>
where
    M: RawMutex,
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
    SpiErr: core::fmt::Debug,
{
    type Error = Error<SpiErr>;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        if matches!(addr_type, AddrType::IPv6) {
            return Err(Error::Unsupported);
        }
        let mut chip = self.chip.lock().await;
        let ip = chip.dns_lookup(host.as_bytes()).await?;
        Ok(IpAddr::V4(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3])))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        // nina-fw has no reverse-DNS opcode.
        Err(Error::Unsupported)
    }
}
