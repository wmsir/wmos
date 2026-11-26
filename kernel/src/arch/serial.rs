use core::fmt::{self, Write};

use crate::arch::io::{inb, outb};

const COM1: u16 = 0x3F8;
const LSR_TX_EMPTY: u8 = 0x20;

/// Initializes COM1 (`0x3F8`) at 115200 baud, 8N1, with FIFO enabled.
pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // Disable all UART interrupts
        outb(COM1 + 3, 0x80); // Enable DLAB to set baud rate divisor
        outb(COM1 + 0, 0x01); // Divisor low byte: 1 (115200 baud)
        outb(COM1 + 1, 0x00); // Divisor high byte: 0
        outb(COM1 + 3, 0x03); // 8 bits, no parity, 1 stop bit (8N1), clear DLAB
        outb(COM1 + 2, 0xC7); // Enable FIFO, clear TX/RX FIFOs, 14-byte threshold
        outb(COM1 + 4, 0x0B); // IRQs enabled, RTS/DSR set

        // Perform loopback check and then restore normal operational mode so both
        // QEMU and real hardware work reliably even if loopback is unemulated.
        outb(COM1 + 4, 0x1E);
        outb(COM1 + 0, 0xAE);
        let _ = inb(COM1 + 0);

        outb(COM1 + 4, 0x0F);
    }
}

#[inline]
fn write_raw(byte: u8) {
    unsafe {
        for _ in 0..100_000 {
            if (inb(COM1 + 5) & LSR_TX_EMPTY) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(COM1, byte);
    }
}

/// Writes a single byte to COM1, translating `'\n'` to `"\r\n"`.
pub fn write_byte(b: u8) {
    if b == b'\n' {
        write_raw(b'\r');
    }
    write_raw(b);
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
