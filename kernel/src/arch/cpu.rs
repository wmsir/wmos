use core::arch::asm;

use crate::arch::io::{inb, io_wait, outb};

#[repr(C, packed)]
struct NullIdtr {
    limit: u16,
    base: u64,
}

/// Disables maskable hardware interrupts on the current CPU.
#[inline]
pub fn cli() {
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
}

/// Enables maskable hardware interrupts on the current CPU.
#[inline]
pub fn sti() {
    unsafe {
        asm!("sti", options(nomem, nostack));
    }
}

/// Halts the CPU until the next external interrupt arrives.
#[inline]
pub fn hlt() {
    unsafe {
        asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

/// Atomically enables interrupts and halts until the next interrupt arrives.
#[inline]
pub fn sti_and_hlt() {
    unsafe {
        asm!("sti", "hlt", options(nomem, nostack));
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

/// Reads the current CR3 page-table base register.
#[inline]
pub fn read_cr3() -> u64 {
    let cr3: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    cr3
}

/// Invalidates the TLB entry for the virtual page containing `addr`.
#[inline]
pub fn invlpg(addr: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags));
    }
}

/// Attempts a hardware reset via the 8042 PS/2 controller, falling back to a triple fault.
pub fn reboot() -> ! {
    cli();
    unsafe {
        for _ in 0..100_000 {
            if (inb(0x64) & 0x02) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(0x64, 0xFE);
        for _ in 0..10_000 {
            io_wait();
        }

        let null_idtr = NullIdtr { limit: 0, base: 0 };
        asm!(
            "lidt [{}]",
            "int3",
            in(reg) &null_idtr,
            options(nostack, noreturn)
        );
    }
}
