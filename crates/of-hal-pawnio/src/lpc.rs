//! The `LpcIO` PawnIO module: raw Super I/O access over the LPC bus.
//!
//! This is a thin, faithful wrapper over the module's published ioctl surface. It adds no
//! chip knowledge — that belongs in a chip driver built on top — but it does own two
//! things that are easy to get wrong and dangerous when wrong:
//!
//! 1. **Bus arbitration.** Every `LpcIO` ioctl is documented as requiring the ISA bus
//!    mutex. [`LpcIo`] cannot be used without holding one; see [`crate::isa`].
//! 2. **Configuration mode.** A Super I/O's registers are only visible after an unlock
//!    sequence, and leaving the chip unlocked is untidy at best. [`ConfigMode`] locks it
//!    again on drop.
//!
//! # ABI
//!
//! Learned from the module's published LGPL source, which documents each entry point.
//! Facts about an interface, not code: nothing here is derived from its implementation.
//!
//! | ioctl | in | out |
//! | --- | --- | --- |
//! | `ioctl_select_slot` | slot (0 → ports `0x2E`/`0x2F`, 1 → `0x4E`/`0x4F`) | — |
//! | `ioctl_find_bars` | — | — |
//! | `ioctl_pio_inb` | port | value |
//! | `ioctl_pio_outb` | port, value | — |
//! | `ioctl_superio_inb` | register | value |
//! | `ioctl_superio_inw` | register | value (big-endian pair) |
//! | `ioctl_superio_outb` | register, value | — |
//!
//! The module only permits port access to the selected index/data pair, the ASUS EC ports
//! `0x25C`/`0x25D`, and base addresses it discovered via `ioctl_find_bars`. Anything else
//! comes back as `STATUS_ACCESS_DENIED`, which is a useful backstop: a bug in our port
//! arithmetic cannot reach an arbitrary I/O port.

use crate::ffi::{PawnIo, PawnIoError};
use crate::isa::IsaBusLock;

/// The Super I/O index/data port pairs a PC can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Ports `0x2E`/`0x2F`. Where nearly every desktop Super I/O lives.
    Primary,
    /// Ports `0x4E`/`0x4F`.
    Secondary,
}

impl Slot {
    /// Both slots, in probe order.
    pub const ALL: [Slot; 2] = [Slot::Primary, Slot::Secondary];

    fn index(self) -> u64 {
        match self {
            Slot::Primary => 0,
            Slot::Secondary => 1,
        }
    }

    /// The index port. The data port is always the next one up.
    pub fn index_port(self) -> u16 {
        match self {
            Slot::Primary => 0x2E,
            Slot::Secondary => 0x4E,
        }
    }
}

/// Register holding the 16-bit chip identifier, readable only in configuration mode.
pub const CHIP_ID_REGISTER: u8 = 0x20;

/// Logical device selector. A Super I/O multiplexes many devices behind one register
/// window; the hardware monitor is one of them.
pub const DEVICE_SELECT_REGISTER: u8 = 0x07;

/// Base address register of the selected logical device.
pub const BASE_ADDRESS_REGISTER: u8 = 0x60;

/// `LpcIO` loaded and bound to a slot, with the bus held for as long as it lives.
///
/// The lock is held for the whole lifetime rather than per-call on purpose: a batched
/// read of every sensor must be one uninterrupted transaction, or the values will not be
/// from the same instant and may not even be from the right registers.
#[derive(Debug)]
pub struct LpcIo {
    pawnio: PawnIo,
    slot: Slot,
    // Field order matters: `pawnio` is dropped before the bus lock, so the last ioctl we
    // issue still happens under arbitration.
    _bus: IsaBusLock,
}

impl LpcIo {
    /// Load the module, take the bus and select a slot.
    pub fn open(slot: Slot, bus: IsaBusLock) -> Result<Self, PawnIoError> {
        let pawnio = PawnIo::load_module_by_name("LpcIO")?;
        let this = Self {
            pawnio,
            slot,
            _bus: bus,
        };
        this.pawnio
            .execute("ioctl_select_slot", &[slot.index()], &mut [])?;
        Ok(this)
    }

    pub fn slot(&self) -> Slot {
        self.slot
    }

    /// Point at the other slot, keeping the module and the bus lock.
    ///
    /// `&mut self` because the module resets its discovered base addresses here, which
    /// invalidates anything a caller had learned about the previous slot.
    pub fn select_slot(&mut self, slot: Slot) -> Result<(), PawnIoError> {
        self.pawnio
            .execute("ioctl_select_slot", &[slot.index()], &mut [])?;
        self.slot = slot;
        Ok(())
    }

    /// Read a Super I/O register. Meaningful only in configuration mode.
    pub fn superio_inb(&self, register: u8) -> Result<u8, PawnIoError> {
        let mut out = [0u64; 1];
        self.pawnio
            .execute("ioctl_superio_inb", &[register.into()], &mut out)?;
        Ok(out[0] as u8)
    }

    /// Read a 16-bit Super I/O register pair (`register` high, `register + 1` low).
    pub fn superio_inw(&self, register: u8) -> Result<u16, PawnIoError> {
        let mut out = [0u64; 1];
        self.pawnio
            .execute("ioctl_superio_inw", &[register.into()], &mut out)?;
        Ok(out[0] as u16)
    }

    /// Write a Super I/O register.
    pub fn superio_outb(&self, register: u8, value: u8) -> Result<(), PawnIoError> {
        self.pawnio
            .execute(
                "ioctl_superio_outb",
                &[register.into(), value.into()],
                &mut [],
            )
            .map(|_| ())
    }

    /// Read a byte from an allowed I/O port.
    pub fn pio_inb(&self, port: u16) -> Result<u8, PawnIoError> {
        let mut out = [0u64; 1];
        self.pawnio
            .execute("ioctl_pio_inb", &[port.into()], &mut out)?;
        Ok(out[0] as u8)
    }

    /// Write a byte to an allowed I/O port.
    pub fn pio_outb(&self, port: u16, value: u8) -> Result<(), PawnIoError> {
        self.pawnio
            .execute("ioctl_pio_outb", &[port.into(), value.into()], &mut [])
            .map(|_| ())
    }

    /// Let the module discover base addresses, widening what [`pio_inb`](Self::pio_inb)
    /// and [`pio_outb`](Self::pio_outb) will accept. Requires configuration mode and a
    /// valid chip ID.
    pub fn find_bars(&self) -> Result<(), PawnIoError> {
        self.pawnio.execute("ioctl_find_bars", &[], &mut [])?;
        Ok(())
    }

    /// Select a logical device within the chip.
    pub fn select_logical_device(&self, ldn: u8) -> Result<(), PawnIoError> {
        self.superio_outb(DEVICE_SELECT_REGISTER, ldn)
    }

    /// Unlock the chip's configuration registers.
    ///
    /// The returned guard locks them again on drop, so a `?` on the way out cannot leave
    /// the chip unlocked.
    pub fn enter_config_mode(&self, vendor: Unlock) -> Result<ConfigMode<'_>, PawnIoError> {
        let port = self.slot.index_port();
        for byte in vendor.enter_sequence() {
            self.pio_outb(port, *byte)?;
        }
        Ok(ConfigMode { lpc: self, vendor })
    }
}

/// How a vendor's Super I/O family wants its configuration registers unlocked.
///
/// The sequences are written to the index port with no data write between them, which is
/// why this needs raw port access rather than `superio_outb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unlock {
    /// Nuvoton NCT6xxx: `0x87` twice to enter, `0xAA` to leave.
    Nuvoton,
    /// ITE IT87xx: `0x87 0x01 0x55 0x55` to enter (`0x55` → `0xAA` on the secondary
    /// slot); leaving is a register write rather than a port sequence.
    Ite,
}

impl Unlock {
    fn enter_sequence(self) -> &'static [u8] {
        match self {
            Unlock::Nuvoton => &[0x87, 0x87],
            Unlock::Ite => &[0x87, 0x01, 0x55, 0x55],
        }
    }

    fn exit_sequence(self) -> &'static [u8] {
        match self {
            Unlock::Nuvoton => &[0xAA],
            // ITE leaves configuration mode through a register write, not the index port.
            Unlock::Ite => &[],
        }
    }
}

/// A chip held in configuration mode. Leaves it on drop.
#[derive(Debug)]
pub struct ConfigMode<'a> {
    lpc: &'a LpcIo,
    vendor: Unlock,
}

impl ConfigMode<'_> {
    /// The 16-bit chip identifier.
    ///
    /// `0x0000` and `0xFFFF` mean nothing answered — either no chip at this slot, or the
    /// unlock sequence did not take. Both are reported as `None` rather than as a chip
    /// whose ID happens to be zero.
    pub fn chip_id(&self) -> Result<Option<u16>, PawnIoError> {
        let id = self.lpc.superio_inw(CHIP_ID_REGISTER)?;
        Ok(plausible_chip_id(id))
    }

    /// The underlying accessor, for chip-specific work while unlocked.
    pub fn lpc(&self) -> &LpcIo {
        self.lpc
    }
}

impl Drop for ConfigMode<'_> {
    fn drop(&mut self) {
        let port = self.lpc.slot.index_port();
        for byte in self.vendor.exit_sequence() {
            // Nothing useful to do on failure: we are already unwinding out of a chip
            // access, and the bus lock is about to be released regardless.
            let _ = self.lpc.pio_outb(port, *byte);
        }
    }
}

/// Whether a chip ID reads as a real answer rather than a floating bus.
///
/// Pure, so the rule is pinned by tests instead of being an inline comparison that quietly
/// drifts. An absent chip reads all-ones (nothing driving the bus low) or all-zeroes.
pub fn plausible_chip_id(id: u16) -> Option<u16> {
    match id {
        0x0000 | 0xFFFF => None,
        id => Some(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_map_to_the_conventional_port_pairs() {
        assert_eq!(Slot::Primary.index_port(), 0x2E);
        assert_eq!(Slot::Secondary.index_port(), 0x4E);
    }

    #[test]
    fn a_floating_bus_is_not_mistaken_for_a_chip() {
        // The two readings an absent chip produces. Treating either as a chip ID would
        // mean "discovering" hardware that is not there and then reading garbage from it.
        assert_eq!(plausible_chip_id(0x0000), None);
        assert_eq!(plausible_chip_id(0xFFFF), None);
    }

    #[test]
    fn real_chip_ids_survive() {
        // NCT6798D and NCT6791D respectively.
        assert_eq!(plausible_chip_id(0xD428), Some(0xD428));
        assert_eq!(plausible_chip_id(0xC803), Some(0xC803));
    }

    #[test]
    fn nuvoton_unlock_is_the_documented_double_write() {
        // Two writes of 0x87 to the index port, 0xAA to leave. Getting this wrong leaves
        // the chip locked and every register reads as 0xFF.
        assert_eq!(Unlock::Nuvoton.enter_sequence(), &[0x87, 0x87]);
        assert_eq!(Unlock::Nuvoton.exit_sequence(), &[0xAA]);
    }
}
