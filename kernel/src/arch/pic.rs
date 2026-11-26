use crate::arch::io::{io_wait, outb};

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x11;
const ICW4_8086: u8 = 0x01;
const PIC_EOI: u8 = 0x20;

/// Remaps the dual 8259 PICs to vectors `0x20..=0x2F` and unmasks IRQ0, IRQ1, IRQ2, and IRQ12.
pub fn init() {
    unsafe {
        outb(PIC1_CMD, ICW1_INIT);
        io_wait();
        outb(PIC2_CMD, ICW1_INIT);
        io_wait();

        // ICW2: Vector offsets (Master -> 0x20, Slave -> 0x28)
        outb(PIC1_DATA, 0x20);
        io_wait();
        outb(PIC2_DATA, 0x28);
        io_wait();

        // ICW3: Cascade wiring (Master IRQ2 bitmask 0x04, Slave identity 2)
        outb(PIC1_DATA, 0x04);
        io_wait();
        outb(PIC2_DATA, 0x02);
        io_wait();

        // ICW4: 8086/88 mode
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();

        // Unmask IRQ0 (PIT), IRQ1 (KBD), IRQ2 (cascade) on master; IRQ12 (slave line 4) on slave
        outb(PIC1_DATA, 0xF8);
        io_wait();
        outb(PIC2_DATA, 0xEF);
        io_wait();
    }
}

/// Sends End-Of-Interrupt to the slave (when `irq >= 8`) and master 8259 PICs.
#[inline]
pub fn send_eoi(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(PIC2_CMD, PIC_EOI);
        }
        outb(PIC1_CMD, PIC_EOI);
    }
}
