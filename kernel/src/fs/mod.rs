use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::cpu;

/// Metadata for a file stored in the in-memory RAMFS.
pub struct FileEntry {
    pub name: String,
    pub size: usize,
}

/// Summary statistics for the in-memory RAMFS.
#[derive(Clone, Copy)]
pub struct FsStats {
    pub file_count: usize,
    pub total_bytes: usize,
}

struct FsLock {
    locked: AtomicBool,
    inner: UnsafeCell<Option<BTreeMap<String, Vec<u8>>>>,
}

unsafe impl Sync for FsLock {}

static RAMFS: FsLock = FsLock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

struct IrqGuard<'a> {
    lock: &'a FsLock,
    saved_if: bool,
}

impl<'a> IrqGuard<'a> {
    fn acquire(lock: &'a FsLock) -> Self {
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
    fn map_mut(&mut self) -> &mut BTreeMap<String, Vec<u8>> {
        let slot = unsafe { &mut *self.lock.inner.get() };
        slot.get_or_insert_with(BTreeMap::new)
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

fn normalize_name(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim().trim_start_matches('/');
    if trimmed.is_empty() || trimmed.contains("..") || trimmed.contains('\0') {
        return Err("Invalid filename");
    }
    Ok(String::from(trimmed))
}

/// Initializes the RAMFS and seeds default files (`readme.txt`, `motd.txt`, `notes.txt`).
pub fn init() {
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    map.clear();

    map.insert(
        String::from("readme.txt"),
        Vec::from(
            &b"WMOS v2.0.0 In-Memory RAMFS\n\
               Commands: ls, cat, write, append, touch, rm, stat, open <app>\n\
               Apps: info, term, paint, notepad\n"[..],
        ),
    );
    map.insert(
        String::from("motd.txt"),
        Vec::from(
            &b"Welcome to WMOS v2.0.0 (x86_64 Rust no_std Micro-OS)!\n\
               Heap allocator, CMOS RTC, double-buffered GUI, and RAMFS are online.\n"[..],
        ),
    );
    map.insert(
        String::from("notes.txt"),
        Vec::from(
            &b"WMOS Notepad\n\
               Edit this text and click [Save] to store it in RAMFS.\n"[..],
        ),
    );
}

/// Returns all RAMFS entries sorted by filename.
pub fn list() -> Vec<FileEntry> {
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    map.iter()
        .map(|(name, data)| FileEntry {
            name: name.clone(),
            size: data.len(),
        })
        .collect()
}

/// Reads the raw bytes of `name` if it exists.
pub fn read(name: &str) -> Option<Vec<u8>> {
    let norm = normalize_name(name).ok()?;
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    map.get(&norm).cloned()
}

/// Reads `name` as a UTF-8 `String` if it exists.
pub fn read_to_string(name: &str) -> Option<String> {
    let bytes = read(name)?;
    Some(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// Writes `data` to `name`, creating or overwriting the file.
pub fn write(name: &str, data: &[u8]) -> Result<usize, &'static str> {
    let norm = normalize_name(name)?;
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    map.insert(norm, data.to_vec());
    Ok(data.len())
}

/// Appends `data` to `name`, creating the file if it does not exist.
pub fn append(name: &str, data: &[u8]) -> Result<usize, &'static str> {
    let norm = normalize_name(name)?;
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    let buf = map.entry(norm).or_default();
    buf.extend_from_slice(data);
    Ok(data.len())
}

/// Creates an empty file at `name` if it does not already exist.
pub fn touch(name: &str) -> Result<(), &'static str> {
    let norm = normalize_name(name)?;
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    map.entry(norm).or_default();
    Ok(())
}

/// Removes `name` from the RAMFS.
pub fn remove(name: &str) -> Result<(), &'static str> {
    let norm = normalize_name(name)?;
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    if map.remove(&norm).is_some() {
        Ok(())
    } else {
        Err("File not found")
    }
}

/// Returns total file count and byte usage across the RAMFS.
pub fn stats() -> FsStats {
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.map_mut();
    let total_bytes = map.values().map(|v| v.len()).sum();
    FsStats {
        file_count: map.len(),
        total_bytes,
    }
}

/// Runs a write/read/remove verification test against the RAMFS.
pub fn self_test() -> bool {
    const TEST_FILE: &str = ".fs_test";
    const PAYLOAD: &[u8] = b"wmos-ramfs-ok\n";

    if write(TEST_FILE, PAYLOAD).is_err() {
        return false;
    }

    match read(TEST_FILE) {
        Some(bytes) if bytes.as_slice() == PAYLOAD => {}
        _ => {
            let _ = remove(TEST_FILE);
            return false;
        }
    }

    if remove(TEST_FILE).is_err() {
        return false;
    }

    read(TEST_FILE).is_none()
}
