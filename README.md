# embassy-nina

[![Crates.io](https://img.shields.io/crates/v/embassy-nina.svg)](https://crates.io/crates/embassy-nina)
[![docs.rs](https://docs.rs/embassy-nina/badge.svg)](https://docs.rs/embassy-nina)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Async, `no_std` Rust driver for the u-blox **NINA-W102** WiFi module running
the factory **Arduino WiFiNINA** firmware (the chip shipped on the Arduino
Nano RP2040 Connect, Nano 33 IoT, MKR WiFi 1010, UNO WiFi Rev2, …).

The driver speaks the WiFiNINA SPI protocol directly — there is no host-side
TCP/IP stack, no `smoltcp`, no `embassy-net`. NINA runs its own onboard
network stack and we just shuttle commands and bytes to it. That means a few
KB of flash and almost no RAM on the host MCU, plus full TLS via the
NINA-side mbedTLS image.

Built on `embedded-hal-async` so it works under any executor; tested
end-to-end under embassy on the RP2040.

> **Not affiliated with the [embassy-rs](https://github.com/embassy-rs/embassy)
> project.** The `embassy-` prefix here signals that the crate targets the
> embassy / `embedded-hal-async` trait ecosystem — it is not an official
> embassy-rs crate.

## At a glance

```rust,ignore
use embassy_nina::{proto, Nina, NinaStack, PinMode};

let mut nina = Nina::new(spi, cs, ack, rst, boot);
nina.init().await?;
nina.connect("my-ssid", "my-psk").await?;   // bundles retry + status wait

// HTTP via the embedded-nal-async trait surface — plugs into reqwless etc.
let chip  = embassy_sync::mutex::Mutex::<NoopRawMutex, _>::new(nina);
let stack = NinaStack::new(&chip);
let mut sock = stack.connect(addr).await?;
sock.write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n").await?;
```

## Surface

| Trait / type                          | Where                                  |
|---------------------------------------|----------------------------------------|
| Low-level chip API                    | [`Nina`]                               |
| `embedded_io_async::{Read, Write}`    | [`NinaTcpSocket`] (mut-borrow flavour) |
| `embedded_nal_async::TcpConnect`      | [`NinaStack`]                          |
| `embedded_nal_async::Dns`             | [`NinaStack`]                          |

`NinaStack` plugs into anything that wants an `embedded-nal-async` impl
(`reqwless` 0.14, `rust-mqtt`, etc).

### What works (verified on hardware against nina-fw 2.0.0 and 3.0.1)

- **STA mode** — `connect`, `wait_for_connected`, `disconnect`, `status`,
  `ip` / `ip_config`, `mac_addr`, `current_ssid` /
  `get_current_rssi` / `get_current_bssid` / `get_current_enct` /
  `get_reason_code`.
- **AP mode** — `start_ap_open` / `start_ap_wpa`. Chip auto-assigns
  192.168.4.1.
- **Scan** — `scan_networks` + `scan_rssi` / `scan_enct` / `scan_channel` /
  `scan_bssid`, plus `network_info(index)` to grab everything in one call.
- **DNS** — `dns_lookup` (also via [`embedded_nal_async::Dns`]).
- **TCP client** — `embedded-io-async::Read/Write` on [`NinaTcpSocket`]
  and `embedded-nal-async::TcpConnect` on [`NinaStack`]. Works with
  `reqwless` 0.14 for HTTP (200 OK end-to-end).
- **TLS** — `NinaStack::new_tls()` + `connect_hostname(host, port)`
  performs the TLS handshake with SNI on-chip. Verified against
  `https://example.com` (Cloudflare-fronted) returning a full 200
  response.
- **UDP** — `udp_bind`, one-shot `udp_send(sock, ip, port, data)`, plus
  the three lower-level `udp_begin_packet` / `udp_write` /
  `udp_end_packet` primitives. Receive via `tcp_avail` + `tcp_recv` (same
  opcodes as TCP); peer info via `udp_remote`. Verified with an NTP
  roundtrip against `pool.ntp.org:123`.
- **TCP server** — `tcp_listen` + `tcp_accept`; the accepted client
  socket plugs into the regular `tcp_recv` / `tcp_send` / `tcp_close`
  flow.
- **Ping** — ICMP via `ping`, returns RTT in ms.
- **NINA GPIO passthrough** — `pin_mode` (typed `PinMode` enum),
  `digital_write`, `digital_read`, `analog_write`, `analog_read`. On the
  Nano RP2040 Connect this is the only way to drive the onboard RGB LED
  (see `proto::LED_R` / `LED_G` / `LED_B` — active-LOW).
- **Network config** — `set_hostname` (DHCP advertising),
  `set_net_config` (static IPv4), `set_dns_config` (manual DNS servers),
  `set_power_save`.
- **Chip info** — `get_fw_version`, `get_temperature` (ESP32
  internal °C).

### Not yet

- mDNS (no nina-fw opcode; would layer on top of UDP).
- WPA2-Enterprise (opcodes 0x4A–0x4F exist; bindings not written).
- File operations / OTA (opcodes exist in nina-fw 3.x).
- HTTPS via `reqwless` directly — `reqwless` 0.14 wants to layer
  `embedded-tls` on top of `TcpConnect`, which would double-encrypt over
  the chip's TLS. Workable path is `NinaStack::new_tls` +
  `connect_hostname` and raw HTTP/1.0 bytes over the trait socket — see
  `blinky/src/bin/reqwless_demo.rs` in the workspace for both flows
  side-by-side.

## Wiring (Arduino Nano RP2040 Connect)

| Driver field | NINA pin    | RP2040 GP | Notes                          |
|--------------|-------------|-----------|--------------------------------|
| `bus` MOSI   | NINA_MOSI   | GP11      | SPI1 TX                        |
| `bus` MISO   | NINA_MISO   | GP8       | SPI1 RX                        |
| `bus` SCK    | NINA_SCK    | GP14      | SPI1 SCK (alt pin)             |
| `cs`         | NINA_CS     | GP9       | Software-driven, idle HIGH     |
| `ack`        | SPIWIFI_ACK | GP10      | LOW = ready, HIGH = busy       |
| `rst`        | NINA_RESETN | GP3       | Active-low                     |
| `boot`       | NINA_GPIO0  | GP2       | HIGH at reset → normal boot. Pass [`NoPin`] if you manage GP2 yourself. |

SPI runs at 8 MHz, mode 0. See `proto::SPI_FREQ_HZ`.

## Full example

```rust,ignore
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::spi::{Config as SpiConfig, Spi};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use core::net::{IpAddr, SocketAddr};
use embassy_nina::{proto, Nina, NinaStack};

let mut cfg = SpiConfig::default();
cfg.frequency = proto::SPI_FREQ_HZ;
let spi  = Spi::new(p.SPI1, p.PIN_14, p.PIN_11, p.PIN_8, p.DMA_CH0, p.DMA_CH1, Irqs, cfg);
let cs   = Output::new(p.PIN_9, Level::High);
let ack  = Input::new (p.PIN_10, Pull::None);
let rst  = Output::new(p.PIN_3, Level::High);
let boot = Output::new(p.PIN_2, Level::High);

let mut nina = Nina::new(spi, cs, ack, rst, boot);
nina.init().await?;
nina.connect("my-ssid", "my-psk").await?;       // up to 5 attempts, 20 s wait each

// Hand to the trait facade.
let chip: Mutex<NoopRawMutex, _> = Mutex::new(nina);
let stack = NinaStack::new(&chip);

// DNS + TCP via embedded-nal-async.
let IpAddr::V4(ip) = stack.get_host_by_name("example.com", AddrType::IPv4).await? else { unreachable!() };
let mut sock = stack.connect(SocketAddr::new(ip.into(), 80)).await?;
sock.write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n").await?;
let mut buf = [0u8; 2048];
while let Ok(n) = sock.read(&mut buf).await {
    if n == 0 { break; }
    // use &buf[..n]
}
```

More reference code in [`examples/`](examples/) and full working binaries
under `blinky/src/bin/` in the [workspace
repo](https://github.com/Bertus-W/embassy-nina).

## Known gotchas

Things that took a while to figure out and that the driver now handles for
you — useful to know if you extend the protocol surface:

- **4-byte command alignment.** Every command must be padded to a 4-byte
  boundary on the wire, or multi-parameter commands silently truncate.
- **`START_CLIENT_TCP` length encoding.** nina-fw 2.x parses all four
  parameters with 8-bit length prefixes (despite what the Arduino host
  driver writes for one of them); port is big-endian because the handler
  applies `ntohs()`.
- **`GET_DATABUF_TCP` `want` field is little-endian.** No `ntohs()` on
  this one — send `want.to_le_bytes()`. Sending BE caps every read at 8
  bytes.
- **`tcp_state` is destructive.** nina-fw's `getClientStateTcp` sets the
  slot's type to "free" whenever its `connected()` check fails — so
  polling state on a freshly-accepted socket during the handshake window
  destroys the slot. Don't poll on accepted server-side sockets; use a
  short fixed wait instead. (Documented inline on the method.)
- **`tcp_send` chunking.** Single-call writes larger than ~100 bytes are
  unreliable on the chip side for repeated server-side connections. The
  driver auto-chunks at 64 B internally to match Arduino's
  `client.println` cadence.
- **USB CDC on the host MCU blocks.** Not a NINA issue, but related: if
  your demo logs to USB CDC and the host isn't reading, the CDC TX
  endpoint fills after ~3 packets and your accept loop wedges. Skip the
  log on the hot path, or use a non-blocking write.

## Cargo features

| Feature | Effect                                          |
|---------|-------------------------------------------------|
| `defmt` | `defmt::Format` on public enums and structs     |

## Examples

Reference code lives in `examples/` (compile against your own
RP2040/SAMD/AVR-based application crate; pin numbers are
RP2040-specific). For a full working build setup see the `blinky/`
member of the workspace repo.

## Minimum supported Rust version

This crate tracks stable Rust. CI runs against `stable`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual-licensed as above without any additional terms or
conditions.
