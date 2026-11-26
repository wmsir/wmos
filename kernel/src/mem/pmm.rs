use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;

use crate::arch::cpu;

const PAGE_SIZE: u64 = 4096;
const BITMAP_WORDS: usize = 16384;
const MAX_FRAMES: usize = BITMAP_WORDS * 64;
const MAX_PHYS: u64 = (MAX_FRAMES as u64) * PAGE_SIZE;
const MIN_USABLE_PHYS: u64 = 0x10_0000;

/// Physical memory allocator frame statistics.
#[derive(Clone, Copy)]
pub struct PmmStats {
    pub total_frames: usize,
    pub used_frames: usize,
    pub free_frames: usize,
}

struct PmmInner {
    bitmap: [u64; BITMAP_WORDS],
    total_frames: usize,
    free_frames: usize,
    next_hint: usize,
}

struct PmmLock {
    locked: AtomicBool,
    inner: UnsafeCell<PmmInner>,
}

unsafe impl Sync for PmmLock {}

static PMM: PmmLock = PmmLock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(PmmInner {
        bitmap: [u64::MAX; BITMAP_WORDS],
        total_frames: 0,
        free_frames: 0,
        next_hint: 0,
    }),
};

struct IrqGuard<'a> {
    lock: &'a PmmLock,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a PmmLock) -> Self {
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
    fn inner_mut(&mut self) -> &mut PmmInner {
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
const fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
const fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

/// Initializes the 4 KiB physical page frame bitmap from Limine's memory map.
pub fn init(memmap: &MemoryMapResponse) {
    let mut guard = IrqGuard::acquire(&PMM);
    let inner = guard.inner_mut();

    inner.bitmap.fill(u64::MAX);
    inner.total_frames = 0;
    inner.free_frames = 0;
    inner.next_hint = 0;

    let mut total_frames: usize = 0;

    for entry in memmap.entries() {
        let is_ram = matches!(
            entry.entry_type,
            EntryType::USABLE
                | EntryType::BOOTLOADER_RECLAIMABLE
                | EntryType::EXECUTABLE_AND_MODULES
                | EntryType::ACPI_RECLAIMABLE
        );

        if is_ram {
            let end = entry.base.saturating_add(entry.length).min(MAX_PHYS);
            let start = entry.base.min(end);
            total_frames = total_frames.saturating_add(((end - start) / PAGE_SIZE) as usize);
        }

        if entry.entry_type == EntryType::USABLE {
            let start = align_up(entry.base.max(MIN_USABLE_PHYS), PAGE_SIZE);
            let end = align_down(entry.base.saturating_add(entry.length), PAGE_SIZE).min(MAX_PHYS);
            if end > start {
                let mut addr = start;
                while addr < end {
                    let frame = (addr / PAGE_SIZE) as usize;
                    let word = frame / 64;
                    let bit = frame % 64;
                    if (inner.bitmap[word] & (1u64 << bit)) != 0 {
                        inner.bitmap[word] &= !(1u64 << bit);
                        inner.free_frames += 1;
                    }
                    addr += PAGE_SIZE;
                }
            }
        }
    }

    inner.total_frames = total_frames.min(MAX_FRAMES).max(inner.free_frames);
}

/// Allocates a single 4 KiB physical page frame, returning its physical address.
pub fn alloc_frame() -> Option<u64> {
    let mut guard = IrqGuard::acquire(&PMM);
    let inner = guard.inner_mut();

    if inner.free_frames == 0 {
        return None;
    }

    let start_word = inner.next_hint;
    for offset in 0..BITMAP_WORDS {
        let word_idx = (start_word + offset) % BITMAP_WORDS;
        let word = inner.bitmap[word_idx];
        if word != u64::MAX {
            let free_bit = (!word).trailing_zeros() as usize;
            inner.bitmap[word_idx] = word | (1u64 << free_bit);
            inner.free_frames -= 1;
            inner.next_hint = word_idx;
            let frame = word_idx * 64 + free_bit;
            return Some((frame as u64) * PAGE_SIZE);
        }
    }

    None
}

/// Frees a previously allocated 4 KiB physical page frame.
pub fn free_frame(phys: u64) {
    if (phys & (PAGE_SIZE - 1)) != 0 || phys >= MAX_PHYS || phys < MIN_USABLE_PHYS {
        return;
    }

    let frame = (phys / PAGE_SIZE) as usize;
    let word_idx = frame / 64;
    let bit = frame % 64;

    let mut guard = IrqGuard::acquire(&PMM);
    let inner = guard.inner_mut();

    if (inner.bitmap[word_idx] & (1u64 << bit)) != 0 {
        inner.bitmap[word_idx] &= !(1u64 << bit);
        inner.free_frames += 1;
        if word_idx < inner.next_hint {
            inner.next_hint = word_idx;
        }
    }
}

/// Returns current physical frame statistics.
pub fn stats() -> PmmStats {
    let mut guard = IrqGuard::acquire(&PMM);
    let inner = guard.inner_mut();
    let total = inner.total_frames;
    let free = inner.free_frames;
    PmmStats {
        total_frames: total,
        used_frames: total.saturating_sub(free),
        free_frames: free,
    }
}
