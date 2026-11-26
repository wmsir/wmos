use crate::arch::io::{inb, outb};

const PS2_DATA: u16 = 0x60;
const PS2_STATUS_CMD: u16 = 0x64;
const TIMEOUT_ITERS: usize = 100_000;

#[inline]
fn wait_input_clear() -> bool {
    unsafe {
        for _ in 0..TIMEOUT_ITERS {
            if (inb(PS2_STATUS_CMD) & 0x02) == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}

#[inline]
fn wait_output_full() -> bool {
    unsafe {
        for _ in 0..TIMEOUT_ITERS {
            if (inb(PS2_STATUS_CMD) & 0x01) != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}

fn flush() {
    unsafe {
        for _ in 0..64 {
            if (inb(PS2_STATUS_CMD) & 0x01) == 0 {
                break;
            }
            let _ = inb(PS2_DATA);
        }
    }
}

fn send_cmd(cmd: u8) {
    if wait_input_clear() {
        unsafe {
            outb(PS2_STATUS_CMD, cmd);
        }
    }
}

fn send_mouse_cmd(cmd: u8) {
    send_cmd(0xD4);
    if wait_input_clear() {
        unsafe {
            outb(PS2_DATA, cmd);
        }
    }
    if wait_output_full() {
        unsafe {
            let _ = inb(PS2_DATA);
        }
    }
}

/// Initializes the 8042 PS/2 controller, enabling keyboard (IRQ1) and mouse streaming (IRQ12).
pub fn init() {
    flush();

    // Disable both PS/2 ports during configuration
    send_cmd(0xAD);
    send_cmd(0xA7);
    flush();

    // Read controller configuration byte
    send_cmd(0x20);
    let mut cfg = if wait_output_full() {
        unsafe { inb(PS2_DATA) }
    } else {
        0x47
    };

    // Enable IRQ1 (bit 0), IRQ12 (bit 1), Set-1 translation (bit 6), clear clock-disable (bits 4..5)
    cfg = (cfg | 0x01 | 0x02 | 0x40) & !0x30;

    send_cmd(0x60);
    if wait_input_clear() {
        unsafe {
            outb(PS2_DATA, cfg);
        }
    }

    // Re-enable keyboard and auxiliary mouse ports
    send_cmd(0xAE);
    send_cmd(0xA8);

    // Configure mouse defaults and enable packet streaming
    send_mouse_cmd(0xF6);
    send_mouse_cmd(0xF4);

    flush();
}
