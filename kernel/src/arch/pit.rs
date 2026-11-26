use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::io::outb;

/// Programmable Interval Timer tick frequency in Hz.
pub const HZ: u64 = 100;

const PIT_BASE_HZ: u64 = 1_193_182;
const PIT_CH0_DATA: u16 = 0x40;
const PIT_CMD: u16 = 0x43;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Initializes PIT channel 0 in square-wave mode at [`HZ`] (100 Hz).
pub fn init() {
    let divisor = (PIT_BASE_HZ / HZ) as u16;
    unsafe {
        outb(PIT_CMD, 0x36);
        outb(PIT_CH0_DATA, (divisor & 0xFF) as u8);
        outb(PIT_CH0_DATA, ((divisor >> 8) & 0xFF) as u8);
    }
}

/// Called from the IRQ0 handler to advance the monotonic tick counter.
#[inline]
pub fn on_irq() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Returns total PIT ticks since initialization.
#[inline]
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Returns whole elapsed seconds derived from PIT ticks.
#[inline]
pub fn uptime_secs() -> u64 {
    ticks() / HZ
}
