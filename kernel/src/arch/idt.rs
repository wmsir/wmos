use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::size_of;

use crate::arch::{cpu, pic, pit};
use crate::drivers::{keyboard, mouse};
use crate::serial_println;

const IDT_ENTRIES: usize = 256;
const KERNEL_CS: u16 = 0x08;
const GATE_INTERRUPT: u8 = 0x8E;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    fn new(handler: u64, ist: u8) -> Self {
        Self {
            offset_low: (handler & 0xFFFF) as u16,
            selector: KERNEL_CS,
            ist,
            type_attr: GATE_INTERRUPT,
            offset_mid: ((handler >> 16) & 0xFFFF) as u16,
            offset_high: ((handler >> 32) & 0xFFFF_FFFF) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, align(16))]
struct IdtTable([IdtEntry; IDT_ENTRIES]);

struct SyncIdt(UnsafeCell<IdtTable>);
unsafe impl Sync for SyncIdt {}

static IDT: SyncIdt = SyncIdt(UnsafeCell::new(IdtTable([IdtEntry::missing(); IDT_ENTRIES])));

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

/// CPU + pushed register frame passed by `isr_common` to [`isr_dispatch`].
#[repr(C)]
pub struct InterruptFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

global_asm!(
    r#"
.section .text,"ax",@progbits

.macro ISR_NOERR num
.global isr_stub_\num
isr_stub_\num:
    push 0
    push \num
    jmp isr_common
.endm

.macro ISR_ERR num
.global isr_stub_\num
isr_stub_\num:
    push \num
    jmp isr_common
.endm

ISR_NOERR 0
ISR_NOERR 1
ISR_NOERR 2
ISR_NOERR 3
ISR_NOERR 4
ISR_NOERR 5
ISR_NOERR 6
ISR_NOERR 7
ISR_ERR   8
ISR_NOERR 9
ISR_ERR   10
ISR_ERR   11
ISR_ERR   12
ISR_ERR   13
ISR_ERR   14
ISR_NOERR 15
ISR_NOERR 16
ISR_ERR   17
ISR_NOERR 18
ISR_NOERR 19
ISR_NOERR 20
ISR_ERR   21
ISR_NOERR 22
ISR_NOERR 23
ISR_NOERR 24
ISR_NOERR 25
ISR_NOERR 26
ISR_NOERR 27
ISR_NOERR 28
ISR_ERR   29
ISR_ERR   30
ISR_NOERR 31

ISR_NOERR 32
ISR_NOERR 33
ISR_NOERR 34
ISR_NOERR 35
ISR_NOERR 36
ISR_NOERR 37
ISR_NOERR 38
ISR_NOERR 39
ISR_NOERR 40
ISR_NOERR 41
ISR_NOERR 42
ISR_NOERR 43
ISR_NOERR 44
ISR_NOERR 45
ISR_NOERR 46
ISR_NOERR 47

.global isr_stub_unknown
isr_stub_unknown:
    push 0
    push 255
    jmp isr_common

isr_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    cld
    call isr_dispatch

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16
    iretq

.section .rodata,"a",@progbits
.align 8
.global isr_stub_table
isr_stub_table:
    .quad isr_stub_0
    .quad isr_stub_1
    .quad isr_stub_2
    .quad isr_stub_3
    .quad isr_stub_4
    .quad isr_stub_5
    .quad isr_stub_6
    .quad isr_stub_7
    .quad isr_stub_8
    .quad isr_stub_9
    .quad isr_stub_10
    .quad isr_stub_11
    .quad isr_stub_12
    .quad isr_stub_13
    .quad isr_stub_14
    .quad isr_stub_15
    .quad isr_stub_16
    .quad isr_stub_17
    .quad isr_stub_18
    .quad isr_stub_19
    .quad isr_stub_20
    .quad isr_stub_21
    .quad isr_stub_22
    .quad isr_stub_23
    .quad isr_stub_24
    .quad isr_stub_25
    .quad isr_stub_26
    .quad isr_stub_27
    .quad isr_stub_28
    .quad isr_stub_29
    .quad isr_stub_30
    .quad isr_stub_31
    .quad isr_stub_32
    .quad isr_stub_33
    .quad isr_stub_34
    .quad isr_stub_35
    .quad isr_stub_36
    .quad isr_stub_37
    .quad isr_stub_38
    .quad isr_stub_39
    .quad isr_stub_40
    .quad isr_stub_41
    .quad isr_stub_42
    .quad isr_stub_43
    .quad isr_stub_44
    .quad isr_stub_45
    .quad isr_stub_46
    .quad isr_stub_47
    .quad isr_stub_unknown
"#
);

extern "C" {
    static isr_stub_table: [u64; 49];
}

/// Dispatches hardware interrupts and CPU exceptions from [`isr_common`].
#[no_mangle]
pub extern "C" fn isr_dispatch(frame: &InterruptFrame) {
    match frame.vector {
        32 => {
            pit::on_irq();
            pic::send_eoi(0);
        }
        33 => {
            keyboard::on_irq();
            pic::send_eoi(1);
        }
        44 => {
            mouse::on_irq();
            pic::send_eoi(12);
        }
        32..=47 => {
            pic::send_eoi((frame.vector - 32) as u8);
        }
        0..=31 => {
            serial_println!(
                "[wmos] EXCEPTION vec={} err={:#x} rip={:#x} rsp={:#x}",
                frame.vector,
                frame.error_code,
                frame.rip,
                frame.rsp
            );
            cpu::halt_loop();
        }
        _ => {}
    }
}

/// Populates all 256 IDT gates (setting IST1 on double fault vector 8) and executes `lidt`.
pub fn init() {
    unsafe {
        let table = &mut (*IDT.0.get()).0;
        let stubs = &isr_stub_table;

        for vec in 0..48 {
            let ist = if vec == 8 { 1 } else { 0 };
            table[vec] = IdtEntry::new(stubs[vec], ist);
        }
        for entry in table.iter_mut().skip(48) {
            *entry = IdtEntry::new(stubs[48], 0);
        }

        let idtr = Idtr {
            limit: (size_of::<IdtTable>() - 1) as u16,
            base: IDT.0.get() as u64,
        };

        asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags));
    }
}
