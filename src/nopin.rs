//! Zero-sized [`OutputPin`] stand-in.
//!
//! Use [`NoPin`] for the `boot` slot of [`crate::Nina`] when you do **not**
//! want the driver to drive `NINA_GPIO0`. This is the recommended setup if
//! you've wired the pin to flip to an input (Hi-Z) after reset so nina-fw
//! can use it as the async-data IRQ line — manage the real pin yourself
//! before/after calling [`crate::Nina::init`].
//!
//! ```ignore
//! use embassy_nina::{Nina, NoPin};
//! // ... drive GP2 (NINA_GPIO0) HIGH yourself, then flip to input.
//! let mut nina = Nina::new(spi, cs, ack, rst, NoPin);
//! nina.init().await?;
//! ```

use core::convert::Infallible;
use embedded_hal::digital::{ErrorType, OutputPin};

/// No-op pin. All operations succeed and do nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPin;

impl ErrorType for NoPin {
    type Error = Infallible;
}

impl OutputPin for NoPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
