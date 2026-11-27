use alloc::vec::Vec;
use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::cpu;
use crate::mem::pmm;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER_ACCESSIBLE: u64 = 1 << 2;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const CACHE_DISABLE: u64 = 1 << 4;
#[allow(dead_code)]
pub const NO_EXECUTE: u64 = 1 << 63;

const HUGE_PAGE: u64 = 1 << 7;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const PAGE_SIZE: u64 = 4096;

static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Initializes the virtual memory manager with Limine's HHDM offset.
pub fn init(hhdm_offset: u64) {
    HHDM_OFFSET.store(hhdm_offset, Ordering::Release);
}

/// Returns the active Limine Higher-Half Direct Map physical-to-virtual offset.
#[inline]
pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Acquire)
}

/// Returns the physical address of the active kernel PML4 page table.
#[inline]
pub fn kernel_cr3() -> u64 {
    cpu::read_cr3() & ADDR_MASK
}

/// Loads a new PML4 physical base address into CR3.
///
/// # Safety
/// The caller must ensure `pml4_phys` points to a valid 4-level page table mapping the kernel.
#[allow(dead_code)]
#[inline]
pub unsafe fn write_cr3(pml4_phys: u64) {
    unsafe {
        asm!("mov cr3, {}", in(reg) (pml4_phys & ADDR_MASK), options(nostack, preserves_flags));
    }
}

#[inline]
unsafe fn table_mut<'a>(phys: u64) -> &'a mut [u64; 512] {
    let virt = (phys + hhdm_offset()) as *mut [u64; 512];
    unsafe { &mut *virt }
}

#[inline]
unsafe fn table_ref<'a>(phys: u64) -> &'a [u64; 512] {
    let virt = (phys + hhdm_offset()) as *const [u64; 512];
    unsafe { &*virt }
}

#[inline]
const fn indices(virt: u64) -> [usize; 4] {
    [
        ((virt >> 39) & 0x1FF) as usize,
        ((virt >> 30) & 0x1FF) as usize,
        ((virt >> 21) & 0x1FF) as usize,
        ((virt >> 12) & 0x1FF) as usize,
    ]
}

unsafe fn next_table_or_alloc(table: &mut [u64; 512], idx: usize) -> Result<u64, &'static str> {
    let entry = table[idx];
    if (entry & PRESENT) != 0 {
        if (entry & HUGE_PAGE) != 0 {
            return Err("Encountered huge page while walking page tables");
        }
        return Ok(entry & ADDR_MASK);
    }

    let frame = pmm::alloc_frame().ok_or("Out of physical frames for page table")?;
    let child = unsafe { table_mut(frame) };
    child.fill(0);
    table[idx] = frame | PRESENT | WRITABLE;
    Ok(frame)
}

/// Isolated per-process 4-level page table space sharing the kernel higher half (`256..512`).
pub struct UserAddressSpace {
    pml4_phys: u64,
    owned_frames: Vec<u64>,
}

impl UserAddressSpace {
    /// Allocates a new PML4 copying the kernel upper-half entries and zeroing the lower half.
    pub fn create_user_space() -> Result<Self, &'static str> {
        let pml4_phys = pmm::alloc_frame().ok_or("Out of physical frames for user PML4")?;
        let k_cr3 = kernel_cr3();

        unsafe {
            let k_pml4 = table_ref(k_cr3);
            let u_pml4 = table_mut(pml4_phys);
            for i in 0..256 {
                u_pml4[i] = 0;
            }
            for i in 256..512 {
                u_pml4[i] = k_pml4[i];
            }
        }

        let mut owned_frames = Vec::new();
        owned_frames.push(pml4_phys);

        Ok(Self {
            pml4_phys,
            owned_frames,
        })
    }

    /// Returns the physical address of this user address space's PML4 table.
    #[inline]
    pub fn pml4_phys(&self) -> u64 {
        self.pml4_phys
    }

    unsafe fn next_user_table_or_alloc(
        table_phys: u64,
        idx: usize,
        owned_frames: &mut Vec<u64>,
    ) -> Result<u64, &'static str> {
        let table = unsafe { table_mut(table_phys) };
        let entry = table[idx];
        if (entry & PRESENT) != 0 {
            if (entry & HUGE_PAGE) != 0 {
                return Err("Encountered huge page in user address space");
            }
            return Ok(entry & ADDR_MASK);
        }

        let frame = pmm::alloc_frame().ok_or("Out of physical frames for user page table")?;
        let child = unsafe { table_mut(frame) };
        child.fill(0);
        table[idx] = frame | PRESENT | WRITABLE | USER_ACCESSIBLE;
        owned_frames.push(frame);
        Ok(frame)
    }

    /// Maps a 4 KiB user virtual page `virt` to physical frame `phys` and tracks `phys` for cleanup.
    pub fn map_user_page(
        &mut self,
        virt: u64,
        phys: u64,
        writable: bool,
    ) -> Result<(), &'static str> {
        if (virt & (PAGE_SIZE - 1)) != 0 || (phys & (PAGE_SIZE - 1)) != 0 {
            return Err("User virtual or physical address is not 4 KiB aligned");
        }
        if virt >= 0x0000_8000_0000_0000 {
            return Err("User page address outside canonical lower half");
        }

        let [i4, i3, i2, i1] = indices(virt);
        unsafe {
            let pdpt_phys =
                Self::next_user_table_or_alloc(self.pml4_phys, i4, &mut self.owned_frames)?;
            let pd_phys = Self::next_user_table_or_alloc(pdpt_phys, i3, &mut self.owned_frames)?;
            let pt_phys = Self::next_user_table_or_alloc(pd_phys, i2, &mut self.owned_frames)?;
            let pt = table_mut(pt_phys);

            if (pt[i1] & PRESENT) != 0 {
                return Err("User virtual page already mapped");
            }

            let leaf_flags = PRESENT | USER_ACCESSIBLE | if writable { WRITABLE } else { 0 };
            pt[i1] = (phys & ADDR_MASK) | leaf_flags;
        }

        self.owned_frames.push(phys);
        Ok(())
    }

    /// Returns the physical frame mapped at `virt`, or allocates and maps a zeroed user frame.
    pub fn ensure_user_page(&mut self, virt: u64, writable: bool) -> Result<u64, &'static str> {
        let page_virt = virt & !(PAGE_SIZE - 1);
        let [i4, i3, i2, i1] = indices(page_virt);

        unsafe {
            let pml4 = table_ref(self.pml4_phys);
            let e4 = pml4[i4];
            if (e4 & PRESENT) != 0 {
                let pdpt = table_ref(e4 & ADDR_MASK);
                let e3 = pdpt[i3];
                if (e3 & PRESENT) != 0 {
                    let pd = table_ref(e3 & ADDR_MASK);
                    let e2 = pd[i2];
                    if (e2 & PRESENT) != 0 {
                        let pt = table_mut(e2 & ADDR_MASK);
                        let e1 = pt[i1];
                        if (e1 & PRESENT) != 0 {
                            if writable {
                                pt[i1] = e1 | WRITABLE;
                            }
                            return Ok(e1 & ADDR_MASK);
                        }
                    }
                }
            }
        }

        let phys = pmm::alloc_frame().ok_or("Out of physical frames for user page")?;
        unsafe {
            core::ptr::write_bytes((phys + hhdm_offset()) as *mut u8, 0, PAGE_SIZE as usize);
        }
        if let Err(e) = self.map_user_page(page_virt, phys, writable) {
            pmm::free_frame(phys);
            return Err(e);
        }
        Ok(phys)
    }
}

impl Drop for UserAddressSpace {
    fn drop(&mut self) {
        for &phys in &self.owned_frames {
            pmm::free_frame(phys);
        }
    }
}

/// Maps a 4 KiB virtual page `virt` to physical frame `phys` with the given PTE flags.
pub fn map_page(virt: u64, phys: u64, flags: u64) -> Result<(), &'static str> {
    if (virt & (PAGE_SIZE - 1)) != 0 || (phys & (PAGE_SIZE - 1)) != 0 {
        return Err("Virtual or physical address is not 4 KiB aligned");
    }

    let [i4, i3, i2, i1] = indices(virt);
    let pml4_phys = cpu::read_cr3() & ADDR_MASK;

    unsafe {
        let pml4 = table_mut(pml4_phys);
        let pdpt_phys = next_table_or_alloc(pml4, i4)?;
        let pdpt = table_mut(pdpt_phys);
        let pd_phys = next_table_or_alloc(pdpt, i3)?;
        let pd = table_mut(pd_phys);
        let pt_phys = next_table_or_alloc(pd, i2)?;
        let pt = table_mut(pt_phys);

        if (pt[i1] & PRESENT) != 0 {
            return Err("Virtual page is already mapped");
        }

        let allowed =
            PRESENT | WRITABLE | USER_ACCESSIBLE | WRITE_THROUGH | CACHE_DISABLE | NO_EXECUTE;
        pt[i1] = (phys & ADDR_MASK) | (flags & allowed) | PRESENT;
    }

    cpu::invlpg(virt);
    Ok(())
}

/// Unmaps a 4 KiB virtual page `virt` and returns its previously mapped physical frame.
pub fn unmap_page(virt: u64) -> Option<u64> {
    if (virt & (PAGE_SIZE - 1)) != 0 {
        return None;
    }

    let [i4, i3, i2, i1] = indices(virt);
    let mut table_phys = cpu::read_cr3() & ADDR_MASK;

    unsafe {
        for idx in [i4, i3, i2] {
            let table = table_ref(table_phys);
            let entry = table[idx];
            if (entry & PRESENT) == 0 || (entry & HUGE_PAGE) != 0 {
                return None;
            }
            table_phys = entry & ADDR_MASK;
        }

        let pt = table_mut(table_phys);
        let entry = pt[i1];
        if (entry & PRESENT) == 0 {
            return None;
        }

        pt[i1] = 0;
        cpu::invlpg(virt);
        Some(entry & ADDR_MASK)
    }
}

/// Translates a virtual address to its mapped physical address using the active CR3 page tables.
pub fn translate(virt: u64) -> Option<u64> {
    let [i4, i3, i2, i1] = indices(virt);
    let pml4_phys = cpu::read_cr3() & ADDR_MASK;

    unsafe {
        let pml4 = table_ref(pml4_phys);
        let e4 = pml4[i4];
        if (e4 & PRESENT) == 0 {
            return None;
        }

        let pdpt = table_ref(e4 & ADDR_MASK);
        let e3 = pdpt[i3];
        if (e3 & PRESENT) == 0 {
            return None;
        }
        if (e3 & HUGE_PAGE) != 0 {
            return Some((e3 & 0x000F_FFFF_C000_0000) | (virt & 0x3FFF_FFFF));
        }

        let pd = table_ref(e3 & ADDR_MASK);
        let e2 = pd[i2];
        if (e2 & PRESENT) == 0 {
            return None;
        }
        if (e2 & HUGE_PAGE) != 0 {
            return Some((e2 & 0x000F_FFFF_FFE0_0000) | (virt & 0x001F_FFFF));
        }

        let pt = table_ref(e2 & ADDR_MASK);
        let e1 = pt[i1];
        if (e1 & PRESENT) == 0 {
            return None;
        }

        Some((e1 & ADDR_MASK) | (virt & 0xFFF))
    }
}

/// Runs a live 4-level map/write/HHDM-verify/unmap self-test and returns `true` if all checks pass.
pub fn self_test() -> bool {
    const TEST_BASE: u64 = 0xFFFF_9000_0000_0000;
    const MAGIC: u64 = 0x574D_4F53_CAFE_BABE;

    let mut test_virt = None;
    for page in 0..4096u64 {
        let candidate = TEST_BASE + page * PAGE_SIZE;
        if translate(candidate).is_none() {
            test_virt = Some(candidate);
            break;
        }
    }

    let virt = match test_virt {
        Some(v) => v,
        None => return false,
    };

    let phys = match pmm::alloc_frame() {
        Some(p) => p,
        None => return false,
    };

    if map_page(virt, phys, PRESENT | WRITABLE).is_err() {
        pmm::free_frame(phys);
        return false;
    }

    let mut passed = true;

    unsafe {
        write_volatile(virt as *mut u64, MAGIC);
        let read_virt = read_volatile(virt as *const u64);
        let read_hhdm = read_volatile((phys + hhdm_offset()) as *const u64);
        if read_virt != MAGIC || read_hhdm != MAGIC {
            passed = false;
        }
    }

    if translate(virt) != Some(phys) {
        passed = false;
    }

    if unmap_page(virt) != Some(phys) {
        passed = false;
    }

    if translate(virt).is_some() {
        passed = false;
    }

    pmm::free_frame(phys);
    passed
}
