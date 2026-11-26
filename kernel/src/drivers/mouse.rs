use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::arch::io::inb;

/// Current pointer coordinates and button bitmask (`bit 0 = left`, `bit 1 = right`, `bit 2 = middle`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MouseState {
    pub x: usize,
    pub y: usize,
    pub buttons: u8,
}

static WIDTH: AtomicUsize = AtomicUsize::new(1024);
static HEIGHT: AtomicUsize = AtomicUsize::new(768);
static PACKED_STATE: AtomicU64 = AtomicU64::new(0);
static MOUSE_DIRTY: AtomicBool = AtomicBool::new(false);
static MOUSE_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

static PACKET_CYCLE: AtomicU8 = AtomicU8::new(0);
static PACKET_BYTE0: AtomicU8 = AtomicU8::new(0);
static PACKET_BYTE1: AtomicU8 = AtomicU8::new(0);

#[inline]
fn pack_state(x: usize, y: usize, buttons: u8) -> u64 {
    ((x as u64) & 0xFF_FFFF)
        | (((y as u64) & 0xFF_FFFF) << 24)
        | (((buttons as u64) & 0xFF) << 48)
}

#[inline]
fn unpack_state(raw: u64) -> MouseState {
    MouseState {
        x: (raw & 0xFF_FFFF) as usize,
        y: ((raw >> 24) & 0xFF_FFFF) as usize,
        buttons: ((raw >> 48) & 0xFF) as u8,
    }
}

/// Configures screen bounds and initializes the mouse cursor at `(w * 56 / 100, h * 45 / 100)`.
pub fn set_bounds(width: usize, height: usize) {
    let w = width.max(1);
    let h = height.max(1);
    WIDTH.store(w, Ordering::Relaxed);
    HEIGHT.store(h, Ordering::Relaxed);

    let init_x = (w * 56 / 100).min(w - 1);
    let init_y = (h * 45 / 100).min(h - 1);
    PACKED_STATE.store(pack_state(init_x, init_y, 0), Ordering::Release);
    PACKET_CYCLE.store(0, Ordering::Relaxed);
    MOUSE_DIRTY.store(false, Ordering::Release);
}

/// Handles PS/2 mouse IRQ12 by decoding 3-byte packets and updating clamped cursor coordinates.
pub fn on_irq() {
    let status = unsafe { inb(0x64) };
    if (status & 0x21) != 0x21 {
        return;
    }

    let byte = unsafe { inb(0x60) };
    MOUSE_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);

    let cycle = PACKET_CYCLE.load(Ordering::Relaxed);
    match cycle {
        0 => {
            // Require always-1 sync bit (0x08) and no X/Y overflow bits (0xC0) so stale
            // ACK bytes like 0xFA cannot hijack packet alignment.
            if (byte & 0xC8) == 0x08 {
                PACKET_BYTE0.store(byte, Ordering::Relaxed);
                PACKET_CYCLE.store(1, Ordering::Relaxed);
            }
        }
        1 => {
            PACKET_BYTE1.store(byte, Ordering::Relaxed);
            PACKET_CYCLE.store(2, Ordering::Relaxed);
        }
        _ => {
            PACKET_CYCLE.store(0, Ordering::Relaxed);
            let b0 = PACKET_BYTE0.load(Ordering::Relaxed);
            let b1 = PACKET_BYTE1.load(Ordering::Relaxed);
            let b2 = byte;

            if (b0 & 0xC0) != 0 {
                return;
            }

            let dx = (b1 as i32) - (((b0 as i32) & 0x10) << 4);
            let dy = (b2 as i32) - (((b0 as i32) & 0x20) << 3);

            let cur = unpack_state(PACKED_STATE.load(Ordering::Acquire));
            let max_x = WIDTH.load(Ordering::Relaxed).saturating_sub(1) as i32;
            let max_y = HEIGHT.load(Ordering::Relaxed).saturating_sub(1) as i32;

            let nx = (cur.x as i32 + dx).clamp(0, max_x) as usize;
            let ny = (cur.y as i32 - dy).clamp(0, max_y) as usize;
            let buttons = b0 & 0x07;

            PACKED_STATE.store(pack_state(nx, ny, buttons), Ordering::Release);
            MOUSE_DIRTY.store(true, Ordering::Release);
        }
    }
}

/// Returns the latest atomically sampled [`MouseState`].
#[inline]
pub fn state() -> MouseState {
    unpack_state(PACKED_STATE.load(Ordering::Acquire))
}

/// Returns `true` if a new mouse packet has arrived since the last [`take_dirty`] call.
#[inline]
pub fn is_dirty() -> bool {
    MOUSE_DIRTY.load(Ordering::Acquire)
}

/// Clears and returns whether the mouse state had pending updates.
#[inline]
pub fn take_dirty() -> bool {
    MOUSE_DIRTY.swap(false, Ordering::AcqRel)
}

/// Returns total PS/2 mouse IRQ12 interrupts handled.
#[inline]
pub fn irq_count() -> u64 {
    MOUSE_IRQ_COUNT.load(Ordering::Relaxed)
}
