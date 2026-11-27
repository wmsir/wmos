use alloc::vec::Vec;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::cpu;
use crate::arch::io::{inl, outl};

const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// Enumerated PCI function descriptor.
#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
    #[allow(dead_code)]
    pub irq_line: u8,
    pub bar5: u32,
}

struct PciLock {
    locked: AtomicBool,
    devices: UnsafeCell<Option<Vec<PciDevice>>>,
}

unsafe impl Sync for PciLock {}

static PCI: PciLock = PciLock {
    locked: AtomicBool::new(false),
    devices: UnsafeCell::new(None),
};

struct IrqGuard<'a> {
    lock: &'a PciLock,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a PciLock) -> Self {
        let rflags: u64;
        unsafe {
            asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags));
        }
        let saved_if = (rflags & 0x200) != 0;
        cpu::cli();
        while lock
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self { lock, saved_if }
    }

    #[inline]
    fn list_mut(&mut self) -> &mut Vec<PciDevice> {
        let slot = unsafe { &mut *self.lock.devices.get() };
        slot.get_or_insert_with(Vec::new)
    }
}

impl Drop for IrqGuard<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.saved_if {
            cpu::sti();
        }
    }
}

#[inline]
const fn config_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | (((dev as u32) & 0x1F) << 11)
        | (((func as u32) & 0x07) << 8)
        | ((offset as u32) & 0xFC)
}

/// Reads an aligned 32-bit dword from PCI configuration space via ports `0xCF8`/`0xCFC`.
pub fn read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr = config_addr(bus, dev, func, offset);
    unsafe {
        outl(PCI_CONFIG_ADDR, addr);
        inl(PCI_CONFIG_DATA)
    }
}

/// Writes an aligned 32-bit dword to PCI configuration space via ports `0xCF8`/`0xCFC`.
pub fn write_u32(bus: u8, dev: u8, func: u8, offset: u8, val: u32) {
    let addr = config_addr(bus, dev, func, offset);
    unsafe {
        outl(PCI_CONFIG_ADDR, addr);
        outl(PCI_CONFIG_DATA, val);
    }
}

fn read_device(bus: u8, dev: u8, func: u8) -> Option<PciDevice> {
    let reg00 = read_u32(bus, dev, func, 0x00);
    let vendor_id = (reg00 & 0xFFFF) as u16;
    if vendor_id == 0xFFFF {
        return None;
    }
    let device_id = ((reg00 >> 16) & 0xFFFF) as u16;

    let reg08 = read_u32(bus, dev, func, 0x08);
    let prog_if = ((reg08 >> 8) & 0xFF) as u8;
    let subclass = ((reg08 >> 16) & 0xFF) as u8;
    let class_code = ((reg08 >> 24) & 0xFF) as u8;

    let reg0c = read_u32(bus, dev, func, 0x0C);
    let header_type = ((reg0c >> 16) & 0xFF) as u8;

    let bar5 = if (header_type & 0x7F) == 0 {
        read_u32(bus, dev, func, 0x24)
    } else {
        0
    };

    let reg3c = read_u32(bus, dev, func, 0x3C);
    let irq_line = (reg3c & 0xFF) as u8;

    Some(PciDevice {
        bus,
        device: dev,
        function: func,
        vendor_id,
        device_id,
        class_code,
        subclass,
        prog_if,
        header_type,
        irq_line,
        bar5,
    })
}

/// Scans PCI buses `0..=7`, devices `0..32`, and returns the number of discovered functions.
pub fn init() -> usize {
    let mut found = Vec::new();

    for bus in 0u8..=7 {
        for dev in 0u8..32 {
            let Some(func0) = read_device(bus, dev, 0) else {
                continue;
            };
            let multi_fn = (func0.header_type & 0x80) != 0;
            found.push(func0);

            if multi_fn {
                for func in 1u8..8 {
                    if let Some(sub) = read_device(bus, dev, func) {
                        found.push(sub);
                    }
                }
            }
        }
    }

    let count = found.len();
    let mut guard = IrqGuard::acquire(&PCI);
    *guard.list_mut() = found;
    count
}

/// Returns a cloned snapshot of all enumerated PCI devices.
pub fn devices() -> Vec<PciDevice> {
    let mut guard = IrqGuard::acquire(&PCI);
    guard.list_mut().clone()
}

/// Returns a human-readable PCI class/subclass name.
pub fn class_name(class_code: u8, subclass: u8) -> &'static str {
    match (class_code, subclass) {
        (0x01, 0x01) => "IDE Storage",
        (0x01, 0x06) => "SATA AHCI",
        (0x01, 0x08) => "NVMe Storage",
        (0x01, _) => "Mass Storage",
        (0x02, 0x00) => "Ethernet",
        (0x02, _) => "Network",
        (0x03, 0x00) => "VGA Display",
        (0x03, _) => "Display",
        (0x04, 0x03) => "HD Audio",
        (0x04, _) => "Multimedia",
        (0x06, 0x00) => "Host Bridge",
        (0x06, 0x01) => "ISA Bridge",
        (0x06, 0x04) => "PCI Bridge",
        (0x06, _) => "Bridge",
        (0x08, _) => "System Periph",
        (0x0C, 0x03) => "USB Controller",
        (0x0C, 0x05) => "SMBus",
        (0x0C, _) => "Serial Bus",
        _ => "PCI Device",
    }
}

/// Returns a human-readable PCI vendor name.
pub fn vendor_name(vendor_id: u16) -> &'static str {
    match vendor_id {
        0x8086 => "Intel",
        0x1234 => "QEMU",
        0x1AF4 => "RedHat/VirtIO",
        0x1B36 => "QEMU/RedHat",
        0x15AD => "VMware",
        0x80EE => "VirtualBox",
        0x10EC => "Realtek",
        0x1022 => "AMD",
        0x10DE => "NVIDIA",
        _ => "Unknown",
    }
}
