# embassy-nina

[![Crates.io](https://img.shields.io/crates/v/embassy-nina.svg)](https://crates.io/crates/embassy-nina)
[![docs.rs](https://docs.rs/embassy-nina/badge.svg)](https://docs.rs/embassy-nina)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Async `no_std` Rust driver for the u-blox **NINA-W102** WiFi module
running the factory **Arduino WiFiNINA** firmware.

The chip runs its own TCP/IP stack and on-chip TLS — your MCU just
shuttles bytes over SPI. That keeps the host footprint tiny (a few KB
of flash, almost no RAM) and gives you full HTTPS without `embedded-tls`
or `smoltcp`.

> Not affiliated with the [embassy-rs](https://github.com/embassy-rs/embassy)
> project. The `embassy-` prefix indicates that the crate targets the
> `embedded-hal-async` trait ecosystem.

## Supported hardware

Any board carrying a NINA-W102 with stock Arduino WiFiNINA firmware:

- Arduino Nano RP2040 Connect *(primary test target)*
- Arduino Nano 33 IoT
- Arduino MKR WiFi 1010
- Arduino UNO WiFi Rev2

Verified against nina-fw **2.0.0** and **3.0.1**.

## Quick start

```sh
cargo add embassy-nina
```

```rust,ignore
use embassy_nina::Nina;

let mut nina = Nina::new(spi, cs, ack, rst, boot);
nina.init().await?;
nina.connect("my-ssid", "my-psk").await?;

// HTTP via embedded-nal-async — works with reqwless, rust-mqtt, etc.
let chip  = Mutex::<NoopRawMutex, _>::new(nina);
let stack = embassy_nina::NinaStack::new(&chip);
let mut sock = stack.connect(addr).await?;
sock.write_all(b"GET / HTTP/1.0\r\n\r\n").await?;
```

A complete RP2040 example — including pin wiring, SPI config, and a
runnable HTTP fetch — lives in [`examples/`](examples/).

## What it does

- **STA + AP mode** — WPA2-PSK association, scanning with RSSI / channel
  / BSSID / encryption, AP with open or WPA2.
- **TCP client** — `embedded_io_async::{Read, Write}` and
  `embedded_nal_async::TcpConnect`. Drop-in for `reqwless` 0.14.
- **TCP server** — `tcp_listen` + `tcp_accept`.
- **TLS** — on-chip mbedTLS handshake with SNI. Verified against
  `https://example.com`.
- **UDP** — bind, one-shot send, receive. Verified with NTP.
- **DNS** — `embedded_nal_async::Dns`.
- **Ping** — ICMP with RTT.
- **NINA GPIO passthrough** — drives the onboard RGB LED on the Nano
  RP2040 Connect (the only way to reach it).
- **Network config** — hostname, static IP, manual DNS, power save.

Full API reference: [docs.rs/embassy-nina](https://docs.rs/embassy-nina).

## Cargo features

| Feature | Effect                                      |
|---------|---------------------------------------------|
| `defmt` | `defmt::Format` on public enums and structs |

## MSRV

Rust **1.87**. Tracks stable.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

PRs welcome. For protocol-level notes (wire-format quirks, nina-fw
behaviour you'll hit when extending the opcode surface), see
[`PROTOCOL.md`](PROTOCOL.md).

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual-licensed as above without any additional terms or
conditions.
