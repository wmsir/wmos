pub mod fat32;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::cpu;
use crate::fs::fat32::Fat32Volume;

/// Metadata for a file stored in RAMFS or the read-only FAT32 disk mount.
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

struct FsLock<T> {
    locked: AtomicBool,
    inner: UnsafeCell<T>,
}

unsafe impl<T> Sync for FsLock<T> {}

static RAMFS: FsLock<Option<BTreeMap<String, Vec<u8>>>> = FsLock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

static DISKFS: FsLock<Option<Fat32Volume>> = FsLock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

struct IrqGuard<'a, T> {
    lock: &'a FsLock<T>,
    saved_if: bool,
}

impl<'a, T> IrqGuard<'a, T> {
    fn acquire(lock: &'a FsLock<T>) -> Self {
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
    fn inner_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.inner.get() }
    }
}

impl<T> Drop for IrqGuard<'_, T> {
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

#[inline]
fn is_disk_path(norm: &str) -> bool {
    norm == "disk" || norm.starts_with("disk/")
}

/// Initializes the RAMFS and seeds default files (`readme.txt`, `motd.txt`, `notes.txt`).
pub fn init() {
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
    map.clear();

    map.insert(
        String::from("readme.txt"),
        Vec::from(
            &b"WMOS v3.0.0 In-Memory RAMFS + FAT32 Disk\n\
               Commands: ls, cat, write, append, touch, rm, stat, disks, lspci, ps, open <app>\n\
               Apps: info, term, paint, notepad, files\n"[..],
        ),
    );
    map.insert(
        String::from("motd.txt"),
        Vec::from(
            &b"Welcome to WMOS v3.0.0 (x86_64 Rust no_std Micro-OS)!\n\
               PCI, AHCI SATA, FAT32 /disk/, Cooperative Scheduler, Heap, RTC, and RAMFS are online.\n"[..],
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

/// Scans the AHCI SATA disk for a FAT32 partition and mounts its files under `disk/`.
pub fn mount_fat32_disk() -> usize {
    let vol = fat32::scan_disk();
    let count = vol.as_ref().map_or(0, |v| v.files.len());
    let mut guard = IrqGuard::acquire(&DISKFS);
    *guard.inner_mut() = vol;
    count
}

/// Returns `true` if a FAT32 disk partition is mounted under `disk/`.
pub fn is_disk_mounted() -> bool {
    let mut guard = IrqGuard::acquire(&DISKFS);
    guard.inner_mut().is_some()
}

/// Returns all mounted read-only FAT32 files prefixed with `disk/`.
pub fn list_disk() -> Vec<FileEntry> {
    let mut guard = IrqGuard::acquire(&DISKFS);
    match guard.inner_mut() {
        Some(vol) => vol
            .files
            .iter()
            .map(|f| FileEntry {
                name: f.path.clone(),
                size: f.size,
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Returns all RAMFS entries sorted by filename.
pub fn list() -> Vec<FileEntry> {
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
    map.iter()
        .map(|(name, data)| FileEntry {
            name: name.clone(),
            size: data.len(),
        })
        .collect()
}

/// Reads the raw bytes of `name` from either the mounted FAT32 `disk/` or RAMFS.
pub fn read(name: &str) -> Option<Vec<u8>> {
    let norm = normalize_name(name).ok()?;
    if norm.starts_with("disk/") {
        let mut guard = IrqGuard::acquire(&DISKFS);
        let vol = guard.inner_mut().as_ref()?;
        let rec = vol.files.iter().find(|f| f.path == norm)?;
        return fat32::read_file(vol, rec);
    }

    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
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

/// Writes `data` to `name` in RAMFS (rejecting read-only `disk/` paths).
pub fn write(name: &str, data: &[u8]) -> Result<usize, &'static str> {
    let norm = normalize_name(name)?;
    if is_disk_path(&norm) {
        return Err("Disk mount is read-only");
    }
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
    map.insert(norm, data.to_vec());
    Ok(data.len())
}

/// Appends `data` to `name` in RAMFS (rejecting read-only `disk/` paths).
pub fn append(name: &str, data: &[u8]) -> Result<usize, &'static str> {
    let norm = normalize_name(name)?;
    if is_disk_path(&norm) {
        return Err("Disk mount is read-only");
    }
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
    let buf = map.entry(norm).or_default();
    buf.extend_from_slice(data);
    Ok(data.len())
}

/// Creates an empty file at `name` in RAMFS (rejecting read-only `disk/` paths).
pub fn touch(name: &str) -> Result<(), &'static str> {
    let norm = normalize_name(name)?;
    if is_disk_path(&norm) {
        return Err("Disk mount is read-only");
    }
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
    map.entry(norm).or_default();
    Ok(())
}

/// Removes `name` from RAMFS (rejecting read-only `disk/` paths).
pub fn remove(name: &str) -> Result<(), &'static str> {
    let norm = normalize_name(name)?;
    if is_disk_path(&norm) {
        return Err("Disk mount is read-only");
    }
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
    if map.remove(&norm).is_some() {
        Ok(())
    } else {
        Err("File not found")
    }
}

/// Returns total file count and byte usage across the RAMFS.
pub fn stats() -> FsStats {
    let mut guard = IrqGuard::acquire(&RAMFS);
    let map = guard.inner_mut().get_or_insert_with(BTreeMap::new);
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
