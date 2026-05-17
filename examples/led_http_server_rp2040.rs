//! Minimal end-user-style HTTP server for the onboard RGB LED.
//!
//! Imports `embassy-nina` as a third-party library and goes straight from
//! `embassy_rp::init` to a `tcp_listen` accept loop. No USB CDC, no dev
//! helpers — what a downstream user of the crate would actually write.
//! Status: solid-on green LED (GP6) once the HTTP server is ready.
//!
//! Endpoints (all `GET`, all return JSON `{"r":0|1,"g":0|1,"b":0|1}`):
//!   /                  current state
//!   /r/0  /r/1         red off / on
//!   /g/0  /g/1         green
//!   /b/0  /b/1         blue
//!   /all/off /all/on   every channel
//!   /rgb/RGB           three bits like /rgb/101

#![no_std]
#![no_main]

use core::fmt::Write;

use embassy_executor::Spawner;
use embassy_nina::{proto, Nina, PinMode};
use embassy_rp::bind_interrupts;
use embassy_rp::dma::InterruptHandler as DmaIrqHandler;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1};
use embassy_rp::spi::{Config as SpiConfig, Spi};
use embassy_time::{Duration, Timer};
use heapless::String as HString;
use panic_halt as _;

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 => DmaIrqHandler<DMA_CH0>, DmaIrqHandler<DMA_CH1>;
});

// ---- User config -----------------------------------------------------------
const SSID: &str = "YOUR-SSID";
const PSK: &str = "YOUR-PSK";
const PORT: u16 = 8080;

#[derive(Clone, Copy, Default)]
struct LedState {
    r: bool,
    g: bool,
    b: bool,
}

impl LedState {
    fn as_json(&self) -> HString<48> {
        let mut s: HString<48> = HString::new();
        let _ = write!(
            &mut s,
            "{{\"r\":{},\"g\":{},\"b\":{}}}",
            self.r as u8, self.g as u8, self.b as u8
        );
        s
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut status_led = Output::new(p.PIN_6, Level::Low);

    // ---- SPI + embassy-nina --------------------------------------------
    let mut spi_cfg = SpiConfig::default();
    spi_cfg.frequency = proto::SPI_FREQ_HZ;
    let spi = Spi::new(
        p.SPI1, p.PIN_14, p.PIN_11, p.PIN_8, p.DMA_CH0, p.DMA_CH1, Irqs, spi_cfg,
    );
    let cs = Output::new(p.PIN_9, Level::High);
    let ack = Input::new(p.PIN_10, Pull::None);
    let rst = Output::new(p.PIN_3, Level::High);
    let boot = Output::new(p.PIN_2, Level::High);
    let mut nina = Nina::new(spi, cs, ack, rst, boot);

    if nina.init().await.is_err() {
        return;
    }

    // Configure on-NINA RGB LED pins (active-LOW), all off.
    for pin in [proto::LED_R, proto::LED_G, proto::LED_B] {
        let _ = nina.pin_mode(pin, PinMode::Output).await;
        let _ = nina.digital_write(pin, 1).await;
    }
    let mut state = LedState::default();

    // High-level connect: up to 5 attempts, 20 s wait each.
    if nina.connect(SSID, PSK).await.is_err() {
        return;
    }

    // Open a listen socket on PORT.
    let listen_sock = match nina.tcp_open_socket().await {
        Ok(s) => s,
        Err(_) => return,
    };
    if nina.tcp_listen(listen_sock, PORT).await.is_err() {
        return;
    }

    // Signal "ready" via the on-board green LED.
    status_led.set_high();

    let mut req_buf = [0u8; 256];
    loop {
        let client_sock = match nina.tcp_accept(listen_sock).await {
            Ok(Some(s)) => s,
            _ => {
                Timer::after(Duration::from_millis(50)).await;
                continue;
            }
        };
        // Don't poll `tcp_state` on a freshly-accepted slot — it poisons.
        Timer::after(Duration::from_millis(80)).await;

        // Drain the request line so we know the path.
        let mut total = 0usize;
        let deadline = embassy_time::Instant::now() + Duration::from_millis(300);
        while embassy_time::Instant::now() < deadline && total < req_buf.len() {
            if nina.tcp_avail(client_sock).await.unwrap_or(0) > 0 {
                let n = nina
                    .tcp_recv(client_sock, &mut req_buf[total..])
                    .await
                    .unwrap_or(0);
                if n == 0 {
                    break;
                }
                total += n;
                if req_buf[..total].windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            } else {
                Timer::after(Duration::from_millis(5)).await;
            }
        }

        let path = extract_path(&req_buf[..total]);
        let (ok, new_state) = handle(path, state);
        if ok {
            state = new_state;
            let _ = nina.digital_write(proto::LED_R, u8::from(!state.r)).await;
            let _ = nina.digital_write(proto::LED_G, u8::from(!state.g)).await;
            let _ = nina.digital_write(proto::LED_B, u8::from(!state.b)).await;
        }

        // `tcp_send` auto-chunks now — one call for the whole response.
        let status_line = if ok { "200 OK" } else { "404 Not Found" };
        let body = state.as_json();
        let mut resp: HString<256> = HString::new();
        let _ = write!(&mut resp,
            "HTTP/1.0 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status_line, body.len(), body);
        let _ = nina.tcp_send(client_sock, resp.as_bytes()).await;

        Timer::after(Duration::from_millis(20)).await;
        let _ = nina.tcp_close(client_sock).await;
    }
}

fn extract_path(req: &[u8]) -> &[u8] {
    let after_get = match req.strip_prefix(b"GET ") {
        Some(s) => s,
        None => return b"/",
    };
    let end = after_get
        .iter()
        .position(|&b| b == b' ' || b == b'\r' || b == b'\n');
    match end {
        Some(i) => &after_get[..i],
        None => after_get,
    }
}

fn handle(path: &[u8], cur: LedState) -> (bool, LedState) {
    let mut s = cur;
    match path {
        b"/" => (true, s),
        b"/r/1" => {
            s.r = true;
            (true, s)
        }
        b"/r/0" => {
            s.r = false;
            (true, s)
        }
        b"/g/1" => {
            s.g = true;
            (true, s)
        }
        b"/g/0" => {
            s.g = false;
            (true, s)
        }
        b"/b/1" => {
            s.b = true;
            (true, s)
        }
        b"/b/0" => {
            s.b = false;
            (true, s)
        }
        b"/all/on" => {
            s.r = true;
            s.g = true;
            s.b = true;
            (true, s)
        }
        b"/all/off" => {
            s.r = false;
            s.g = false;
            s.b = false;
            (true, s)
        }
        p if p.starts_with(b"/rgb/") && p.len() == 8 => {
            let parse = |b: u8| match b {
                b'0' => Some(false),
                b'1' => Some(true),
                _ => None,
            };
            match (parse(p[5]), parse(p[6]), parse(p[7])) {
                (Some(r), Some(g), Some(b)) => {
                    s.r = r;
                    s.g = g;
                    s.b = b;
                    (true, s)
                }
                _ => (false, cur),
            }
        }
        _ => (false, cur),
    }
}
