use core::arch::asm;

/// Disables maskable hardware interrupts on the current CPU.
#[inline]
pub fn cli() {
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
}

/// Halts the CPU until the next external interrupt arrives.
#[inline]
pub fn hlt() {
    unsafe {
        asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

/// Disables interrupts and halts the CPU indefinitely.
pub fn halt_loop() -> ! {
    cli();
    loop {
        hlt();
    }
}

/// Reads the 64-bit CPU time-stamp counter via `rdtsc`.
#[allow(dead_code)]
#[inline]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}
