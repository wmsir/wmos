use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::arch::io::inb;

/// Decoded keyboard input events consumed by the desktop and shell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Char(u8),
    Backspace,
    Enter,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
}

const QUEUE_CAP: usize = 128;

struct RingBuffer {
    head: AtomicUsize,
    tail: AtomicUsize,
    buf: [UnsafeCell<Option<KeyEvent>>; QUEUE_CAP],
}

unsafe impl Sync for RingBuffer {}

const EMPTY_SLOT: UnsafeCell<Option<KeyEvent>> = UnsafeCell::new(None);

static KEY_QUEUE: RingBuffer = RingBuffer {
    head: AtomicUsize::new(0),
    tail: AtomicUsize::new(0),
    buf: [EMPTY_SLOT; QUEUE_CAP],
};

static KEY_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
static LEFT_SHIFT: AtomicBool = AtomicBool::new(false);
static RIGHT_SHIFT: AtomicBool = AtomicBool::new(false);
static CAPS_LOCK: AtomicBool = AtomicBool::new(false);
static EXTENDED_E0: AtomicBool = AtomicBool::new(false);

const NORMAL_MAP: [u8; 89] = [
    0, 0, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 0, 0,
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', 0, 0,
    b'a', b's', b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0, b'\\',
    b'z', b'x', b'c', b'v', b'b', b'n', b'm', b',', b'.', b'/', 0, b'*', 0, b' ',
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    b'7', b'8', b'9', b'-', b'4', b'5', b'6', b'+', b'1', b'2', b'3', b'0', b'.',
    0, 0, 0, 0, 0,
];

const SHIFT_MAP: [u8; 89] = [
    0, 0, b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'_', b'+', 0, 0,
    b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'{', b'}', 0, 0,
    b'A', b'S', b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', b'~', 0, b'|',
    b'Z', b'X', b'C', b'V', b'B', b'N', b'M', b'<', b'>', b'?', 0, b'*', 0, b' ',
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    b'7', b'8', b'9', b'-', b'4', b'5', b'6', b'+', b'1', b'2', b'3', b'0', b'.',
    0, 0, 0, 0, 0,
];

#[inline]
fn push_event(ev: KeyEvent) {
    let head = KEY_QUEUE.head.load(Ordering::Relaxed);
    let next = (head + 1) & (QUEUE_CAP - 1);
    if next != KEY_QUEUE.tail.load(Ordering::Acquire) {
        unsafe {
            *KEY_QUEUE.buf[head].get() = Some(ev);
        }
        KEY_QUEUE.head.store(next, Ordering::Release);
    }
}

/// Handles PS/2 keyboard IRQ1 by decoding Set 1 scancodes into [`KeyEvent`]s.
pub fn on_irq() {
    let status = unsafe { inb(0x64) };
    if (status & 0x01) == 0 || (status & 0x20) != 0 {
        return;
    }

    let scancode = unsafe { inb(0x60) };
    KEY_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);

    if scancode == 0xE0 {
        EXTENDED_E0.store(true, Ordering::Relaxed);
        return;
    }

    if EXTENDED_E0.swap(false, Ordering::Relaxed) {
        if (scancode & 0x80) != 0 {
            return;
        }
        match scancode {
            0x48 => push_event(KeyEvent::Up),
            0x50 => push_event(KeyEvent::Down),
            0x4B => push_event(KeyEvent::Left),
            0x4D => push_event(KeyEvent::Right),
            _ => {}
        }
        return;
    }

    match scancode {
        0x2A => {
            LEFT_SHIFT.store(true, Ordering::Relaxed);
            return;
        }
        0xAA => {
            LEFT_SHIFT.store(false, Ordering::Relaxed);
            return;
        }
        0x36 => {
            RIGHT_SHIFT.store(true, Ordering::Relaxed);
            return;
        }
        0xB6 => {
            RIGHT_SHIFT.store(false, Ordering::Relaxed);
            return;
        }
        0x3A => {
            let prev = CAPS_LOCK.load(Ordering::Relaxed);
            CAPS_LOCK.store(!prev, Ordering::Relaxed);
            return;
        }
        _ => {}
    }

    if (scancode & 0x80) != 0 {
        return;
    }

    match scancode {
        0x01 => push_event(KeyEvent::Escape),
        0x0E => push_event(KeyEvent::Backspace),
        0x0F => push_event(KeyEvent::Tab),
        0x1C => push_event(KeyEvent::Enter),
        code => {
            let idx = code as usize;
            if idx < NORMAL_MAP.len() {
                let shift = LEFT_SHIFT.load(Ordering::Relaxed) || RIGHT_SHIFT.load(Ordering::Relaxed);
                let caps = CAPS_LOCK.load(Ordering::Relaxed);
                let base_ch = NORMAL_MAP[idx];
                if base_ch != 0 {
                    let ch = if base_ch.is_ascii_lowercase() {
                        if shift ^ caps {
                            SHIFT_MAP[idx]
                        } else {
                            base_ch
                        }
                    } else if shift {
                        SHIFT_MAP[idx]
                    } else {
                        base_ch
                    };
                    if ch != 0 {
                        push_event(KeyEvent::Char(ch));
                    }
                }
            }
        }
    }
}

/// Pops the oldest pending [`KeyEvent`] from the SPSC ring buffer.
pub fn pop_event() -> Option<KeyEvent> {
    let tail = KEY_QUEUE.tail.load(Ordering::Relaxed);
    if tail == KEY_QUEUE.head.load(Ordering::Acquire) {
        return None;
    }
    let ev = unsafe { (*KEY_QUEUE.buf[tail].get()).take() };
    KEY_QUEUE
        .tail
        .store((tail + 1) & (QUEUE_CAP - 1), Ordering::Release);
    ev
}

/// Returns `true` if there is at least one buffered keyboard event waiting.
#[inline]
pub fn has_pending() -> bool {
    KEY_QUEUE.tail.load(Ordering::Acquire) != KEY_QUEUE.head.load(Ordering::Acquire)
}

/// Returns total PS/2 keyboard IRQ1 interrupts handled.
#[inline]
pub fn irq_count() -> u64 {
    KEY_IRQ_COUNT.load(Ordering::Relaxed)
}
