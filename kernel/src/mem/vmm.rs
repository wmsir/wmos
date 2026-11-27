use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::cpu;
use crate::mem::pmm;

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
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

        let allowed = PRESENT | WRITABLE | WRITE_THROUGH | CACHE_DISABLE | NO_EXECUTE;
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
