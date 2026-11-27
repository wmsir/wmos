use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr::{copy_nonoverlapping, read_volatile, write_bytes, write_volatile};
use core::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};

use crate::arch::cpu;
use crate::drivers::pci;
use crate::mem::{pmm, vmm};

const E1000_MMIO_VIRT: u64 = 0xFFFF_B001_0000_0000;
const E1000_MMIO_PAGES: u64 = 32;

const REG_CTRL: u64 = 0x0000;
const REG_EERD: u64 = 0x0014;
const REG_IMC: u64 = 0x00D8;
const REG_RCTL: u64 = 0x0100;
const REG_TCTL: u64 = 0x0400;
const REG_TIPG: u64 = 0x0410;
const REG_RDBAL: u64 = 0x2800;
const REG_RDBAH: u64 = 0x2804;
const REG_RDLEN: u64 = 0x2808;
const REG_RDH: u64 = 0x2810;
const REG_RDT: u64 = 0x2818;
const REG_TDBAL: u64 = 0x3800;
const REG_TDBAH: u64 = 0x3804;
const REG_TDLEN: u64 = 0x3808;
const REG_TDH: u64 = 0x3810;
const REG_TDT: u64 = 0x3818;
const REG_MTA_BASE: u64 = 0x5200;
const REG_RAL0: u64 = 0x5400;
const REG_RAH0: u64 = 0x5404;

const RING_LEN: usize = 16;
const BUF_SIZE: usize = 2048;

/// Hardware metadata for an initialized Intel e1000/e1000e NIC.
#[derive(Clone, Copy)]
pub struct NicInfo {
    pub mac: [u8; 6],
    pub vendor_id: u16,
    pub device_id: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDesc {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TxDesc {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

struct E1000State {
    mmio_base: u64,
    info: NicInfo,
    rx_ring_phys: u64,
    tx_ring_phys: u64,
    rx_buf_frames: [u64; 8],
    tx_buf_frames: [u64; 8],
    rx_cur: usize,
    tx_cur: usize,
}

struct E1000Lock {
    locked: AtomicBool,
    inner: UnsafeCell<Option<E1000State>>,
}

unsafe impl Sync for E1000Lock {}

static E1000: E1000Lock = E1000Lock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

static TX_PACKETS: AtomicU64 = AtomicU64::new(0);
static RX_PACKETS: AtomicU64 = AtomicU64::new(0);

struct IrqGuard<'a> {
    lock: &'a E1000Lock,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a E1000Lock) -> Self {
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
    fn inner_mut(&mut self) -> &mut Option<E1000State> {
        unsafe { &mut *self.lock.inner.get() }
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
unsafe fn mmio_read(base: u64, reg: u64) -> u32 {
    unsafe { read_volatile((base + reg) as *const u32) }
}

#[inline]
unsafe fn mmio_write(base: u64, reg: u64, val: u32) {
    unsafe { write_volatile((base + reg) as *mut u32, val) }
}

fn read_eeprom_word(mmio_base: u64, addr: u8) -> Option<u16> {
    unsafe {
        mmio_write(mmio_base, REG_EERD, ((addr as u32) << 8) | 1);
        for _ in 0..100_000 {
            let val = mmio_read(mmio_base, REG_EERD);
            if (val & 0x10) != 0 {
                return Some((val >> 16) as u16);
            }
            core::hint::spin_loop();
        }
    }
    None
}

fn read_mac(mmio_base: u64) -> [u8; 6] {
    let ral = unsafe { mmio_read(mmio_base, REG_RAL0) };
    let rah = unsafe { mmio_read(mmio_base, REG_RAH0) };

    let mut mac = [
        (ral & 0xFF) as u8,
        ((ral >> 8) & 0xFF) as u8,
        ((ral >> 16) & 0xFF) as u8,
        ((ral >> 24) & 0xFF) as u8,
        (rah & 0xFF) as u8,
        ((rah >> 8) & 0xFF) as u8,
    ];

    if mac == [0; 6] || mac == [0xFF; 6] {
        for word in 0..3u8 {
            if let Some(w) = read_eeprom_word(mmio_base, word) {
                mac[(word as usize) * 2] = (w & 0xFF) as u8;
                mac[(word as usize) * 2 + 1] = ((w >> 8) & 0xFF) as u8;
            }
        }
    }

    mac
}

fn free_frames(frames: &[u64]) {
    for &phys in frames {
        if phys != 0 {
            pmm::free_frame(phys);
        }
    }
}

/// Probes PCI for an Intel e1000/e1000e NIC, maps MMIO, initializes RX/TX rings, and returns [`NicInfo`].
pub fn init() -> Option<NicInfo> {
    let dev = pci::devices().into_iter().find(|d| {
        d.vendor_id == 0x8086 && matches!(d.device_id, 0x100E | 0x10D3 | 0x100F | 0x1004)
    })?;

    let bar0_raw = dev.bar0 & !0xF;
    if bar0_raw == 0 {
        return None;
    }

    let cmd = pci::read_u32(dev.bus, dev.device, dev.function, 0x04);
    pci::write_u32(dev.bus, dev.device, dev.function, 0x04, cmd | 0x06);

    let bar0_phys = (bar0_raw & 0xFFFF_F000) as u64;
    let bar0_off = (bar0_raw & 0x0FFF) as u64;
    let mmio_flags = vmm::PRESENT | vmm::WRITABLE | vmm::WRITE_THROUGH | vmm::CACHE_DISABLE;

    for page in 0..E1000_MMIO_PAGES {
        let virt = E1000_MMIO_VIRT + page * 4096;
        if vmm::translate(virt).is_none()
            && vmm::map_page(virt, bar0_phys + page * 4096, mmio_flags).is_err()
        {
            return None;
        }
    }

    let mmio_base = E1000_MMIO_VIRT + bar0_off;
    unsafe {
        mmio_write(mmio_base, REG_IMC, 0xFFFF_FFFF);
        let ctrl = mmio_read(mmio_base, REG_CTRL);
        let next_ctrl = (ctrl & !((1 << 3) | (1 << 26) | (1 << 31))) | (1 << 6);
        mmio_write(mmio_base, REG_CTRL, next_ctrl);
        mmio_write(mmio_base, REG_IMC, 0xFFFF_FFFF);

        for idx in 0..128u64 {
            mmio_write(mmio_base, REG_MTA_BASE + idx * 4, 0);
        }
    }

    let mac = read_mac(mmio_base);

    let rx_ring_phys = pmm::alloc_frame()?;
    let Some(tx_ring_phys) = pmm::alloc_frame() else {
        pmm::free_frame(rx_ring_phys);
        return None;
    };

    let mut rx_buf_frames = [0u64; 8];
    for slot in &mut rx_buf_frames {
        match pmm::alloc_frame() {
            Some(p) => *slot = p,
            None => {
                free_frames(&rx_buf_frames);
                pmm::free_frame(tx_ring_phys);
                pmm::free_frame(rx_ring_phys);
                return None;
            }
        }
    }

    let mut tx_buf_frames = [0u64; 8];
    for slot in &mut tx_buf_frames {
        match pmm::alloc_frame() {
            Some(p) => *slot = p,
            None => {
                free_frames(&tx_buf_frames);
                free_frames(&rx_buf_frames);
                pmm::free_frame(tx_ring_phys);
                pmm::free_frame(rx_ring_phys);
                return None;
            }
        }
    }

    let hhdm = vmm::hhdm_offset();
    unsafe {
        write_bytes((rx_ring_phys + hhdm) as *mut u8, 0, 4096);
        write_bytes((tx_ring_phys + hhdm) as *mut u8, 0, 4096);
        for &p in &rx_buf_frames {
            write_bytes((p + hhdm) as *mut u8, 0, 4096);
        }
        for &p in &tx_buf_frames {
            write_bytes((p + hhdm) as *mut u8, 0, 4096);
        }

        let rx_ring = (rx_ring_phys + hhdm) as *mut RxDesc;
        for i in 0..RING_LEN {
            let buf_phys = rx_buf_frames[i / 2] + ((i % 2) * BUF_SIZE) as u64;
            write_volatile(
                rx_ring.add(i),
                RxDesc {
                    addr: buf_phys,
                    length: 0,
                    checksum: 0,
                    status: 0,
                    errors: 0,
                    special: 0,
                },
            );
        }

        let tx_ring = (tx_ring_phys + hhdm) as *mut TxDesc;
        for i in 0..RING_LEN {
            let buf_phys = tx_buf_frames[i / 2] + ((i % 2) * BUF_SIZE) as u64;
            write_volatile(
                tx_ring.add(i),
                TxDesc {
                    addr: buf_phys,
                    length: 0,
                    cso: 0,
                    cmd: 0,
                    status: 1,
                    css: 0,
                    special: 0,
                },
            );
        }

        fence(Ordering::Release);

        mmio_write(mmio_base, REG_RDBAL, (rx_ring_phys & 0xFFFF_FFFF) as u32);
        mmio_write(mmio_base, REG_RDBAH, (rx_ring_phys >> 32) as u32);
        mmio_write(mmio_base, REG_RDLEN, (RING_LEN * 16) as u32);
        mmio_write(mmio_base, REG_RDH, 0);
        mmio_write(mmio_base, REG_RDT, (RING_LEN - 1) as u32);
        mmio_write(mmio_base, REG_RCTL, (1 << 1) | (1 << 15) | (1 << 26));

        mmio_write(mmio_base, REG_TDBAL, (tx_ring_phys & 0xFFFF_FFFF) as u32);
        mmio_write(mmio_base, REG_TDBAH, (tx_ring_phys >> 32) as u32);
        mmio_write(mmio_base, REG_TDLEN, (RING_LEN * 16) as u32);
        mmio_write(mmio_base, REG_TDH, 0);
        mmio_write(mmio_base, REG_TDT, 0);
        mmio_write(
            mmio_base,
            REG_TCTL,
            (1 << 1) | (1 << 3) | (0x10 << 4) | (0x40 << 12),
        );
        mmio_write(mmio_base, REG_TIPG, 10 | (8 << 10) | (6 << 20));
    }

    let info = NicInfo {
        mac,
        vendor_id: dev.vendor_id,
        device_id: dev.device_id,
    };

    let mut guard = IrqGuard::acquire(&E1000);
    *guard.inner_mut() = Some(E1000State {
        mmio_base,
        info,
        rx_ring_phys,
        tx_ring_phys,
        rx_buf_frames,
        tx_buf_frames,
        rx_cur: 0,
        tx_cur: 0,
    });
    TX_PACKETS.store(0, Ordering::Release);
    RX_PACKETS.store(0, Ordering::Release);

    Some(info)
}

/// Returns [`NicInfo`] if the Intel e1000 NIC has been initialized.
pub fn info() -> Option<NicInfo> {
    let mut guard = IrqGuard::acquire(&E1000);
    guard.inner_mut().as_ref().map(|st| st.info)
}

/// Transmits a raw Ethernet frame (`14..=1514` bytes) via the e1000 TX descriptor ring.
pub fn send_frame(frame: &[u8]) -> Result<(), &'static str> {
    if frame.len() < 14 || frame.len() > 1514 {
        return Err("Invalid Ethernet frame length");
    }

    let mut guard = IrqGuard::acquire(&E1000);
    let state = guard.inner_mut().as_mut().ok_or("No e1000 NIC active")?;

    let hhdm = vmm::hhdm_offset();
    let idx = state.tx_cur;
    let tx_ring = (state.tx_ring_phys + hhdm) as *mut TxDesc;

    let mut ready = false;
    for _ in 0..100_000 {
        let desc = unsafe { read_volatile(tx_ring.add(idx)) };
        if (desc.status & 1) != 0 {
            ready = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !ready {
        return Err("e1000 TX ring full");
    }

    let buf_phys = state.tx_buf_frames[idx / 2] + ((idx % 2) * BUF_SIZE) as u64;
    let buf_virt = (buf_phys + hhdm) as *mut u8;

    unsafe {
        copy_nonoverlapping(frame.as_ptr(), buf_virt, frame.len());
        write_volatile(
            tx_ring.add(idx),
            TxDesc {
                addr: buf_phys,
                length: frame.len() as u16,
                cso: 0,
                cmd: 1 | 2 | 8, // EOP | IFCS | RS
                status: 0,
                css: 0,
                special: 0,
            },
        );
        fence(Ordering::Release);
        state.tx_cur = (idx + 1) % RING_LEN;
        mmio_write(state.mmio_base, REG_TDT, state.tx_cur as u32);
    }

    TX_PACKETS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Polls the e1000 RX ring for a received Ethernet frame, copying it into `buf`.
pub fn poll_recv(buf: &mut [u8]) -> Option<usize> {
    let mut guard = IrqGuard::acquire(&E1000);
    let state = guard.inner_mut().as_mut()?;

    let hhdm = vmm::hhdm_offset();
    let idx = state.rx_cur;
    let rx_ring = (state.rx_ring_phys + hhdm) as *mut RxDesc;

    fence(Ordering::Acquire);
    let desc = unsafe { read_volatile(rx_ring.add(idx)) };
    if (desc.status & 1) == 0 {
        return None;
    }

    let copy_len = (desc.length as usize).min(buf.len()).min(BUF_SIZE);
    let buf_phys = state.rx_buf_frames[idx / 2] + ((idx % 2) * BUF_SIZE) as u64;
    let buf_virt = (buf_phys + hhdm) as *const u8;

    unsafe {
        copy_nonoverlapping(buf_virt, buf.as_mut_ptr(), copy_len);
        write_volatile(
            rx_ring.add(idx),
            RxDesc {
                addr: buf_phys,
                length: 0,
                checksum: 0,
                status: 0,
                errors: 0,
                special: 0,
            },
        );
        fence(Ordering::Release);
        state.rx_cur = (idx + 1) % RING_LEN;
        mmio_write(state.mmio_base, REG_RDT, idx as u32);
    }

    RX_PACKETS.fetch_add(1, Ordering::Relaxed);
    Some(copy_len)
}

/// Returns `(tx_packets, rx_packets)` counters since NIC initialization.
#[inline]
pub fn stats() -> (u64, u64) {
    (
        TX_PACKETS.load(Ordering::Relaxed),
        RX_PACKETS.load(Ordering::Relaxed),
    )
}
