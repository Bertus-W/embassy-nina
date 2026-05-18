# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-05-18

### Fixed

- README links to `examples/` and `PROTOCOL.md` now use absolute GitHub
  URLs so they resolve on docs.rs, where the README is embedded as the
  crate's top-level rustdoc via `#![doc = include_str!(...)]`.

## [0.1.0] - 2026-05-18

### Added

- Initial public release.
- `Nina` chip-level driver covering: WPA2-PSK STA, AP (open + WPA2), DNS,
  TCP client + server, TLS via on-chip mbedTLS with SNI (`new_tls` +
  `connect_hostname`), UDP, ping, NINA GPIO passthrough, network
  config (hostname, static IP, DNS, power save), chip info (firmware
  version, temperature, RSSI, BSSID, encryption type, last disconnect
  reason).
- `embedded-io-async::{Read, Write}` impls (`NinaTcpSocket`).
- `embedded-nal-async::{TcpConnect, Dns}` impls (`NinaStack`).
- High-level helpers: `Nina::connect(ssid, psk)` (retry + status wait),
  `Nina::wait_for_connected`, `Nina::udp_send`, `Nina::network_info`.
- Type-safe `Socket` newtype for chip-side socket handles.
- Typed `PinMode` enum.
- Verified on hardware against nina-fw 2.0.0 and 3.0.1.

[Unreleased]: https://github.com/Bertus-W/embassy-nina/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Bertus-W/embassy-nina/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Bertus-W/embassy-nina/releases/tag/v0.1.0
