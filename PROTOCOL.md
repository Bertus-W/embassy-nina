# Protocol notes

This document is for people extending `embassy-nina` — adding new
opcodes, fixing wire-format bugs, or porting to a new board. The
end-user [README](README.md) covers normal usage.

## Wiring (Arduino Nano RP2040 Connect)

| Driver field | NINA pin    | RP2040 GP | Notes                                  |
|--------------|-------------|-----------|----------------------------------------|
| `bus` MOSI   | NINA_MOSI   | GP11      | SPI1 TX                                |
| `bus` MISO   | NINA_MISO   | GP8       | SPI1 RX                                |
| `bus` SCK    | NINA_SCK    | GP14      | SPI1 SCK (alt pin)                     |
| `cs`         | NINA_CS     | GP9       | Software-driven, idle HIGH             |
| `ack`        | SPIWIFI_ACK | GP10      | LOW = ready, HIGH = busy               |
| `rst`        | NINA_RESETN | GP3       | Active-low                             |
| `boot`       | NINA_GPIO0  | GP2       | HIGH at reset → normal boot. Pass `NoPin` if you manage GP2 yourself. |

SPI runs at 8 MHz, mode 0. See `proto::SPI_FREQ_HZ`.

## Wire-format gotchas

Things that took a while to figure out and that the driver now handles
internally. Useful to know if you extend the opcode surface or chase a
mysterious truncation.

### 4-byte command alignment

Every command must be padded to a 4-byte boundary on the wire. Without
the padding, multi-parameter commands silently truncate the tail.

### `START_CLIENT_TCP` length encoding

nina-fw 2.x parses all four parameters with 8-bit length prefixes —
even though the Arduino host driver writes one of them as 16-bit.
Port is big-endian because the chip-side handler applies `ntohs()`.

### `GET_DATABUF_TCP` `want` field is little-endian

No `ntohs()` on this one — send `want.to_le_bytes()`. Sending big-endian
caps every read at 8 bytes.

### `tcp_state` is destructive on freshly-accepted sockets

nina-fw's `getClientStateTcp` frees the slot (`socketTypes[s] = 255`)
whenever its `connected()` check fails. On a freshly-accepted socket
the handshake window is wide enough that a poll lands inside it and
destroys the slot.

Don't poll state on server-side accepted sockets. Use a short fixed
wait instead. (The driver documents this inline on the affected
methods.)

### `tcp_send` chunking

Single-call writes larger than ~100 bytes are unreliable on the chip
side for repeated server-side connections. The driver auto-chunks at
64 B internally — matches Arduino's `client.println` cadence and
clears it up.

### USB CDC on the host MCU blocks

Not a NINA issue, but it kept showing up during development: if your
demo logs to USB CDC and the host isn't reading, the CDC TX endpoint
fills after ~3 packets and your accept loop wedges. Skip the log on
the hot path, or use a non-blocking write.

## Not yet implemented

- **mDNS** — no nina-fw opcode; would layer on top of UDP.
- **WPA2-Enterprise** — opcodes 0x4A–0x4F exist in nina-fw; bindings
  not written.
- **File ops / OTA** — opcodes exist in nina-fw 3.x.
- **HTTPS via `reqwless` directly** — `reqwless` 0.14 wants to layer
  `embedded-tls` over `TcpConnect`, which would double-encrypt over
  the chip's TLS. The workable path is `NinaStack::new_tls` +
  `connect_hostname` and raw HTTP/1.0 bytes over the trait socket. See
  `blinky/src/bin/reqwless_demo.rs` in the workspace repo for both
  flows side-by-side.

## References

- [arduino/nina-fw](https://github.com/arduino/nina-fw) — firmware source
- [arduino-libraries/WiFiNINA](https://github.com/arduino-libraries/WiFiNINA) — Arduino host-side driver (reference for the wire protocol)
