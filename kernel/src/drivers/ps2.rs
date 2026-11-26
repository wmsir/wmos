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
    let base_cfg = if wait_output_full() {
        unsafe { inb(PS2_DATA) }
    } else {
        0x44
    };

    // Keep IRQ1/IRQ12 disabled while enabling ports and configuring the mouse so
    // command ACKs (0xFA) never latch a stale IRQ into the 8259 PIC before sti().
    let init_cfg = (base_cfg | 0x40) & !0x33;
    send_cmd(0x60);
    if wait_input_clear() {
        unsafe {
            outb(PS2_DATA, init_cfg);
        }
    }

    // Re-enable keyboard and auxiliary mouse ports
    send_cmd(0xAE);
    send_cmd(0xA8);

    // Configure mouse defaults and enable packet streaming while IRQs are masked
    send_mouse_cmd(0xF6);
    send_mouse_cmd(0xF4);
    flush();

    // Now enable IRQ1 (bit 0) and IRQ12 (bit 1) in the 8042 controller configuration
    let final_cfg = init_cfg | 0x03;
    send_cmd(0x60);
    if wait_input_clear() {
        unsafe {
            outb(PS2_DATA, final_cfg);
        }
    }

    flush();
}
