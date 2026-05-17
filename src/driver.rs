//! Core driver: [`Nina`] struct, reset sequence, and the low-level
//! `send_cmd` / `recv_cmd` framing.

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;

use crate::error::Error;
use crate::proto::*;
use crate::scan::Scan;

pub use crate::proto::{Param, SockState, WlStatus};

/// Type-safe handle to a chip-side socket slot.
///
/// Returned by [`Nina::tcp_open_socket`] and [`Nina::tcp_accept`]. Pass it
/// into [`Nina::tcp_send`], [`Nina::tcp_recv`], [`Nina::tcp_close`], etc.
/// The wrapper just keeps you from accidentally passing the wrong `u8` —
/// it does **not** auto-close on `Drop` (close is async and the chip
/// expects an explicit `tcp_close`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Socket(pub(crate) u8);

impl Socket {
    /// Raw chip-side socket index. Mostly for diagnostics.
    pub const fn raw(&self) -> u8 {
        self.0
    }
}

impl core::fmt::Display for Socket {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// IPv4 configuration returned by [`Nina::ip_config`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct IpConfig {
    /// Address assigned to the STA interface.
    pub ip: [u8; 4],
    /// Subnet mask.
    pub subnet: [u8; 4],
    /// Default gateway.
    pub gateway: [u8; 4],
}

/// WiFiNINA driver bound to a SPI bus + control pins.
///
/// Pin mapping for the Arduino Nano RP2040 Connect (RP2040 SPI1):
///
/// | Driver field | NINA signal      | RP2040 GP | Note                       |
/// |--------------|------------------|-----------|----------------------------|
/// | `bus` MOSI   | NINA_MOSI        | GP11      | SPI1 TX                    |
/// | `bus` MISO   | NINA_MISO        | GP8       | SPI1 RX                    |
/// | `bus` SCK    | NINA_SCK         | GP14      | SPI1 SCK (alternate pin)   |
/// | `cs`         | NINA_CS          | GP9       | Software-driven, idle HIGH |
/// | `ack`        | SPIWIFI_ACK      | GP10      | LOW = ready, HIGH = busy   |
/// | `rst`        | NINA_RESETN      | GP3       | Active-low                 |
/// | `boot`       | NINA_GPIO0       | GP2       | HIGH at reset → normal     |
pub struct Nina<Bus, Cs, Ack, Rst, Boot> {
    bus: Bus,
    cs: Cs,
    ack: Ack,
    rst: Rst,
    boot: Boot,
}

impl<Bus, Cs, Ack, Rst, Boot, SpiErr> Nina<Bus, Cs, Ack, Rst, Boot>
where
    Bus: SpiBus<u8, Error = SpiErr>,
    Cs: OutputPin,
    Ack: Wait + InputPin,
    Rst: OutputPin,
    Boot: OutputPin,
{
    /// Construct a driver from a configured SPI bus and the four control
    /// pins. Does not touch the bus or pins; call [`Self::init`] to perform
    /// the hardware reset sequence.
    pub fn new(bus: Bus, cs: Cs, ack: Ack, rst: Rst, boot: Boot) -> Self {
        Self {
            bus,
            cs,
            ack,
            rst,
            boot,
        }
    }

    /// Hardware reset + boot + flush retained WiFi state.
    ///
    /// 1. Drive GPIO0 HIGH (selects normal flash boot, not download mode).
    /// 2. Drive CS HIGH (SPI idle).
    /// 3. Drive RESETN LOW for [`RESET_LOW_MS`].
    /// 4. Drive RESETN HIGH and wait [`RESET_BOOT_MS`] for nina-fw to boot.
    /// 5. Drive GPIO0 LOW. The Arduino reference flips this pin to Hi-Z
    ///    input post-boot; an [`OutputPin`] can't go Hi-Z, but driving LOW
    ///    is equivalent for nina-fw 2.x's purposes (and required — keeping
    ///    GPIO0 HIGH after boot wedges the chip into a half-state where
    ///    `SET_PASSPHRASE` ack's success but the radio never associates).
    /// 6. Issue a chip-side `disconnect` to drain any retained association
    ///    state. Without this, a host MCU re-flash with NINA still powered
    ///    leaves the WiFi stack in a state where subsequent connects ack
    ///    success but stay `Disconnected` forever. We swallow the result
    ///    because the chip can also be in a perfectly clean state where
    ///    disconnect is a no-op.
    pub async fn init(&mut self) -> Result<(), Error<SpiErr>> {
        self.boot.set_high().map_err(|_| Error::Pin)?;
        self.cs.set_high().map_err(|_| Error::Pin)?;
        self.rst.set_low().map_err(|_| Error::Pin)?;
        Timer::after(Duration::from_millis(RESET_LOW_MS)).await;
        self.rst.set_high().map_err(|_| Error::Pin)?;
        Timer::after(Duration::from_millis(RESET_BOOT_MS)).await;
        self.boot.set_low().map_err(|_| Error::Pin)?;
        // Triple-disconnect to flush retained WiFi state, spaced enough for
        // nina-fw to actually process each call. Cheap (~1 s) and means
        // callers don't need to do this dance themselves before every
        // `connect_wpa`.
        for _ in 0..3 {
            let _ = self.disconnect().await;
            Timer::after(Duration::from_millis(300)).await;
        }
        Ok(())
    }

    /// Get the firmware version string baked into the NINA-side image
    /// (`CommandHandler::handleGetFwVersion`, returns `FIRMWARE_VERSION`
    /// e.g. `"1.4.8\0"`). Up to 16 bytes are read; trailing NUL stripped.
    pub async fn get_fw_version<'a>(
        &mut self,
        buf: &'a mut [u8],
    ) -> Result<&'a [u8], Error<SpiErr>> {
        self.send_cmd_no_params(CMD_GET_FW_VERSION).await?;
        let n = self.recv_cmd_one_param(CMD_GET_FW_VERSION, buf).await?;
        // strip NUL terminator if present
        let slice = &buf[..n];
        let trimmed = match slice.iter().position(|&b| b == 0) {
            Some(i) => &slice[..i],
            None => slice,
        };
        Ok(trimmed)
    }

    /// Trigger a Wi-Fi scan and return the discovered SSIDs.
    ///
    /// `scratch` must be large enough to hold the response payload (one
    /// length byte + SSID bytes per network). 512 bytes covers ~15 typical
    /// networks. `settle_ms` is how long to wait between firing
    /// `START_SCAN_NETWORKS` and pulling results — the NINA scans
    /// asynchronously and 5-6 s is needed for a full sweep.
    pub async fn scan_networks<'a>(
        &mut self,
        scratch: &'a mut [u8],
        settle_ms: u64,
    ) -> Result<Scan<'a>, Error<SpiErr>> {
        // Kick the scan off. Response is a single 1-byte ack.
        self.send_cmd_no_params(CMD_START_SCAN_NETWORKS).await?;
        let mut ack = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_START_SCAN_NETWORKS, &mut ack)
            .await?;

        embassy_time::Timer::after(embassy_time::Duration::from_millis(settle_ms)).await;

        // Pull the result list.
        self.send_cmd_no_params(CMD_SCAN_NETWORKS).await?;
        let (n, written) = self.recv_cmd_into(CMD_SCAN_NETWORKS, scratch).await?;
        Ok(Scan {
            buf: &scratch[..written],
            n,
        })
    }

    /// One-shot WPA2-PSK connect with sensible retry behaviour: up to 5
    /// `connect_wpa` attempts with a 20 s status wait + 2 s settle each.
    /// Returns `Ok(())` on association or `Err(Error::Timeout)` after the
    /// last attempt. Use [`Self::connect_wpa`] + [`Self::wait_for_connected`]
    /// directly if you want different timing.
    pub async fn connect(&mut self, ssid: &str, psk: &str) -> Result<(), Error<SpiErr>> {
        for _ in 0..5 {
            let _ = self.connect_wpa(ssid.as_bytes(), psk.as_bytes()).await;
            if self
                .wait_for_connected(Duration::from_secs(20))
                .await
                .is_ok()
            {
                return Ok(());
            }
            let _ = self.disconnect().await;
            Timer::after(Duration::from_secs(2)).await;
        }
        Err(Error::Timeout)
    }

    /// Poll [`Self::status`] until it reports [`WlStatus::Connected`] or
    /// the deadline passes. Returns `Ok(())` on connect, or `Err(Error::Timeout)`
    /// if the radio didn't associate within `timeout`. Saves callers from
    /// hand-writing the poll loop after every [`Self::connect_wpa`].
    pub async fn wait_for_connected(&mut self, timeout: Duration) -> Result<(), Error<SpiErr>> {
        let deadline = embassy_time::Instant::now() + timeout;
        while embassy_time::Instant::now() < deadline {
            if let Ok(WlStatus::Connected) = self.status().await {
                return Ok(());
            }
            Timer::after(Duration::from_millis(250)).await;
        }
        Err(Error::Timeout)
    }

    /// Connect to a WPA2-PSK network. Returns the chip's ack byte (1 on
    /// accept, 0xFF on firmware-side failure). The actual association is
    /// asynchronous; use [`Self::wait_for_connected`] (or poll
    /// [`Self::status`] yourself) for [`WlStatus::Connected`].
    pub async fn connect_wpa(
        &mut self,
        ssid: &[u8],
        passphrase: &[u8],
    ) -> Result<u8, Error<SpiErr>> {
        self.send_cmd(CMD_SET_PASSPHRASE, &[ssid, passphrase])
            .await?;
        let mut ack = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_SET_PASSPHRASE, &mut ack)
            .await?;
        Ok(ack[0])
    }

    /// Read the NINA's STA MAC address (6 bytes). Confirms the radio is
    /// alive and reachable independent of any join attempt.
    pub async fn mac_addr(&mut self) -> Result<[u8; 6], Error<SpiErr>> {
        self.send_cmd(CMD_GET_MAC_ADDR, &[&[DUMMY]]).await?;
        let mut buf = [0u8; 8];
        let n = self.recv_cmd_one_param(CMD_GET_MAC_ADDR, &mut buf).await?;
        if n != 6 {
            return Err(Error::Framing);
        }
        // Arduino returns MAC reversed (LSB-first); swap to canonical order.
        Ok([buf[5], buf[4], buf[3], buf[2], buf[1], buf[0]])
    }

    /// SSID the chip is currently associated with (or attempting). May be
    /// empty if no association is in flight.
    pub async fn current_ssid<'a>(&mut self, buf: &'a mut [u8]) -> Result<&'a [u8], Error<SpiErr>> {
        self.send_cmd(CMD_GET_CURR_SSID, &[&[DUMMY]]).await?;
        let n = self.recv_cmd_one_param(CMD_GET_CURR_SSID, buf).await?;
        Ok(&buf[..n])
    }

    /// RSSI of the scan result at `index` (i8 dBm, e.g. -50).
    pub async fn scan_rssi(&mut self, index: u8) -> Result<i8, Error<SpiErr>> {
        self.send_cmd(CMD_GET_IDX_RSSI, &[&[index]]).await?;
        let mut buf = [0u8; 4];
        let n = self.recv_cmd_one_param(CMD_GET_IDX_RSSI, &mut buf).await?;
        if n != 4 {
            return Err(Error::Framing);
        }
        // 32-bit little-endian signed value
        let v = i32::from_le_bytes(buf);
        Ok(v as i8)
    }

    /// Encryption type of the scan result at `index`.
    pub async fn scan_enct(&mut self, index: u8) -> Result<EncType, Error<SpiErr>> {
        self.send_cmd(CMD_GET_IDX_ENCT, &[&[index]]).await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_GET_IDX_ENCT, &mut buf).await?;
        Ok(EncType::from_u8(buf[0]))
    }

    /// Bundle all per-scan-index info into one call. Calls
    /// `scan_rssi` + `scan_enct` + `scan_channel` + `scan_bssid` in
    /// sequence. Use it after [`Self::scan_networks`] to enrich each
    /// SSID with RSSI / channel / encryption / BSSID without remembering
    /// to call four separate methods.
    pub async fn network_info(
        &mut self,
        index: u8,
    ) -> Result<crate::scan::NetworkInfo, Error<SpiErr>> {
        let rssi = self.scan_rssi(index).await?;
        let enct = self.scan_enct(index).await?;
        let channel = self.scan_channel(index).await?;
        let bssid = self.scan_bssid(index).await?;
        Ok(crate::scan::NetworkInfo {
            rssi,
            enct,
            channel,
            bssid,
        })
    }

    /// Channel of the scan result at `index`.
    pub async fn scan_channel(&mut self, index: u8) -> Result<u8, Error<SpiErr>> {
        self.send_cmd(CMD_GET_IDX_CHANNEL, &[&[index]]).await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_GET_IDX_CHANNEL, &mut buf)
            .await?;
        Ok(buf[0])
    }

    // ============== Server-side TCP (listen / accept) =======================
    //
    // Verified working on nina-fw 2.0.0 and 3.0.1 for sustained sequential
    // connections — bindings match Arduino's `WiFiServer`/`WiFiClient`
    // implementation (`Arduino_SpiNINA`).
    //
    // Caveat: [`Self::tcp_state`] poisons any slot that isn't currently
    // `connected()` — nina-fw's `getClientStateTcp` sets
    // `socketTypes[socket] = 255` (free) when its connection check
    // fails. So don't poll state on a freshly-accepted server-side slot;
    // a short fixed wait after [`Self::tcp_accept`] is enough.

    /// Bind a socket as a TCP listener on `port`. Mirrors Arduino's
    /// `WiFiServer::begin()`. Use [`Self::tcp_accept`] to pick up client
    /// connections.
    pub async fn tcp_listen(&mut self, sock: Socket, port: u16) -> Result<(), Error<SpiErr>> {
        // Per Arduino_SpiNINA's `sendParam(uint16_t)`: 8-bit length prefix
        // = 2, then HIGH byte first, LOW byte second (BIG-ENDIAN).
        let port_be = port.to_be_bytes();
        self.send_cmd(CMD_START_SERVER_TCP, &[&port_be, &[sock.0], &[TCP_MODE]])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_START_SERVER_TCP, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Poll a listening socket for a freshly-connected client. Returns
    /// the client's socket index, or `None` if no client has connected
    /// yet. The returned socket is usable with the regular
    /// `tcp_recv`/`tcp_send`/`tcp_close` methods.
    ///
    /// Wire detail (per `Arduino_SpiNINA::ServerDrv::availServer`):
    /// `AVAIL_DATA_TCP` (0x2B) with TWO params — `sock` and an `accept`
    /// flag — returns a single 16-bit LE param: the new client's socket
    /// index, or 0x00FF when no client is pending.
    pub async fn tcp_accept(
        &mut self,
        listen_sock: Socket,
    ) -> Result<Option<Socket>, Error<SpiErr>> {
        // accept=0 ⇒ peek for the pending client without committing.
        // Mirrors Arduino's `WiFiServer::available()` default. Caller
        // should check the returned socket's state == Established before
        // using it (the chip may return the same sock index repeatedly
        // until the handshake is actually complete).
        self.send_cmd(CMD_AVAIL_DATA_TCP, &[&[listen_sock.0], &[0u8]])
            .await?;
        let mut buf = [0u8; 4];
        let n = self
            .recv_cmd_one_param(CMD_AVAIL_DATA_TCP, &mut buf)
            .await?;
        let raw = match n {
            1 => buf[0] as u16,
            2 => u16::from_le_bytes([buf[0], buf[1]]),
            _ => return Err(Error::Framing),
        };
        if raw == 0x00FF || raw == 0xFFFF {
            Ok(None)
        } else {
            Ok(Some(Socket(raw as u8)))
        }
    }

    // ============== Misc chip info / configuration ==========================

    /// Set the DHCP hostname the chip advertises. Most routers will then
    /// show this as the device name in the client list.
    pub async fn set_hostname(&mut self, name: &str) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_SET_HOSTNAME, &[name.as_bytes()]).await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_SET_HOSTNAME, &mut buf).await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Set a static IPv4 config (disables DHCP). Call **before**
    /// [`Self::connect_wpa`].
    pub async fn set_net_config(
        &mut self,
        ip: [u8; 4],
        gateway: [u8; 4],
        subnet: [u8; 4],
    ) -> Result<(), Error<SpiErr>> {
        // wifi_drv.cpp:`WiFiDrv::config(validParams, ipAddress, gateway, subnet)`.
        // validParams = number of trailing params being supplied (1 = ip only,
        // 2 = ip+gw, 3 = ip+gw+subnet).
        self.send_cmd(CMD_SET_NET, &[&[3u8], &ip, &gateway, &subnet])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_SET_NET, &mut buf).await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Set primary + secondary DNS servers. Call **before**
    /// [`Self::connect_wpa`]; supplied values override DHCP-provided ones.
    pub async fn set_dns_config(
        &mut self,
        primary: [u8; 4],
        secondary: [u8; 4],
    ) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_SET_DNS_CONFIG, &[&[2u8], &primary, &secondary])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_SET_DNS_CONFIG, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Enable / disable WiFi power-save mode. `enabled == true` lets the
    /// chip sleep between DTIM beacons (~lower current, ~higher RX
    /// latency).
    pub async fn set_power_save(&mut self, enabled: bool) -> Result<(), Error<SpiErr>> {
        let v: u8 = u8::from(enabled);
        self.send_cmd(CMD_SET_POWER_MODE, &[&[v]]).await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_SET_POWER_MODE, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Read the ESP32's internal temperature in degrees Celsius (f32 LE on
    /// the wire). Useful as a sanity check that the chip is alive and as a
    /// proxy for chip heating under load.
    pub async fn get_temperature(&mut self) -> Result<f32, Error<SpiErr>> {
        self.send_cmd_no_params(CMD_GET_TEMPERATURE).await?;
        let mut buf = [0u8; 4];
        let n = self
            .recv_cmd_one_param(CMD_GET_TEMPERATURE, &mut buf)
            .await?;
        if n != 4 {
            return Err(Error::Framing);
        }
        Ok(f32::from_le_bytes(buf))
    }

    /// RSSI of the current STA association in dBm (signed i32 LE on wire).
    pub async fn get_current_rssi(&mut self) -> Result<i32, Error<SpiErr>> {
        self.send_cmd(CMD_GET_CURR_RSSI, &[&[DUMMY]]).await?;
        let mut buf = [0u8; 4];
        let n = self.recv_cmd_one_param(CMD_GET_CURR_RSSI, &mut buf).await?;
        if n != 4 {
            return Err(Error::Framing);
        }
        Ok(i32::from_le_bytes(buf))
    }

    /// BSSID of the AP currently associated with (6 bytes, MAC order
    /// canonicalised — Arduino returns it reversed on the wire).
    pub async fn get_current_bssid(&mut self) -> Result<[u8; 6], Error<SpiErr>> {
        self.send_cmd(CMD_GET_CURR_BSSID, &[&[DUMMY]]).await?;
        let mut buf = [0u8; 8];
        let n = self
            .recv_cmd_one_param(CMD_GET_CURR_BSSID, &mut buf)
            .await?;
        if n != 6 {
            return Err(Error::Framing);
        }
        Ok([buf[5], buf[4], buf[3], buf[2], buf[1], buf[0]])
    }

    /// Encryption type of the current association
    /// (matches the [`crate::EncType`] returned by [`Self::scan_enct`]).
    pub async fn get_current_enct(&mut self) -> Result<EncType, Error<SpiErr>> {
        self.send_cmd(CMD_GET_CURR_ENCT, &[&[DUMMY]]).await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_GET_CURR_ENCT, &mut buf).await?;
        Ok(EncType::from_u8(buf[0]))
    }

    /// BSSID for a scan index (Arduino-style reversed → canonical MAC order).
    pub async fn scan_bssid(&mut self, index: u8) -> Result<[u8; 6], Error<SpiErr>> {
        self.send_cmd(CMD_GET_IDX_BSSID, &[&[index]]).await?;
        let mut buf = [0u8; 8];
        let n = self.recv_cmd_one_param(CMD_GET_IDX_BSSID, &mut buf).await?;
        if n != 6 {
            return Err(Error::Framing);
        }
        Ok([buf[5], buf[4], buf[3], buf[2], buf[1], buf[0]])
    }

    /// Last WiFi disconnect reason code (vendor-specific u8 from the
    /// ESP32-side WiFi stack). Useful for diagnosing why
    /// [`Self::connect_wpa`] failed — values mirror ESP-IDF's
    /// `wifi_err_reason_t` (e.g. 15 = `4WAY_HANDSHAKE_TIMEOUT`, 201 =
    /// `NO_AP_FOUND`, 202 = `AUTH_FAIL`).
    pub async fn get_reason_code(&mut self) -> Result<u8, Error<SpiErr>> {
        self.send_cmd(CMD_GET_REASON_CODE, &[&[DUMMY]]).await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_GET_REASON_CODE, &mut buf)
            .await?;
        Ok(buf[0])
    }

    // ============== NINA GPIO passthrough ===================================
    //
    // The NINA-W102 has spare GPIOs that the host MCU can drive over SPI.
    // On the Nano RP2040 Connect the onboard RGB LED is wired to these
    // pins (R=27, G=25, B=26 — see [`crate::proto::LED_R`] etc.), so this
    // is the only path to the most visible status indicator on the board.

    /// Configure a NINA-side GPIO. `mode` is one of
    /// [`crate::proto::PIN_INPUT`], [`crate::proto::PIN_OUTPUT`],
    /// [`crate::proto::PIN_INPUT_PULLUP`].
    pub async fn pin_mode(
        &mut self,
        pin: u8,
        mode: crate::proto::PinMode,
    ) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_SET_PIN_MODE, &[&[pin], &[mode as u8]])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_SET_PIN_MODE, &mut buf).await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Drive a NINA-side GPIO HIGH (`value != 0`) or LOW (`value == 0`).
    /// The pin must already be configured as output via [`Self::pin_mode`].
    pub async fn digital_write(&mut self, pin: u8, value: u8) -> Result<(), Error<SpiErr>> {
        let v: u8 = if value != 0 { 1 } else { 0 };
        self.send_cmd(CMD_SET_DIGITAL_WRITE, &[&[pin], &[v]])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_SET_DIGITAL_WRITE, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// PWM-style write on a NINA-side GPIO. `value` is 0..=255. The pin
    /// must support PWM and be configured as output.
    pub async fn analog_write(&mut self, pin: u8, value: u8) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_SET_ANALOG_WRITE, &[&[pin], &[value]])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_SET_ANALOG_WRITE, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Read a NINA-side GPIO as digital — returns `true` for HIGH,
    /// `false` for LOW. Pin must be configured as
    /// [`crate::proto::PIN_INPUT`] or [`crate::proto::PIN_INPUT_PULLUP`]
    /// first via [`Self::pin_mode`]; reading a pin configured as
    /// OUTPUT may always return LOW because the ESP32 disables the input
    /// buffer in that mode.
    pub async fn digital_read(&mut self, pin: u8) -> Result<bool, Error<SpiErr>> {
        self.send_cmd(CMD_GET_DIGITAL_READ, &[&[pin]]).await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_GET_DIGITAL_READ, &mut buf)
            .await?;
        Ok(buf[0] != 0)
    }

    /// Read a NINA-side ADC pin. Returns the raw value (the ESP32's ADC is
    /// 12-bit but nina-fw scales it — typical range 0..=1023 to match
    /// Arduino conventions).
    pub async fn analog_read(&mut self, pin: u8) -> Result<u16, Error<SpiErr>> {
        self.send_cmd(CMD_GET_ANALOG_READ, &[&[pin]]).await?;
        let mut buf = [0u8; 4];
        let n = self
            .recv_cmd_one_param(CMD_GET_ANALOG_READ, &mut buf)
            .await?;
        let v = match n {
            1 => buf[0] as u16,
            2 => u16::from_le_bytes([buf[0], buf[1]]),
            4 => u32::from_le_bytes(buf).min(u16::MAX as u32) as u16,
            _ => return Err(Error::Framing),
        };
        Ok(v)
    }

    /// Start an open (unencrypted) AP on `channel` (1..=13). After this,
    /// poll [`Self::status`] for [`WlStatus::ApListening`] (no client yet)
    /// or [`WlStatus::ApConnected`] (a client has joined). The chip
    /// auto-assigns 192.168.4.1 as its AP IP.
    pub async fn start_ap_open(&mut self, ssid: &[u8], channel: u8) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_SET_AP_NET, &[ssid, &[channel]]).await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_SET_AP_NET, &mut buf).await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Start a WPA2-PSK AP on `channel`. Passphrase must be 8..=63 bytes
    /// (WPA2 minimum). The chip auto-assigns 192.168.4.1 as its AP IP.
    pub async fn start_ap_wpa(
        &mut self,
        ssid: &[u8],
        passphrase: &[u8],
        channel: u8,
    ) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_SET_AP_PASSPHRASE, &[ssid, passphrase, &[channel]])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_SET_AP_PASSPHRASE, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Tear down any current association.
    pub async fn disconnect(&mut self) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_DISCONNECT, &[&[DUMMY]]).await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_DISCONNECT, &mut buf).await?;
        Ok(())
    }

    /// ICMP ping. `ttl` is the IP TTL field (Arduino default = 128).
    /// Returns the round-trip time in milliseconds, or `Err(Error::Nina)`
    /// if the host is unreachable / timed out.
    pub async fn ping(&mut self, ip: [u8; 4], ttl: u8) -> Result<u16, Error<SpiErr>> {
        self.send_cmd(CMD_PING, &[&ip, &[ttl]]).await?;
        let mut buf = [0u8; 4];
        let n = self.recv_cmd_one_param(CMD_PING, &mut buf).await?;
        let rtt = match n {
            1 => buf[0] as u16,
            2 => u16::from_le_bytes([buf[0], buf[1]]),
            _ => return Err(Error::Framing),
        };
        // nina-fw returns 0xFFFF for unreachable / timeout.
        if rtt == 0xFFFF {
            return Err(Error::Nina);
        }
        Ok(rtt)
    }

    /// Resolve a hostname to an IPv4 address via the NINA's DNS client.
    /// Two-step exchange under the hood: `REQ_HOST_BY_NAME` queues the
    /// lookup, `GET_HOST_BY_NAME` collects the result.
    pub async fn dns_lookup(&mut self, hostname: &[u8]) -> Result<[u8; 4], Error<SpiErr>> {
        self.send_cmd(CMD_REQ_HOST_BY_NAME, &[hostname]).await?;
        let mut ack = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_REQ_HOST_BY_NAME, &mut ack)
            .await?;
        if ack[0] != 1 {
            return Err(Error::Nina);
        }

        // The chip needs some time to do the DNS query. Poll briefly.
        for _ in 0..40 {
            embassy_time::Timer::after(embassy_time::Duration::from_millis(100)).await;
            self.send_cmd_no_params(CMD_GET_HOST_BY_NAME).await?;
            let mut ip = [0u8; 4];
            match self.recv_cmd_one_param(CMD_GET_HOST_BY_NAME, &mut ip).await {
                Ok(4) => {
                    // 0.0.0.0 means "still resolving" — retry.
                    if ip != [0, 0, 0, 0] {
                        return Ok(ip);
                    }
                }
                Ok(_) => {}
                Err(Error::Framing) | Err(Error::Nina) => {} // not ready yet
                Err(e) => return Err(e),
            }
        }
        Err(Error::Timeout)
    }

    // ============== TCP socket primitives ====================================

    /// Reserve a socket handle. Range 0..=7 typically; 0xFF means
    /// "no socket available".
    pub async fn tcp_open_socket(&mut self) -> Result<Socket, Error<SpiErr>> {
        self.send_cmd_no_params(CMD_GET_SOCKET).await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_GET_SOCKET, &mut buf).await?;
        if buf[0] == 0xFF {
            return Err(Error::Nina);
        }
        Ok(Socket(buf[0]))
    }

    /// Start a TCP/UDP/TLS connect attempt on `sock`. Returns once the
    /// command is acked; poll [`Self::tcp_state`] until [`SockState::Established`].
    pub async fn tcp_start_client(
        &mut self,
        sock: Socket,
        ip: [u8; 4],
        port: u16,
        mode: u8,
    ) -> Result<(), Error<SpiErr>> {
        // nina-fw 2.x: all four params use 8-bit length prefixes, and port is
        // expected in network (big-endian) byte order — handler applies
        // ntohs() on its in-memory copy.
        let port_be = port.to_be_bytes();
        self.send_cmd(CMD_START_CLIENT_TCP, &[&ip, &port_be, &[sock.0], &[mode]])
            .await?;
        // nina-fw returns 1 param (ack byte) on success or 0 params on
        // failure. Accept either via the variable-param recv.
        let mut scratch = [0u8; 4];
        let _ = self
            .recv_cmd_into(CMD_START_CLIENT_TCP, &mut scratch)
            .await?;
        Ok(())
    }

    /// Start a TCP/TLS connect attempt that also carries a hostname for
    /// SNI. Same opcode as [`Self::tcp_start_client`] but with five params
    /// (hostname prepended). Required for TLS to CDN-fronted sites — the
    /// chip uses `hostname` for the TLS SNI extension and `ip` for the
    /// underlying TCP connection.
    pub async fn tcp_start_client_hostname(
        &mut self,
        sock: Socket,
        hostname: &[u8],
        ip: [u8; 4],
        port: u16,
        mode: u8,
    ) -> Result<(), Error<SpiErr>> {
        let port_be = port.to_be_bytes();
        self.send_cmd(
            CMD_START_CLIENT_TCP,
            &[hostname, &ip, &port_be, &[sock.0], &[mode]],
        )
        .await?;
        let mut scratch = [0u8; 4];
        let _ = self
            .recv_cmd_into(CMD_START_CLIENT_TCP, &mut scratch)
            .await?;
        Ok(())
    }

    /// Current TCP state of the socket. Use this to wait for
    /// [`SockState::Established`] after `tcp_start_client`.
    ///
    /// **Caveat:** nina-fw's `getClientStateTcp` handler **frees** the slot
    /// (sets `socketTypes[socket] = 255`) whenever its `connected()` check
    /// returns false. So polling state on a server-side accepted socket
    /// during the handshake window can destroy the slot. Safe to call on
    /// a *client* socket created via [`Self::tcp_start_client`]; risky on
    /// a server-side accepted socket — prefer a short fixed wait there.
    pub async fn tcp_state(&mut self, sock: Socket) -> Result<SockState, Error<SpiErr>> {
        self.send_cmd(CMD_GET_CLIENT_STATE_TCP, &[&[sock.0]])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_GET_CLIENT_STATE_TCP, &mut buf)
            .await?;
        Ok(SockState::from_u8(buf[0]))
    }

    /// Bytes available to read on `sock`. Use this to gate a `tcp_recv` call.
    pub async fn tcp_avail(&mut self, sock: Socket) -> Result<u16, Error<SpiErr>> {
        self.send_cmd(CMD_AVAIL_DATA_TCP, &[&[sock.0]]).await?;
        let mut buf = [0u8; 4];
        let n = self
            .recv_cmd_one_param(CMD_AVAIL_DATA_TCP, &mut buf)
            .await?;
        let v = match n {
            1 => buf[0] as u16,
            2 => u16::from_le_bytes([buf[0], buf[1]]),
            _ => return Err(Error::Framing),
        };
        Ok(v)
    }

    /// Send all of `data` on the TCP socket. Splits internally into
    /// ~64-byte chunks (the cadence Arduino's `client.print` ends up
    /// producing), retries each chunk up to 5 times on transient `0` ack,
    /// and polls `DATA_SENT_TCP` between chunks. Returns the total bytes
    /// the firmware accepted — equals `data.len()` on success.
    pub async fn tcp_send(&mut self, sock: Socket, data: &[u8]) -> Result<u16, Error<SpiErr>> {
        // Each chip-side `SEND_DATA_TCP` call writes a single TCP segment.
        // Large single-call writes (~109+ bytes seen in practice) work for
        // the first 2 connections then the chip's send pipeline wedges.
        // Splitting into smaller chunks — same cadence as Arduino's per-line
        // `client.println` — fixes it. 64 bytes is a sweet spot.
        const CHUNK: usize = 64;
        let mut total: u16 = 0;
        for chunk in data.chunks(CHUNK) {
            let mut written: u16 = 0;
            for _ in 0..5 {
                self.send_cmd_mixed(
                    CMD_SEND_DATA_TCP,
                    &[Param::Long(&[sock.0]), Param::Long(chunk)],
                )
                .await?;
                let mut buf = [0u8; 4];
                let n = self.recv_cmd_one_param(CMD_SEND_DATA_TCP, &mut buf).await?;
                written = match n {
                    1 => buf[0] as u16,
                    2 => u16::from_le_bytes([buf[0], buf[1]]),
                    _ => return Err(Error::Framing),
                };
                if written > 0 {
                    break;
                }
            }
            if written == 0 {
                return Err(Error::Nina);
            }
            self.check_data_sent(sock).await?;
            total = total.saturating_add(written);
        }
        Ok(total)
    }

    /// Block until the chip reports the previous `tcp_send` payload has
    /// been transmitted (up to ~1 s). Mirrors Arduino's
    /// `ServerDrv::checkDataSent`. Called automatically by
    /// [`Self::tcp_send`]; exposed standalone for the rare case you want
    /// to confirm a write yourself.
    pub async fn check_data_sent(&mut self, sock: Socket) -> Result<(), Error<SpiErr>> {
        for _ in 0..20 {
            self.send_cmd(CMD_DATA_SENT_TCP, &[&[sock.0]]).await?;
            let mut buf = [0u8; 1];
            let _ = self.recv_cmd_one_param(CMD_DATA_SENT_TCP, &mut buf).await?;
            if buf[0] != 0 {
                return Ok(());
            }
            Timer::after(Duration::from_millis(50)).await;
        }
        Err(Error::Timeout)
    }

    /// Pull up to `out.len()` bytes from the TCP receive buffer.
    /// Returns the count actually delivered (may be 0).
    pub async fn tcp_recv(&mut self, sock: Socket, out: &mut [u8]) -> Result<usize, Error<SpiErr>> {
        let want = (out.len().min(u16::MAX as usize)) as u16;
        // The `want` value is read host-native (LE) by nina-fw — no ntohs() —
        // so encode it LE. Param length prefixes themselves are still BE
        // (DATA_FLAG framing), handled by send_cmd_mixed's Param::Long.
        let want_le = want.to_le_bytes();
        self.send_cmd_mixed(
            CMD_GET_DATABUF_TCP,
            &[Param::Long(&[sock.0]), Param::Long(&want_le)],
        )
        .await?;
        // 16-bit-length response.
        self.recv_cmd_one_param_16(CMD_GET_DATABUF_TCP, out).await
    }

    // ============== UDP socket primitives ===================================

    /// Bind a socket for UDP receive on `port` (any interface). The chip
    /// allocates the socket internally — pass the result of
    /// [`Self::tcp_open_socket`].
    pub async fn udp_bind(&mut self, sock: Socket, port: u16) -> Result<(), Error<SpiErr>> {
        // Verified working with NTP via 8-bit length prefixes + BE port.
        // (Server-side TCP framing might differ — see `tcp_listen`.)
        let port_be = port.to_be_bytes();
        self.send_cmd(CMD_START_SERVER_TCP, &[&port_be, &[sock.0], &[UDP_MODE]])
            .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_START_SERVER_TCP, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// High-level UDP send: addresses a datagram at `(ip, port)`, writes
    /// the payload, and commits — in one call. Prefer this over the
    /// three-step [`Self::udp_begin_packet`] / [`Self::udp_write`] /
    /// [`Self::udp_end_packet`] sequence unless you specifically need to
    /// fragment a build across awaits.
    pub async fn udp_send(
        &mut self,
        sock: Socket,
        ip: [u8; 4],
        port: u16,
        data: &[u8],
    ) -> Result<(), Error<SpiErr>> {
        self.udp_begin_packet(sock, ip, port).await?;
        Timer::after(Duration::from_millis(20)).await;
        self.udp_write(sock, data).await?;
        self.udp_end_packet(sock).await
    }

    /// Start a UDP "packet" addressed at `(ip, port)`. After this you call
    /// [`Self::udp_write`] one or more times to buffer bytes, then
    /// [`Self::udp_end_packet`] to actually transmit. Mirrors Arduino's
    /// `WiFiUDP::beginPacket`. Prefer the one-shot [`Self::udp_send`] when
    /// you have the whole payload up front.
    pub async fn udp_begin_packet(
        &mut self,
        sock: Socket,
        ip: [u8; 4],
        port: u16,
    ) -> Result<(), Error<SpiErr>> {
        self.tcp_start_client(sock, ip, port, UDP_MODE).await
    }

    /// Append `data` to the in-progress UDP packet. Multiple calls
    /// concatenate. The chip's ack is a single byte (1 = accepted, not a
    /// byte count); returns `Ok(())` on accept.
    pub async fn udp_write(&mut self, sock: Socket, data: &[u8]) -> Result<(), Error<SpiErr>> {
        self.send_cmd_mixed(
            CMD_INSERT_DATABUF,
            &[Param::Long(&[sock.0]), Param::Long(data)],
        )
        .await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_INSERT_DATABUF, &mut buf)
            .await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Commit the buffered UDP packet onto the wire. Returns `Ok(())` once
    /// the chip ACKs; the actual datagram is fire-and-forget.
    pub async fn udp_end_packet(&mut self, sock: Socket) -> Result<(), Error<SpiErr>> {
        // Brief delay — the chip needs a moment after `INSERT_DATABUF` before
        // it will accept `SEND_UDP_DATA`. Without this, we sometimes get
        // ack=0 even though the data was buffered.
        embassy_time::Timer::after(embassy_time::Duration::from_millis(10)).await;
        self.send_cmd(CMD_SEND_UDP_DATA, &[&[sock.0]]).await?;
        let mut buf = [0u8; 1];
        let _ = self.recv_cmd_one_param(CMD_SEND_UDP_DATA, &mut buf).await?;
        if buf[0] != 1 {
            return Err(Error::Nina);
        }
        Ok(())
    }

    /// Peer IP+port of the most recent UDP datagram delivered on `sock`.
    /// Only meaningful right after a successful [`Self::tcp_recv`].
    pub async fn udp_remote(&mut self, sock: Socket) -> Result<([u8; 4], u16), Error<SpiErr>> {
        self.send_cmd(CMD_GET_REMOTE_DATA, &[&[sock.0]]).await?;
        let mut scratch = [0u8; 16];
        let (_n, written) = self
            .recv_cmd_into(CMD_GET_REMOTE_DATA, &mut scratch)
            .await?;
        // Two params: [len=4, ip], [len=2, port_be]
        if written < 1 + 4 + 1 + 2 {
            return Err(Error::Framing);
        }
        if scratch[0] != 4 {
            return Err(Error::Framing);
        }
        let ip = [scratch[1], scratch[2], scratch[3], scratch[4]];
        if scratch[5] != 2 {
            return Err(Error::Framing);
        }
        let port = u16::from_be_bytes([scratch[6], scratch[7]]);
        Ok((ip, port))
    }

    /// Close the socket. Releases the handle so a future
    /// `tcp_open_socket` can reuse it.
    pub async fn tcp_close(&mut self, sock: Socket) -> Result<(), Error<SpiErr>> {
        self.send_cmd(CMD_STOP_CLIENT_TCP, &[&[sock.0]]).await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_STOP_CLIENT_TCP, &mut buf)
            .await?;
        // `STOP_CLIENT_TCP` chip-side already frees the slot
        // (`socketTypes[socket] = 255`). Arduino additionally polls
        // `tcp_state` to wait for the lwip TCP teardown to complete, but
        // nina-fw's `getClientStateTcp` poisons OTHER slots that aren't
        // currently connected — so the only safe thing is a fixed wait.
        Timer::after(Duration::from_millis(100)).await;
        Ok(())
    }

    /// High-level TCP connect: opens a socket, kicks off the SYN, polls
    /// state until established. Returns an [`crate::NinaTcpSocket`] that
    /// impls `embedded-io-async` Read + Write.
    pub async fn tcp_connect<'a>(
        &'a mut self,
        ip: [u8; 4],
        port: u16,
    ) -> Result<crate::tcp::NinaTcpSocket<'a, Bus, Cs, Ack, Rst, Boot>, Error<SpiErr>> {
        let sock = self.tcp_open_socket().await?;
        self.tcp_start_client(sock, ip, port, TCP_MODE).await?;
        for _ in 0..40 {
            match self.tcp_state(sock).await? {
                SockState::Established => {
                    return Ok(crate::tcp::NinaTcpSocket::new(self, sock));
                }
                SockState::Closed
                | SockState::FinWait1
                | SockState::FinWait2
                | SockState::Closing
                | SockState::LastAck
                | SockState::TimeWait
                | SockState::CloseWait => {
                    // Allow a brief grace window for the chip to flip from
                    // its default 'Closed' into 'SynSent' before bailing.
                }
                _ => {}
            }
            embassy_time::Timer::after(embassy_time::Duration::from_millis(250)).await;
        }
        let _ = self.tcp_close(sock).await;
        Err(Error::Timeout)
    }

    /// Current Wi-Fi connection state.
    pub async fn status(&mut self) -> Result<WlStatus, Error<SpiErr>> {
        // GET_CONN_STATUS takes one dummy param, returns one byte.
        self.send_cmd(CMD_GET_CONN_STATUS, &[&[DUMMY]]).await?;
        let mut buf = [0u8; 1];
        let _ = self
            .recv_cmd_one_param(CMD_GET_CONN_STATUS, &mut buf)
            .await?;
        Ok(WlStatus::from_u8(buf[0]))
    }

    /// IPv4 address assigned to the STA interface. Only meaningful when
    /// status is [`WlStatus::Connected`]. Shorthand for
    /// `ip_config().map(|c| c.ip)`.
    pub async fn ip(&mut self) -> Result<[u8; 4], Error<SpiErr>> {
        Ok(self.ip_config().await?.ip)
    }

    /// Full IPv4 configuration assigned by DHCP (or static config): address,
    /// subnet mask, gateway. Only meaningful when status is
    /// [`WlStatus::Connected`].
    pub async fn ip_config(&mut self) -> Result<IpConfig, Error<SpiErr>> {
        // GET_IPADDR takes one dummy param, returns 3 params: IP, mask, gw.
        self.send_cmd(CMD_GET_IPADDR, &[&[DUMMY]]).await?;
        let mut scratch = [0u8; 32];
        let (n_params, written) = self.recv_cmd_into(CMD_GET_IPADDR, &mut scratch).await?;
        if n_params < 3 {
            return Err(Error::Framing);
        }

        // Each param is encoded as [len, ..len bytes..]; we expect three
        // back-to-back 4-byte values.
        let mut cur = 0usize;
        let mut take_v4 = || -> Result<[u8; 4], Error<SpiErr>> {
            if cur + 1 > written {
                return Err(Error::Framing);
            }
            let len = scratch[cur] as usize;
            cur += 1;
            if len != 4 || cur + 4 > written {
                return Err(Error::Framing);
            }
            let v = [
                scratch[cur],
                scratch[cur + 1],
                scratch[cur + 2],
                scratch[cur + 3],
            ];
            cur += 4;
            Ok(v)
        };
        let ip = take_v4()?;
        let subnet = take_v4()?;
        let gateway = take_v4()?;
        Ok(IpConfig {
            ip,
            subnet,
            gateway,
        })
    }

    // -------- low-level framing ------------------------------------------

    /// Send a command with N short (8-bit length) parameters.
    async fn send_cmd(&mut self, cmd: u8, params: &[&[u8]]) -> Result<(), Error<SpiErr>> {
        // Re-pack as Param::Short and delegate.
        // Build a small heapless::Vec to avoid alloc — but we don't have
        // heapless as a dep. Inline-build via send_cmd_inner instead.
        self.ack.wait_for_low().await.map_err(|_| Error::Pin)?;

        self.cs.set_low().map_err(|_| Error::Pin)?;
        let _ = select(
            self.ack.wait_for_high(),
            Timer::after(Duration::from_millis(ACK_AFTER_CS_MS)),
        )
        .await;

        let res = async {
            let hdr = [START_CMD, cmd & !REPLY_FLAG, params.len() as u8];
            self.bus.write(&hdr).await.map_err(Error::Spi)?;
            let mut total = 3usize;
            for p in params {
                self.bus.write(&[p.len() as u8]).await.map_err(Error::Spi)?;
                self.bus.write(p).await.map_err(Error::Spi)?;
                total += 1 + p.len();
            }
            self.bus.write(&[END_CMD]).await.map_err(Error::Spi)?;
            total += 1;
            self.pad_to_4(total).await?;
            Ok::<(), Error<SpiErr>>(())
        }
        .await;

        let _ = self.cs.set_high();
        res
    }

    /// Send a command with mixed 8-bit / 16-bit-length parameters.
    /// Builds the entire frame in a stack buffer and clocks it out as a
    /// single `bus.write` — avoids any timing-between-writes artifact.
    /// Max frame: 256 bytes (SET_PASSPHRASE is the biggest non-data case).
    pub(crate) async fn send_cmd_mixed(
        &mut self,
        cmd: u8,
        params: &[Param<'_>],
    ) -> Result<(), Error<SpiErr>> {
        let mut buf = [0u8; 256];
        let mut n = 0usize;
        buf[n] = START_CMD;
        n += 1;
        buf[n] = cmd & !REPLY_FLAG;
        n += 1;
        buf[n] = params.len() as u8;
        n += 1;
        for p in params {
            match p {
                Param::Short(data) => {
                    if n + 1 + data.len() > buf.len() {
                        return Err(Error::BufferTooSmall);
                    }
                    buf[n] = data.len() as u8;
                    n += 1;
                    buf[n..n + data.len()].copy_from_slice(data);
                    n += data.len();
                }
                Param::Long(data) => {
                    if n + 2 + data.len() > buf.len() {
                        return Err(Error::BufferTooSmall);
                    }
                    let len_be = (data.len() as u16).to_be_bytes();
                    buf[n..n + 2].copy_from_slice(&len_be);
                    n += 2;
                    buf[n..n + data.len()].copy_from_slice(data);
                    n += data.len();
                }
            }
        }
        if n + 1 > buf.len() {
            return Err(Error::BufferTooSmall);
        }
        buf[n] = END_CMD;
        n += 1;
        while !n.is_multiple_of(4) {
            if n >= buf.len() {
                return Err(Error::BufferTooSmall);
            }
            buf[n] = DUMMY;
            n += 1;
        }

        self.ack.wait_for_low().await.map_err(|_| Error::Pin)?;

        self.cs.set_low().map_err(|_| Error::Pin)?;
        let _ = select(
            self.ack.wait_for_high(),
            Timer::after(Duration::from_millis(ACK_AFTER_CS_MS)),
        )
        .await;

        let res = self.bus.write(&buf[..n]).await.map_err(Error::Spi);

        let _ = self.cs.set_high();
        res
    }

    /// Receive a single response parameter that uses a 16-bit-BE length
    /// prefix (the `waitResponseData16` variant in WiFiNINA host code).
    pub(crate) async fn recv_cmd_one_param_16(
        &mut self,
        cmd: u8,
        out: &mut [u8],
    ) -> Result<usize, Error<SpiErr>> {
        self.ack.wait_for_low().await.map_err(|_| Error::Pin)?;

        self.cs.set_low().map_err(|_| Error::Pin)?;
        let _ = select(
            self.ack.wait_for_high(),
            Timer::after(Duration::from_millis(ACK_AFTER_CS_MS)),
        )
        .await;

        let res = self.recv_cmd_one_param_16_inner(cmd, out).await;

        let _ = self.cs.set_high();
        res
    }

    async fn recv_cmd_one_param_16_inner(
        &mut self,
        cmd: u8,
        out: &mut [u8],
    ) -> Result<usize, Error<SpiErr>> {
        let mut byte = [DUMMY; 1];
        for _ in 0..START_TIMEOUT_ITERS {
            byte[0] = DUMMY;
            self.bus
                .transfer_in_place(&mut byte)
                .await
                .map_err(Error::Spi)?;
            if byte[0] == START_CMD {
                break;
            }
            if byte[0] == ERR_CMD {
                return Err(Error::Nina);
            }
        }
        if byte[0] != START_CMD {
            return Err(Error::Timeout);
        }

        let mut hdr = [DUMMY; 2];
        self.bus
            .transfer_in_place(&mut hdr)
            .await
            .map_err(Error::Spi)?;
        if hdr[0] != (cmd | REPLY_FLAG) {
            return Err(Error::Framing);
        }
        if hdr[1] != 1 {
            return Err(Error::Framing);
        }

        let mut len_buf = [DUMMY; 2];
        self.bus
            .transfer_in_place(&mut len_buf)
            .await
            .map_err(Error::Spi)?;
        let len = u16::from_be_bytes(len_buf) as usize;
        if len > out.len() {
            return Err(Error::BufferTooSmall);
        }

        for slot in out.iter_mut().take(len) {
            let mut b = [DUMMY; 1];
            self.bus
                .transfer_in_place(&mut b)
                .await
                .map_err(Error::Spi)?;
            *slot = b[0];
        }

        let mut end = [DUMMY; 1];
        self.bus
            .transfer_in_place(&mut end)
            .await
            .map_err(Error::Spi)?;
        if end[0] != END_CMD {
            return Err(Error::Framing);
        }

        Ok(len)
    }

    async fn pad_to_4(&mut self, total: usize) -> Result<(), Error<SpiErr>> {
        let mut total = total;
        while !total.is_multiple_of(4) {
            self.bus.write(&[DUMMY]).await.map_err(Error::Spi)?;
            total += 1;
        }
        Ok(())
    }

    /// Convenience: zero-param send.
    async fn send_cmd_no_params(&mut self, cmd: u8) -> Result<(), Error<SpiErr>> {
        self.send_cmd(cmd, &[]).await
    }

    /// Read a response framed as `[START, cmd|REPLY_FLAG, 1, len, ..bytes.., END]`.
    /// Copies up to `out.len()` payload bytes into `out`, returns the count.
    async fn recv_cmd_one_param(
        &mut self,
        cmd: u8,
        out: &mut [u8],
    ) -> Result<usize, Error<SpiErr>> {
        // Wait for NINA to finish processing the request.
        self.ack.wait_for_low().await.map_err(|_| Error::Pin)?;

        self.cs.set_low().map_err(|_| Error::Pin)?;
        let _ = select(
            self.ack.wait_for_high(),
            Timer::after(Duration::from_millis(ACK_AFTER_CS_MS)),
        )
        .await;

        let res = self.recv_cmd_one_param_inner(cmd, out).await;

        let _ = self.cs.set_high();
        res
    }

    /// Variable-param receive: caller supplies a scratch buffer; this packs
    /// each response param into it as `[len, bytes...]` repeated, and
    /// returns `(num_params, total_bytes_written)`.
    async fn recv_cmd_into(
        &mut self,
        cmd: u8,
        scratch: &mut [u8],
    ) -> Result<(u8, usize), Error<SpiErr>> {
        self.ack.wait_for_low().await.map_err(|_| Error::Pin)?;

        self.cs.set_low().map_err(|_| Error::Pin)?;
        let _ = select(
            self.ack.wait_for_high(),
            Timer::after(Duration::from_millis(ACK_AFTER_CS_MS)),
        )
        .await;

        let res = self.recv_cmd_into_inner(cmd, scratch).await;

        let _ = self.cs.set_high();
        res
    }

    async fn recv_cmd_into_inner(
        &mut self,
        cmd: u8,
        scratch: &mut [u8],
    ) -> Result<(u8, usize), Error<SpiErr>> {
        let mut byte = [DUMMY; 1];
        for _ in 0..START_TIMEOUT_ITERS {
            byte[0] = DUMMY;
            self.bus
                .transfer_in_place(&mut byte)
                .await
                .map_err(Error::Spi)?;
            if byte[0] == START_CMD {
                break;
            }
            if byte[0] == ERR_CMD {
                return Err(Error::Nina);
            }
        }
        if byte[0] != START_CMD {
            return Err(Error::Timeout);
        }

        let mut hdr = [DUMMY; 2];
        self.bus
            .transfer_in_place(&mut hdr)
            .await
            .map_err(Error::Spi)?;
        if hdr[0] != (cmd | REPLY_FLAG) {
            return Err(Error::Framing);
        }
        let num_params = hdr[1];

        let mut written = 0;
        for _ in 0..num_params {
            let mut len_buf = [DUMMY; 1];
            self.bus
                .transfer_in_place(&mut len_buf)
                .await
                .map_err(Error::Spi)?;
            let len = len_buf[0] as usize;
            if written + 1 + len > scratch.len() {
                return Err(Error::BufferTooSmall);
            }
            scratch[written] = len_buf[0];
            written += 1;
            for slot in scratch[written..written + len].iter_mut() {
                let mut b = [DUMMY; 1];
                self.bus
                    .transfer_in_place(&mut b)
                    .await
                    .map_err(Error::Spi)?;
                *slot = b[0];
            }
            written += len;
        }

        let mut end = [DUMMY; 1];
        self.bus
            .transfer_in_place(&mut end)
            .await
            .map_err(Error::Spi)?;
        if end[0] != END_CMD {
            return Err(Error::Framing);
        }

        Ok((num_params, written))
    }

    async fn recv_cmd_one_param_inner(
        &mut self,
        cmd: u8,
        out: &mut [u8],
    ) -> Result<usize, Error<SpiErr>> {
        // Spin-clock 0xFF on MOSI until we see START_CMD (0xE0).
        // The host driver pads with DUMMY bytes; we do the same.
        let mut byte = [0u8; 1];
        for _ in 0..START_TIMEOUT_ITERS {
            byte[0] = DUMMY;
            self.bus
                .transfer_in_place(&mut byte)
                .await
                .map_err(Error::Spi)?;
            if byte[0] == START_CMD {
                break;
            }
            if byte[0] == ERR_CMD {
                return Err(Error::Nina);
            }
        }
        if byte[0] != START_CMD {
            return Err(Error::Timeout);
        }

        // Header: reply_cmd, num_params, param_len
        let mut hdr = [DUMMY; 3];
        self.bus
            .transfer_in_place(&mut hdr)
            .await
            .map_err(Error::Spi)?;
        if hdr[0] != (cmd | REPLY_FLAG) {
            return Err(Error::Framing);
        }
        if hdr[1] != 1 {
            return Err(Error::Framing);
        }
        let len = hdr[2] as usize;
        if len > out.len() {
            return Err(Error::BufferTooSmall);
        }

        // Payload
        for slot in out.iter_mut().take(len) {
            let mut b = [DUMMY; 1];
            self.bus
                .transfer_in_place(&mut b)
                .await
                .map_err(Error::Spi)?;
            *slot = b[0];
        }

        // Trailing END_CMD
        let mut end = [DUMMY; 1];
        self.bus
            .transfer_in_place(&mut end)
            .await
            .map_err(Error::Spi)?;
        if end[0] != END_CMD {
            return Err(Error::Framing);
        }

        Ok(len)
    }
}
