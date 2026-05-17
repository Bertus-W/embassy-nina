#![allow(missing_docs)]
//! Wire-protocol constants and a few protocol-level types.
//!
//! Most callers should ignore this module — use [`crate::Nina`]'s methods
//! and the re-exports from the crate root (`PinMode`, `SockState`,
//! `EncType`, `WlStatus`). The `CMD_*` constants and the [`Param`] enum
//! are exposed for advanced users implementing custom commands.
//!
//! Reference: `arduino-libraries/WiFiNINA` v1.7.0 `src/utility/spi_drv.{h,cpp}`
//! and `src/utility/wifi_spi.h`. Firmware side: `arduino/nina-fw`
//! `main/CommandHandler.cpp`.
//!
//! Frame layout (`spi_drv.cpp` L548-L552):
//!
//! ```text
//! | START_CMD | C/R+CMD | numParam | paramLen | param | ... | END_CMD |
//! |  8 bit    | 8 bit   |  8 bit   |  8/16    | n     | ... |  8 bit  |
//! ```
//!
//! - C/R is the top bit of the CMD byte: `0` = command from host,
//!   `1` = reply from NINA (`REPLY_FLAG`).
//! - Commands with `DATA_FLAG` (`0x40`) set in their opcode encode each
//!   parameter length as a 16-bit big-endian word instead of an 8-bit byte.

#[doc(hidden)]
pub const START_CMD: u8 = 0xE0;
#[doc(hidden)]
pub const END_CMD: u8 = 0xEE;
#[doc(hidden)]
pub const ERR_CMD: u8 = 0xEF;

#[doc(hidden)]
pub const REPLY_FLAG: u8 = 1 << 7;
#[doc(hidden)]
pub const DATA_FLAG: u8 = 0x40;

#[doc(hidden)]
pub const DUMMY: u8 = 0xFF;

/// Max iterations to poll the bus for `START_CMD` during the response phase.
/// Matches `TIMEOUT_CHAR` in `wifi_spi.h`.
#[doc(hidden)]
pub const START_TIMEOUT_ITERS: u32 = 1000;

/// Time the host waits between asserting CS and seeing ACK go HIGH.
/// `spiSlaveSelect()` polls for ~5 ms then proceeds anyway.
#[doc(hidden)]
pub const ACK_AFTER_CS_MS: u64 = 5;

/// Reset pulse width and post-release boot wait. Arduino reference is
/// 10 ms + 750 ms; we go longer because some retained-state scenarios
/// (re-flashing host without USB power cycle) need the ESP32 to actually
/// see a fresh boot.
#[doc(hidden)]
pub const RESET_LOW_MS: u64 = 100;
#[doc(hidden)]
pub const RESET_BOOT_MS: u64 = 2_500;

/// SPI bus parameters (`spi_drv.cpp` L126).
pub const SPI_FREQ_HZ: u32 = 8_000_000;

// ---- Command opcodes ----------------------------------------------------
//
// Subset filled in as we implement each command. Source: `wifi_spi.h`
// L57-L227 (master branch).

#[doc(hidden)]
pub const CMD_SET_NET: u8 = 0x10;
#[doc(hidden)]
pub const CMD_SET_PASSPHRASE: u8 = 0x11;
#[doc(hidden)]
pub const CMD_SET_DNS_CONFIG: u8 = 0x15;
#[doc(hidden)]
pub const CMD_SET_HOSTNAME: u8 = 0x16;
#[doc(hidden)]
pub const CMD_SET_POWER_MODE: u8 = 0x17;
#[doc(hidden)]
pub const CMD_SET_AP_NET: u8 = 0x18;
#[doc(hidden)]
pub const CMD_SET_AP_PASSPHRASE: u8 = 0x19;
#[doc(hidden)]
pub const CMD_GET_TEMPERATURE: u8 = 0x1B;
#[doc(hidden)]
pub const CMD_GET_CURR_BSSID: u8 = 0x24;
#[doc(hidden)]
pub const CMD_GET_CURR_RSSI: u8 = 0x25;
#[doc(hidden)]
pub const CMD_GET_CURR_ENCT: u8 = 0x26;
#[doc(hidden)]
pub const CMD_GET_STATE_TCP: u8 = 0x29;
#[doc(hidden)]
pub const CMD_GET_REASON_CODE: u8 = 0x1F;
#[doc(hidden)]
pub const CMD_GET_IDX_BSSID: u8 = 0x3C;
#[doc(hidden)]
pub const CMD_GET_DIGITAL_READ: u8 = 0x53;
#[doc(hidden)]
pub const CMD_GET_ANALOG_READ: u8 = 0x54;
#[doc(hidden)]
pub const CMD_GET_CONN_STATUS: u8 = 0x20;
#[doc(hidden)]
pub const CMD_GET_IPADDR: u8 = 0x21;
#[doc(hidden)]
pub const CMD_GET_MAC_ADDR: u8 = 0x22;
#[doc(hidden)]
pub const CMD_GET_CURR_SSID: u8 = 0x23;
#[doc(hidden)]
pub const CMD_START_SERVER_TCP: u8 = 0x28;
#[doc(hidden)]
pub const CMD_DATA_SENT_TCP: u8 = 0x2A;
#[doc(hidden)]
pub const CMD_AVAIL_DATA_TCP: u8 = 0x2B;
#[doc(hidden)]
pub const CMD_START_CLIENT_TCP: u8 = 0x2D;
#[doc(hidden)]
pub const CMD_STOP_CLIENT_TCP: u8 = 0x2E;
#[doc(hidden)]
pub const CMD_GET_CLIENT_STATE_TCP: u8 = 0x2F;
#[doc(hidden)]
pub const CMD_DISCONNECT: u8 = 0x30;
#[doc(hidden)]
pub const CMD_REQ_HOST_BY_NAME: u8 = 0x34;
#[doc(hidden)]
pub const CMD_GET_HOST_BY_NAME: u8 = 0x35;
#[doc(hidden)]
pub const CMD_GET_IDX_RSSI: u8 = 0x32;
#[doc(hidden)]
pub const CMD_GET_IDX_ENCT: u8 = 0x33;
#[doc(hidden)]
pub const CMD_SEND_UDP_DATA: u8 = 0x39;
#[doc(hidden)]
pub const CMD_GET_REMOTE_DATA: u8 = 0x3A;
#[doc(hidden)]
pub const CMD_GET_IDX_CHANNEL: u8 = 0x3D;
#[doc(hidden)]
pub const CMD_GET_SOCKET: u8 = 0x3F;
#[doc(hidden)]
pub const CMD_SEND_DATA_TCP: u8 = 0x44; // DATA_FLAG-bit set
#[doc(hidden)]
pub const CMD_GET_DATABUF_TCP: u8 = 0x45; // DATA_FLAG-bit set
#[doc(hidden)]
pub const CMD_INSERT_DATABUF: u8 = 0x46; // DATA_FLAG-bit set
#[doc(hidden)]
pub const CMD_PING: u8 = 0x3E;
#[doc(hidden)]
pub const CMD_SET_PIN_MODE: u8 = 0x50;
#[doc(hidden)]
pub const CMD_SET_DIGITAL_WRITE: u8 = 0x51;
#[doc(hidden)]
pub const CMD_SET_ANALOG_WRITE: u8 = 0x52;

// nina-fw `pinMode` values (mirror Arduino's INPUT/OUTPUT/INPUT_PULLUP).
#[doc(hidden)]
pub const PIN_INPUT: u8 = 0;
#[doc(hidden)]
pub const PIN_OUTPUT: u8 = 1;
#[doc(hidden)]
pub const PIN_INPUT_PULLUP: u8 = 2;

/// Pin direction for [`crate::Nina::pin_mode`] — mirrors Arduino's
/// `INPUT` / `OUTPUT` / `INPUT_PULLUP`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PinMode {
    /// High-impedance input.
    Input = 0,
    /// Push-pull output.
    Output = 1,
    /// Input with the chip's internal pull-up enabled.
    InputPullup = 2,
}

// Nano RP2040 Connect RGB LED — wired to NINA's spare GPIOs.
//
// Polarity: **active-LOW**. Write `0` (LOW) to turn a channel ON,
// `1` (HIGH) to turn it OFF. Writing 0 to all three = white.
/// NINA GPIO for the **red** channel of the onboard RGB LED (active-LOW).
pub const LED_R: u8 = 27;
/// NINA GPIO for the **green** channel of the onboard RGB LED (active-LOW).
pub const LED_G: u8 = 25;
/// NINA GPIO for the **blue** channel of the onboard RGB LED (active-LOW).
pub const LED_B: u8 = 26;

/// Transport mode passed to START_CLIENT.
#[doc(hidden)]
pub const TCP_MODE: u8 = 0;
#[doc(hidden)]
pub const UDP_MODE: u8 = 1;
#[doc(hidden)]
pub const TLS_MODE: u8 = 2;

/// TCP socket state — `enum tcp_state` in lwip.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SockState {
    Closed = 0,
    Listen = 1,
    SynSent = 2,
    SynRcvd = 3,
    Established = 4,
    FinWait1 = 5,
    FinWait2 = 6,
    CloseWait = 7,
    Closing = 8,
    LastAck = 9,
    TimeWait = 10,
    Unknown = 0xFF,
}

impl SockState {
    pub fn from_u8(b: u8) -> Self {
        match b {
            0 => Self::Closed,
            1 => Self::Listen,
            2 => Self::SynSent,
            3 => Self::SynRcvd,
            4 => Self::Established,
            5 => Self::FinWait1,
            6 => Self::FinWait2,
            7 => Self::CloseWait,
            8 => Self::Closing,
            9 => Self::LastAck,
            10 => Self::TimeWait,
            _ => Self::Unknown,
        }
    }
}

/// One parameter as it appears on the wire. Picks 8-bit vs 16-bit-BE length
/// encoding — the choice is per-parameter and hardcoded per command in
/// nina-fw's handler.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub enum Param<'a> {
    /// 8-bit length prefix. Used by most non-data commands.
    Short(&'a [u8]),
    /// 16-bit big-endian length prefix. Used for TCP payload, IP+port etc.
    Long(&'a [u8]),
}

/// `enc_type` enum from Arduino WiFi.h.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EncType {
    Wep = 5,
    Tkip = 2,
    Ccmp = 4, // WPA2-AES
    None = 7,
    Auto = 8,
    Other = 0,
}

impl EncType {
    pub fn from_u8(b: u8) -> Self {
        match b {
            5 => Self::Wep,
            2 => Self::Tkip,
            4 => Self::Ccmp,
            7 => Self::None,
            8 => Self::Auto,
            _ => Self::Other,
        }
    }
}
#[doc(hidden)]
pub const CMD_SCAN_NETWORKS: u8 = 0x27;
#[doc(hidden)]
pub const CMD_START_SCAN_NETWORKS: u8 = 0x36;
#[doc(hidden)]
pub const CMD_GET_FW_VERSION: u8 = 0x37;

/// `WL_STATUS` enum from Arduino WiFi.h — values match nina-fw.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum WlStatus {
    NoShield = 255,
    Idle = 0,
    NoSsidAvail = 1,
    ScanCompleted = 2,
    Connected = 3,
    ConnectFailed = 4,
    ConnectionLost = 5,
    Disconnected = 6,
    ApListening = 7,
    ApConnected = 8,
    ApFailed = 9,
}

impl WlStatus {
    pub fn from_u8(b: u8) -> Self {
        match b {
            0 => Self::Idle,
            1 => Self::NoSsidAvail,
            2 => Self::ScanCompleted,
            3 => Self::Connected,
            4 => Self::ConnectFailed,
            5 => Self::ConnectionLost,
            6 => Self::Disconnected,
            7 => Self::ApListening,
            8 => Self::ApConnected,
            9 => Self::ApFailed,
            _ => Self::NoShield,
        }
    }
}
