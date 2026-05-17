# embassy-nina examples

These are reference implementations of common embassy-nina usage patterns.
They're written for the **Arduino Nano RP2040 Connect** (RP2040 host MCU
driving the on-board NINA-W102). The pin numbers, SPI peripheral, and
`Irqs` binding are RP2040-specific — adapt them for your board's HAL.

To actually build and flash one of these, copy it into a `bin/` directory
in your own RP2040 application crate, add `embassy-nina` to your
`Cargo.toml`, and `cargo run` against `thumbv6m-none-eabi`. See the
sibling `blinky/` member in this workspace for a full working build setup.

## Files

| File | What it shows |
|---|---|
| `led_http_server_rp2040.rs` | Minimal HTTP server controlling the on-NINA RGB LED. Uses only the public embassy-nina API — no helper modules. The smallest end-to-end example. |
