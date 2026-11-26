use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;

const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct IstStack([u8; DOUBLE_FAULT_STACK_SIZE]);

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static DOUBLE_FAULT_STACK: SyncCell<IstStack> =
    SyncCell(UnsafeCell::new(IstStack([0; DOUBLE_FAULT_STACK_SIZE])));

#[repr(C, packed)]
struct Tss {
    reserved1: u32,
    rsp: [u64; 3],
    reserved2: u64,
    ist: [u64; 7],
    reserved3: u64,
    reserved4: u16,
    iomap_base: u16,
}

static TSS: SyncCell<Tss> = SyncCell(UnsafeCell::new(Tss {
    reserved1: 0,
    rsp: [0; 3],
    reserved2: 0,
    ist: [0; 7],
    reserved3: 0,
    reserved4: 0,
    iomap_base: size_of::<Tss>() as u16,
}));

static GDT: SyncCell<[u64; 5]> = SyncCell(UnsafeCell::new([
    0x0000_0000_0000_0000, // 0x00: Null
    0x00AF_9A00_0000_FFFF, // 0x08: Kernel 64-bit Code
    0x00CF_9200_0000_FFFF, // 0x10: Kernel Data
    0,                     // 0x18: TSS low
    0,                     // 0x20: TSS high
]));

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// Initializes the 64-bit GDT and TSS with IST1 double-fault stack, and reloads segments.
pub fn init() {
    unsafe {
        let stack_base = DOUBLE_FAULT_STACK.0.get() as u64;
        let stack_top = stack_base + DOUBLE_FAULT_STACK_SIZE as u64;

        let tss = &mut *TSS.0.get();
        tss.ist[0] = stack_top;
        tss.iomap_base = size_of::<Tss>() as u16;

        let tss_base = TSS.0.get() as u64;
        let tss_limit = (size_of::<Tss>() - 1) as u64;

        let tss_low = (tss_limit & 0xFFFF)
            | ((tss_base & 0x00FF_FFFF) << 16)
            | (0x89u64 << 40)
            | (((tss_limit >> 16) & 0xF) << 48)
            | (((tss_base >> 24) & 0xFF) << 56);
        let tss_high = (tss_base >> 32) & 0xFFFF_FFFF;

        let gdt = &mut *GDT.0.get();
        gdt[3] = tss_low;
        gdt[4] = tss_high;

        let gdtr = Gdtr {
            limit: (size_of::<[u64; 5]>() - 1) as u16,
            base: GDT.0.get() as u64,
        };

        asm!(
            "lgdt [{gdtr}]",
            "push 0x08",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            "mov ax, 0x18",
            "ltr ax",
            gdtr = in(reg) &gdtr,
            out("rax") _,
            options(preserves_flags)
        );
    }
}
