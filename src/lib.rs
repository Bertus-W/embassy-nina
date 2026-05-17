#![no_std]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]

pub mod error;
pub mod nal;
pub mod nopin;
pub mod proto;
pub mod scan;
pub mod tcp;

mod driver;

pub use driver::{IpConfig, Nina, Socket};
pub use error::Error;
pub use nal::{NinaSocket, NinaStack};
pub use nopin::NoPin;
pub use proto::{EncType, PinMode, SockState, WlStatus};
pub use scan::{NetworkInfo, Scan, ScanIter};
pub use tcp::NinaTcpSocket;
