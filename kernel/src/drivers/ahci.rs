use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr::{copy_nonoverlapping, read_volatile, write_bytes, write_volatile};
use core::sync::atomic::{fence, AtomicBool, Ordering};

use crate::arch::cpu;
use crate::drivers::pci;
use crate::mem::{pmm, vmm};

const AHCI_MMIO_VIRT: u64 = 0xFFFF_B000_0000_0000;
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0C;

const PORT_BASE_OFF: u64 = 0x100;
const PORT_STRIDE: u64 = 0x80;

const PX_CLB: u64 = 0x00;
const PX_CLBU: u64 = 0x04;
const PX_FB: u64 = 0x08;
const PX_FBU: u64 = 0x0C;
const PX_IS: u64 = 0x10;
const PX_CMD: u64 = 0x18;
const PX_TFD: u64 = 0x20;
const PX_SIG: u64 = 0x24;
const PX_SSTS: u64 = 0x28;
const PX_SERR: u64 = 0x30;
const PX_CI: u64 = 0x38;

const SATA_SIG_ATA: u32 = 0x0000_0101;
const ATA_CMD_IDENTIFY: u8 = 0xEC;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xEA;

/// Metadata for the active AHCI SATA disk.
#[derive(Clone, Copy)]
pub struct DiskInfo {
    pub port: usize,
    pub sectors: u64,
    pub model: [u8; 40],
}

impl DiskInfo {
    /// Returns the trimmed ASCII model name string.
    pub fn model_str(&self) -> &str {
        core::str::from_utf8(&self.model)
            .unwrap_or("SATA Disk")
            .trim_end()
    }
}

#[repr(C)]
struct HbaCmdHeader {
    flags: u16,
    prdtl: u16,
    prdbc: u32,
    ctba: u32,
    ctbau: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct HbaPrdtEntry {
    dba: u32,
    dbau: u32,
    reserved: u32,
    dbc: u32,
}

struct AhciState {
    port_base: u64,
    ctrl_phys: u64,
    dma_phys: u64,
    info: DiskInfo,
}

struct AhciLock {
    locked: AtomicBool,
    inner: UnsafeCell<Option<AhciState>>,
}

unsafe impl Sync for AhciLock {}

static AHCI: AhciLock = AhciLock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

struct IrqGuard<'a> {
    lock: &'a AhciLock,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a AhciLock) -> Self {
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
    fn inner_mut(&mut self) -> &mut Option<AhciState> {
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
unsafe fn mmio_read(addr: u64) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

#[inline]
unsafe fn mmio_write(addr: u64, val: u32) {
    unsafe { write_volatile(addr as *mut u32, val) }
}

fn stop_port(port_base: u64) {
    unsafe {
        let mut cmd = mmio_read(port_base + PX_CMD);
        cmd &= !(1 << 0);
        cmd &= !(1 << 4);
        mmio_write(port_base + PX_CMD, cmd);

        for _ in 0..500_000 {
            let cur = mmio_read(port_base + PX_CMD);
            if (cur & ((1 << 14) | (1 << 15))) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }
}

fn start_port(port_base: u64) {
    unsafe {
        for _ in 0..500_000 {
            if (mmio_read(port_base + PX_CMD) & (1 << 15)) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        let cmd = mmio_read(port_base + PX_CMD);
        mmio_write(port_base + PX_CMD, cmd | (1 << 4));
        let cmd2 = mmio_read(port_base + PX_CMD);
        mmio_write(port_base + PX_CMD, cmd2 | (1 << 0));
    }
}

fn wait_port_ready(port_base: u64) -> bool {
    unsafe {
        for _ in 0..1_000_000 {
            let tfd = mmio_read(port_base + PX_TFD);
            if (tfd & 0x88) == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}

fn issue_dma_cmd(
    port_base: u64,
    ctrl_phys: u64,
    dma_phys: u64,
    ata_cmd: u8,
    lba: u64,
    sectors: u16,
    is_write: bool,
) -> Result<(), &'static str> {
    let is_flush = ata_cmd == ATA_CMD_FLUSH_CACHE_EXT;
    if !is_flush && (sectors == 0 || sectors > 8) {
        return Err("Invalid AHCI sector chunk count");
    }

    if !wait_port_ready(port_base) {
        return Err("AHCI port timed out waiting for ready");
    }

    let hhdm = vmm::hhdm_offset();
    let ctrl_virt = (ctrl_phys + hhdm) as *mut u8;

    unsafe {
        mmio_write(port_base + PX_IS, 0xFFFF_FFFF);

        let flags = if is_write && !is_flush {
            5 | (1 << 6)
        } else {
            5
        };
        let prdtl = if is_flush { 0 } else { 1 };

        let cmd_hdr = ctrl_virt as *mut HbaCmdHeader;
        write_volatile(
            cmd_hdr,
            HbaCmdHeader {
                flags,
                prdtl,
                prdbc: 0,
                ctba: ((ctrl_phys + 0x800) & 0xFFFF_FFFF) as u32,
                ctbau: ((ctrl_phys + 0x800) >> 32) as u32,
                reserved: [0; 4],
            },
        );

        let ct_virt = ctrl_virt.add(0x800);
        write_bytes(ct_virt, 0, 0x90);

        let cfis = ct_virt;
        *cfis.add(0) = 0x27; // Host-to-Device Register FIS
        *cfis.add(1) = 0x80; // Command bit
        *cfis.add(2) = ata_cmd;
        if !is_flush {
            *cfis.add(4) = (lba & 0xFF) as u8;
            *cfis.add(5) = ((lba >> 8) & 0xFF) as u8;
            *cfis.add(6) = ((lba >> 16) & 0xFF) as u8;
            *cfis.add(7) = if ata_cmd == ATA_CMD_IDENTIFY { 0x00 } else { 0x40 };
            *cfis.add(8) = ((lba >> 24) & 0xFF) as u8;
            *cfis.add(9) = ((lba >> 32) & 0xFF) as u8;
            *cfis.add(10) = ((lba >> 40) & 0xFF) as u8;
            *cfis.add(12) = (sectors & 0xFF) as u8;
            *cfis.add(13) = ((sectors >> 8) & 0xFF) as u8;

            let byte_count = (sectors as u32) * 512;
            let prdt = ct_virt.add(0x80) as *mut HbaPrdtEntry;
            write_volatile(
                prdt,
                HbaPrdtEntry {
                    dba: (dma_phys & 0xFFFF_FFFF) as u32,
                    dbau: (dma_phys >> 32) as u32,
                    reserved: 0,
                    dbc: (byte_count - 1) & 0x003F_FFFF,
                },
            );
        } else {
            *cfis.add(7) = 0x40;
        }

        fence(Ordering::Release);
        mmio_write(port_base + PX_CI, 1);

        for _ in 0..2_000_000 {
            let is = mmio_read(port_base + PX_IS);
            if (is & (1 << 30)) != 0 {
                return Err("AHCI task file error");
            }
            if (mmio_read(port_base + PX_CI) & 1) == 0 {
                fence(Ordering::Acquire);
                let tfd = mmio_read(port_base + PX_TFD);
                if (tfd & 0x01) != 0 {
                    return Err("AHCI command error");
                }
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    Err("AHCI command timed out")
}

/// Probes PCI for an AHCI SATA controller, initializes the first active SATA disk, and runs `IDENTIFY`.
pub fn init() -> Option<DiskInfo> {
    let dev = pci::devices()
        .into_iter()
        .find(|d| d.class_code == 0x01 && d.subclass == 0x06 && d.prog_if == 0x01)?;

    let bar5_raw = dev.bar5 & !0xF;
    if bar5_raw == 0 {
        return None;
    }

    let cmd = pci::read_u32(dev.bus, dev.device, dev.function, 0x04);
    pci::write_u32(dev.bus, dev.device, dev.function, 0x04, cmd | 0x06);

    let bar5_phys = (bar5_raw & 0xFFFF_F000) as u64;
    let bar5_off = (bar5_raw & 0x0FFF) as u64;
    let mmio_flags = vmm::PRESENT | vmm::WRITABLE | vmm::WRITE_THROUGH | vmm::CACHE_DISABLE;

    for page in 0..2u64 {
        let virt = AHCI_MMIO_VIRT + page * 4096;
        if vmm::translate(virt).is_none()
            && vmm::map_page(virt, bar5_phys + page * 4096, mmio_flags).is_err()
        {
            return None;
        }
    }

    let abar = AHCI_MMIO_VIRT + bar5_off;
    unsafe {
        let ghc = mmio_read(abar + HBA_GHC);
        mmio_write(abar + HBA_GHC, ghc | (1 << 31));
    }

    let pi = unsafe { mmio_read(abar + HBA_PI) };
    let mut active_port: Option<(usize, u64)> = None;

    for port in 0..32usize {
        if (pi & (1 << port)) == 0 {
            continue;
        }
        let port_base = abar + PORT_BASE_OFF + (port as u64) * PORT_STRIDE;
        let ssts = unsafe { mmio_read(port_base + PX_SSTS) };
        let det = ssts & 0x0F;
        let ipm = (ssts >> 8) & 0x0F;
        let sig = unsafe { mmio_read(port_base + PX_SIG) };

        if det == 3 && ipm == 1 && sig == SATA_SIG_ATA {
            active_port = Some((port, port_base));
            break;
        }
    }

    let (port, port_base) = active_port?;

    stop_port(port_base);

    let ctrl_phys = pmm::alloc_frame()?;
    let Some(dma_phys) = pmm::alloc_frame() else {
        pmm::free_frame(ctrl_phys);
        return None;
    };

    let hhdm = vmm::hhdm_offset();
    unsafe {
        write_bytes((ctrl_phys + hhdm) as *mut u8, 0, 4096);
        write_bytes((dma_phys + hhdm) as *mut u8, 0, 4096);

        mmio_write(port_base + PX_CLB, (ctrl_phys & 0xFFFF_FFFF) as u32);
        mmio_write(port_base + PX_CLBU, (ctrl_phys >> 32) as u32);
        mmio_write(port_base + PX_FB, ((ctrl_phys + 0x400) & 0xFFFF_FFFF) as u32);
        mmio_write(port_base + PX_FBU, ((ctrl_phys + 0x400) >> 32) as u32);
        mmio_write(port_base + PX_IS, 0xFFFF_FFFF);
        mmio_write(port_base + PX_SERR, 0xFFFF_FFFF);
    }

    start_port(port_base);

    if issue_dma_cmd(port_base, ctrl_phys, dma_phys, ATA_CMD_IDENTIFY, 0, 1, false).is_err() {
        stop_port(port_base);
        pmm::free_frame(ctrl_phys);
        pmm::free_frame(dma_phys);
        return None;
    }

    let mut id_buf = [0u8; 512];
    unsafe {
        copy_nonoverlapping((dma_phys + hhdm) as *const u8, id_buf.as_mut_ptr(), 512);
    }

    let lba48 = u64::from_le_bytes([
        id_buf[200], id_buf[201], id_buf[202], id_buf[203], id_buf[204], id_buf[205],
        id_buf[206], id_buf[207],
    ]);
    let sectors = if lba48 != 0 {
        lba48
    } else {
        u32::from_le_bytes([id_buf[120], id_buf[121], id_buf[122], id_buf[123]]) as u64
    };

    let mut model = [b' '; 40];
    for word in 0..20 {
        let hi = id_buf[54 + word * 2 + 1];
        let lo = id_buf[54 + word * 2];
        model[word * 2] = if (0x20..=0x7E).contains(&hi) { hi } else { b' ' };
        model[word * 2 + 1] = if (0x20..=0x7E).contains(&lo) { lo } else { b' ' };
    }

    let info = DiskInfo {
        port,
        sectors,
        model,
    };

    let mut guard = IrqGuard::acquire(&AHCI);
    *guard.inner_mut() = Some(AhciState {
        port_base,
        ctrl_phys,
        dma_phys,
        info,
    });

    Some(info)
}

/// Returns information about the active AHCI SATA disk if present.
pub fn disk_info() -> Option<DiskInfo> {
    let mut guard = IrqGuard::acquire(&AHCI);
    guard.inner_mut().as_ref().map(|st| st.info)
}

/// Reads `count` 512-byte sectors starting at `lba` into `buf` via AHCI DMA.
pub fn read_sectors(lba: u64, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
    let needed = (count as usize) * 512;
    if buf.len() < needed {
        return Err("Destination buffer smaller than requested sector read");
    }
    if count == 0 {
        return Ok(());
    }

    let mut guard = IrqGuard::acquire(&AHCI);
    let state = guard.inner_mut().as_ref().ok_or("No AHCI disk active")?;

    if lba.saturating_add(count as u64) > state.info.sectors {
        return Err("Sector read beyond disk capacity");
    }

    let hhdm = vmm::hhdm_offset();
    let mut remaining = count;
    let mut cur_lba = lba;
    let mut offset = 0usize;

    while remaining > 0 {
        let chunk = remaining.min(8);
        issue_dma_cmd(
            state.port_base,
            state.ctrl_phys,
            state.dma_phys,
            ATA_CMD_READ_DMA_EXT,
            cur_lba,
            chunk,
            false,
        )?;
        let chunk_bytes = (chunk as usize) * 512;
        unsafe {
            copy_nonoverlapping(
                (state.dma_phys + hhdm) as *const u8,
                buf.as_mut_ptr().add(offset),
                chunk_bytes,
            );
        }
        remaining -= chunk;
        cur_lba += chunk as u64;
        offset += chunk_bytes;
    }

    Ok(())
}

/// Writes `count` 512-byte sectors from `buf` starting at `lba` via AHCI DMA and flushes disk cache.
pub fn write_sectors(lba: u64, count: u16, buf: &[u8]) -> Result<(), &'static str> {
    let needed = (count as usize) * 512;
    if buf.len() < needed {
        return Err("Source buffer smaller than requested sector write");
    }
    if count == 0 {
        return Ok(());
    }

    let mut guard = IrqGuard::acquire(&AHCI);
    let state = guard.inner_mut().as_ref().ok_or("No AHCI disk active")?;

    if lba.saturating_add(count as u64) > state.info.sectors {
        return Err("Sector write beyond disk capacity");
    }

    let hhdm = vmm::hhdm_offset();
    let mut remaining = count;
    let mut cur_lba = lba;
    let mut offset = 0usize;

    while remaining > 0 {
        let chunk = remaining.min(8);
        let chunk_bytes = (chunk as usize) * 512;
        unsafe {
            copy_nonoverlapping(
                buf.as_ptr().add(offset),
                (state.dma_phys + hhdm) as *mut u8,
                chunk_bytes,
            );
        }
        issue_dma_cmd(
            state.port_base,
            state.ctrl_phys,
            state.dma_phys,
            ATA_CMD_WRITE_DMA_EXT,
            cur_lba,
            chunk,
            true,
        )?;
        remaining -= chunk;
        cur_lba += chunk as u64;
        offset += chunk_bytes;
    }

    issue_dma_cmd(
        state.port_base,
        state.ctrl_phys,
        state.dma_phys,
        ATA_CMD_FLUSH_CACHE_EXT,
        0,
        0,
        false,
    )?;

    Ok(())
}
