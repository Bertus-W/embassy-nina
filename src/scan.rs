//! Wi-Fi scan result types returned by [`crate::Nina::scan_networks`].
//!
//! For just SSIDs, call [`Scan::iter`]. For full per-network info
//! (RSSI / encryption / channel / BSSID), call
//! [`crate::Nina::network_info`] with each index — it bundles all the
//! per-network queries into one [`NetworkInfo`].

use crate::proto::EncType;

/// Full info for a single scan result. Returned by
/// [`crate::Nina::network_info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NetworkInfo {
    /// RSSI in dBm.
    pub rssi: i8,
    /// Encryption type.
    pub enct: EncType,
    /// 802.11 channel.
    pub channel: u8,
    /// BSSID (MAC of the AP), canonical order.
    pub bssid: [u8; 6],
}

/// Borrowed view over a completed scan. Indexes into a caller-provided
/// scratch buffer that holds `[len_0, ssid_0, len_1, ssid_1, ...]`.
pub struct Scan<'a> {
    pub(crate) buf: &'a [u8],
    pub(crate) n: u8,
}

impl<'a> Scan<'a> {
    /// Number of networks discovered.
    pub fn len(&self) -> usize {
        self.n as usize
    }
    /// `true` if the scan returned no networks.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Iterate the SSID bytes of each discovered network. Items are raw
    /// byte slices — SSIDs are not guaranteed UTF-8.
    pub fn iter(&self) -> ScanIter<'_> {
        ScanIter {
            buf: self.buf,
            rem: self.n,
        }
    }
}

/// Iterator returned by [`Scan::iter`]. Yields each network's SSID bytes.
pub struct ScanIter<'a> {
    buf: &'a [u8],
    rem: u8,
}

impl<'a> Iterator for ScanIter<'a> {
    type Item = &'a [u8]; // SSID bytes (no NUL, no length prefix)
    fn next(&mut self) -> Option<Self::Item> {
        if self.rem == 0 || self.buf.is_empty() {
            return None;
        }
        let len = *self.buf.first()? as usize;
        if 1 + len > self.buf.len() {
            return None;
        }
        let ssid = &self.buf[1..1 + len];
        self.buf = &self.buf[1 + len..];
        self.rem -= 1;
        Some(ssid)
    }
}
