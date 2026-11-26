use core::arch::asm;

/// Reads a byte from the given x86 I/O port.
///
/// # Safety
/// The caller must ensure that reading from `port` is valid and has no unintended
/// hardware side effects.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!(
            "in al, dx",
            out("al") val,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    val
}

/// Writes a byte to the given x86 I/O port.
///
/// # Safety
/// The caller must ensure that writing `val` to `port` is valid for the target device.
#[inline]
pub unsafe fn outb(port: u16, val: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Waits one I/O cycle by writing to unused diagnostic port `0x80`.
#[allow(dead_code)]
#[inline]
pub fn io_wait() {
    unsafe {
        outb(0x80, 0);
    }
}
