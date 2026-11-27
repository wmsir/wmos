use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::fmt::{self, Write};

use crate::arch::io::{inb, outb};
use crate::arch::{cpu, pit};
use crate::task;

const COM1: u16 = 0x3F8;
const COM2: u16 = 0x2F8;
const LSR_RX_READY: u8 = 0x01;
const LSR_TX_EMPTY: u8 = 0x20;

fn init_uart(base: u16) {
    unsafe {
        outb(base + 1, 0x00); // Disable all UART interrupts
        outb(base + 3, 0x80); // Enable DLAB to set baud rate divisor
        outb(base + 0, 0x01); // Divisor low byte: 1 (115200 baud)
        outb(base + 1, 0x00); // Divisor high byte: 0
        outb(base + 3, 0x03); // 8 bits, no parity, 1 stop bit (8N1), clear DLAB
        outb(base + 2, 0xC7); // Enable FIFO, clear TX/RX FIFOs, 14-byte threshold
        outb(base + 4, 0x0B); // IRQs enabled, RTS/DSR set

        // Perform loopback check and restore normal operational mode
        outb(base + 4, 0x1E);
        outb(base + 0, 0xAE);
        let _ = inb(base + 0);

        outb(base + 4, 0x0F);
    }
}

/// Initializes COM1 (`0x3F8`) at 115200 baud, 8N1, with FIFO enabled.
pub fn init() {
    init_uart(COM1);
}

/// Initializes COM2 (`0x2F8`) at 115200 baud, 8N1, for external MCU UART communication.
pub fn com2_init() {
    init_uart(COM2);
}

#[inline]
fn write_raw_port(base: u16, byte: u8) {
    unsafe {
        for _ in 0..100_000 {
            if (inb(base + 5) & LSR_TX_EMPTY) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(base, byte);
    }
}

/// Writes a single byte to COM1, translating `'\n'` to `"\r\n"`.
pub fn write_byte(b: u8) {
    if b == b'\n' {
        write_raw_port(COM1, b'\r');
    }
    write_raw_port(COM1, b);
}

/// Writes raw bytes directly to COM2 (`0x2F8`).
pub fn com2_write_bytes(data: &[u8]) {
    for &b in data {
        write_raw_port(COM2, b);
    }
}

/// Sends `cmd` (appending `'\n'` if absent) over COM2 and waits up to `timeout_ticks` for a reply line.
pub fn com2_exchange_line(cmd: &str, timeout_ticks: u64) -> Option<String> {
    unsafe {
        for _ in 0..256 {
            if (inb(COM2 + 5) & LSR_RX_READY) == 0 {
                break;
            }
            let _ = inb(COM2);
        }
    }

    com2_write_bytes(cmd.as_bytes());
    if !cmd.ends_with('\n') {
        com2_write_bytes(b"\n");
    }

    let start = pit::ticks();
    let mut rx = Vec::new();

    loop {
        let mut saw_newline = false;
        unsafe {
            while (inb(COM2 + 5) & LSR_RX_READY) != 0 {
                let b = inb(COM2);
                if b == b'\n' {
                    saw_newline = true;
                    break;
                }
                if b != b'\r' && rx.len() < 128 {
                    rx.push(b);
                }
            }
        }

        if saw_newline || rx.len() >= 128 {
            break;
        }
        if pit::ticks().wrapping_sub(start) >= timeout_ticks {
            break;
        }

        task::yield_now();
        let rflags: u64;
        unsafe {
            asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags));
        }
        if (rflags & 0x200) != 0 {
            cpu::hlt();
        }
    }

    if rx.is_empty() {
        return None;
    }

    let line = String::from_utf8_lossy(&rx);
    let trimmed = line.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(String::from(trimmed))
    }
}

/// `core::fmt::Write` adapter for COM1 serial output.
pub struct SerialWriter;

impl Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            write_byte(byte);
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    let mut writer = SerialWriter;
    let _ = writer.write_fmt(args);
}

/// Prints formatted text to the COM1 serial port without a trailing newline.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::arch::serial::_print(core::format_args!($($arg)*))
    };
}

/// Prints formatted text to the COM1 serial port with a trailing newline.
#[macro_export]
macro_rules! serial_println {
    () => {
        $crate::serial_print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::serial_print!("{}\n", core::format_args!($($arg)*))
    };
}
