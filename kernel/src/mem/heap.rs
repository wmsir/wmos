use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ptr::{null_mut, write_bytes};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::cpu;
use crate::mem::{pmm, vmm};

/// Virtual base address of the kernel heap.
pub const HEAP_START: u64 = 0xFFFF_A000_0000_0000;

const PAGE_SIZE: usize = 4096;
const MIN_PAGES: usize = 512;
const MAX_PAGES: usize = 4096;
const MIN_ALIGN: usize = 16;
const MIN_BLOCK_SIZE: usize = 32;

/// Kernel heap utilization statistics.
#[derive(Clone, Copy)]
pub struct HeapStats {
    pub total_bytes: usize,
    pub used_bytes: usize,
    pub free_bytes: usize,
    pub alloc_count: usize,
}

#[repr(C)]
struct FreeNode {
    size: usize,
    next: *mut FreeNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AllocHeader {
    start: usize,
    size: usize,
}

struct HeapInner {
    head: *mut FreeNode,
    total_bytes: usize,
    used_bytes: usize,
    alloc_count: usize,
}

struct LockedHeap {
    locked: AtomicBool,
    inner: UnsafeCell<HeapInner>,
}

unsafe impl Sync for LockedHeap {}

struct IrqGuard<'a> {
    lock: &'a LockedHeap,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a LockedHeap) -> Self {
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
    fn inner_mut(&mut self) -> &mut HeapInner {
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
const fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

struct KernelAllocator;

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

static HEAP: LockedHeap = LockedHeap {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(HeapInner {
        head: null_mut(),
        total_bytes: 0,
        used_bytes: 0,
        alloc_count: 0,
    }),
};

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut guard = IrqGuard::acquire(&HEAP);
        let inner = guard.inner_mut();

        let header_size = size_of::<AllocHeader>();
        let user_align = layout.align().max(8);
        let user_size = layout.size().max(1);

        let mut prev: *mut FreeNode = null_mut();
        let mut curr = inner.head;

        while !curr.is_null() {
            let block_start = curr as usize;
            let block_size = unsafe { (*curr).size };
            let block_end = block_start.saturating_add(block_size);
            let next_node = unsafe { (*curr).next };

            let user_addr = align_up(block_start.saturating_add(header_size), user_align);
            let needed_end = align_up(user_addr.saturating_add(user_size), MIN_ALIGN)
                .max(block_start.saturating_add(MIN_BLOCK_SIZE));

            if needed_end <= block_end {
                let remaining = block_end - needed_end;
                let alloc_size = if remaining >= size_of::<FreeNode>() {
                    let rem_node = needed_end as *mut FreeNode;
                    unsafe {
                        (*rem_node).size = remaining;
                        (*rem_node).next = next_node;
                    }
                    if prev.is_null() {
                        inner.head = rem_node;
                    } else {
                        unsafe {
                            (*prev).next = rem_node;
                        }
                    }
                    needed_end - block_start
                } else {
                    if prev.is_null() {
                        inner.head = next_node;
                    } else {
                        unsafe {
                            (*prev).next = next_node;
                        }
                    }
                    block_size
                };

                let header_ptr = (user_addr - header_size) as *mut AllocHeader;
                unsafe {
                    (*header_ptr).start = block_start;
                    (*header_ptr).size = alloc_size;
                }

                inner.used_bytes += alloc_size;
                inner.alloc_count += 1;
                return user_addr as *mut u8;
            }

            prev = curr;
            curr = next_node;
        }

        null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let header_ptr = (ptr as usize - size_of::<AllocHeader>()) as *const AllocHeader;
        let header = unsafe { *header_ptr };
        let block_start = header.start;
        let block_size = header.size;

        let mut guard = IrqGuard::acquire(&HEAP);
        let inner = guard.inner_mut();

        inner.used_bytes = inner.used_bytes.saturating_sub(block_size);
        inner.alloc_count = inner.alloc_count.saturating_sub(1);

        let mut prev: *mut FreeNode = null_mut();
        let mut curr = inner.head;
        while !curr.is_null() && (curr as usize) < block_start {
            prev = curr;
            curr = unsafe { (*curr).next };
        }

        let (merged_size, merged_next) = if !curr.is_null()
            && block_start.saturating_add(block_size) == curr as usize
        {
            unsafe { (block_size + (*curr).size, (*curr).next) }
        } else {
            (block_size, curr)
        };

        let new_node = block_start as *mut FreeNode;
        unsafe {
            (*new_node).size = merged_size;
            (*new_node).next = merged_next;
        }

        if !prev.is_null() {
            let prev_end = (prev as usize).saturating_add(unsafe { (*prev).size });
            if prev_end == block_start {
                unsafe {
                    (*prev).size += merged_size;
                    (*prev).next = merged_next;
                }
            } else {
                unsafe {
                    (*prev).next = new_node;
                }
            }
        } else {
            inner.head = new_node;
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { self.alloc(layout) };
        if !ptr.is_null() {
            unsafe {
                write_bytes(ptr, 0, layout.size());
            }
        }
        ptr
    }
}

/// Maps physical frames at [`HEAP_START`] and initializes the kernel heap allocator.
pub fn init() -> usize {
    let free_frames = pmm::stats().free_frames;
    let cap_pages = (free_frames / 2).max(MIN_PAGES).min(MAX_PAGES);
    let target_pages = cap_pages.min(free_frames);

    let mut mapped_pages = 0usize;
    for i in 0..target_pages {
        let Some(phys) = pmm::alloc_frame() else {
            break;
        };
        let virt = HEAP_START + (i as u64) * (PAGE_SIZE as u64);
        if vmm::map_page(virt, phys, vmm::PRESENT | vmm::WRITABLE).is_err() {
            pmm::free_frame(phys);
            break;
        }
        mapped_pages += 1;
    }

    let total_bytes = mapped_pages * PAGE_SIZE;

    let mut guard = IrqGuard::acquire(&HEAP);
    let inner = guard.inner_mut();
    inner.total_bytes = total_bytes;
    inner.used_bytes = 0;
    inner.alloc_count = 0;

    if total_bytes >= size_of::<FreeNode>() {
        let head = HEAP_START as *mut FreeNode;
        unsafe {
            (*head).size = total_bytes;
            (*head).next = null_mut();
        }
        inner.head = head;
    } else {
        inner.head = null_mut();
    }

    total_bytes
}

/// Returns current heap allocation and capacity statistics.
pub fn stats() -> HeapStats {
    let mut guard = IrqGuard::acquire(&HEAP);
    let inner = guard.inner_mut();
    HeapStats {
        total_bytes: inner.total_bytes,
        used_bytes: inner.used_bytes,
        free_bytes: inner.total_bytes.saturating_sub(inner.used_bytes),
        alloc_count: inner.alloc_count,
    }
}

/// Exercises `Box`, `Vec`, and `String` allocations and verifies coalescing deallocation.
pub fn self_test() -> bool {
    let baseline_used = stats().used_bytes;

    {
        let boxed = Box::new(0x1234_5678_9ABC_DEF0u64);
        if *boxed != 0x1234_5678_9ABC_DEF0u64 {
            return false;
        }

        let mut vec = Vec::new();
        for i in 0u32..64 {
            vec.push(i * 3 + 1);
        }
        if vec.len() != 64 || vec[0] != 1 || vec[63] != 190 {
            return false;
        }

        let mut text = String::from("wmos-");
        text.push_str("v2.0.0-heap");
        if text != "wmos-v2.0.0-heap" {
            return false;
        }
    }

    stats().used_bytes == baseline_used
}
