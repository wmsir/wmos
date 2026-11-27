use alloc::string::String;
use core::ptr::copy_nonoverlapping;

use crate::arch::syscall;
use crate::mem::vmm::{self, UserAddressSpace};
use crate::serial_println;

const PT_LOAD: u32 = 1;
const PF_W: u32 = 2;
const USER_ADDR_CEIL: u64 = 0x0000_7000_0000_0000;
const USER_STACK_BASE: u64 = 0x0000_7FFF_F000_0000;
const USER_STACK_PAGES: u64 = 4;
const USER_RSP_INIT: u64 = 0x0000_7FFF_F000_3FF0;
const PAGE_SIZE: u64 = 4096;

/// Output captured from a synchronous Ring-3 ELF64 execution.
pub struct ExecOutput {
    pub exit_code: i64,
    pub stdout: String,
}

#[inline]
fn read_u16(bytes: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([bytes[off], bytes[off + 1]])
}

#[inline]
fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

#[inline]
fn read_u64(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        bytes[off],
        bytes[off + 1],
        bytes[off + 2],
        bytes[off + 3],
        bytes[off + 4],
        bytes[off + 5],
        bytes[off + 6],
        bytes[off + 7],
    ])
}

/// Validates and executes a static x86_64 `ET_EXEC` ELF64 binary in isolated Ring-3 user mode.
pub fn run_elf(bytes: &[u8]) -> Result<ExecOutput, &'static str> {
    if bytes.len() < 64 {
        return Err("ELF header truncated");
    }
    if &bytes[0..4] != b"\x7FELF" || bytes[4] != 2 || bytes[5] != 1 {
        return Err("Unsupported ELF format (expected ELF64 LSB)");
    }

    let e_type = read_u16(bytes, 16);
    let e_machine = read_u16(bytes, 18);
    if e_type != 2 || e_machine != 62 {
        return Err("Unsupported ELF type/arch (expected x86_64 ET_EXEC)");
    }

    let e_entry = read_u64(bytes, 24);
    let e_phoff = read_u64(bytes, 32);
    let e_phentsize = read_u16(bytes, 54) as usize;
    let e_phnum = read_u16(bytes, 56) as usize;

    if e_entry == 0 || e_entry >= USER_ADDR_CEIL {
        return Err("Invalid ELF user entry point");
    }
    if e_phentsize < 56 || e_phnum == 0 {
        return Err("Invalid ELF program header table");
    }

    let ph_end = (e_phoff as usize)
        .checked_add(e_phnum.saturating_mul(e_phentsize))
        .ok_or("ELF program header overflow")?;
    if ph_end > bytes.len() {
        return Err("ELF program headers extend past file end");
    }

    let mut user_space = UserAddressSpace::create_user_space()?;
    let hhdm = vmm::hhdm_offset();
    let mut loaded_segments = 0usize;

    for i in 0..e_phnum {
        let ph_off = (e_phoff as usize) + i * e_phentsize;
        let p_type = read_u32(bytes, ph_off);
        if p_type != PT_LOAD {
            continue;
        }

        let p_flags = read_u32(bytes, ph_off + 4);
        let p_offset = read_u64(bytes, ph_off + 8);
        let p_vaddr = read_u64(bytes, ph_off + 16);
        let p_filesz = read_u64(bytes, ph_off + 32);
        let p_memsz = read_u64(bytes, ph_off + 40);

        if p_memsz < p_filesz || p_vaddr >= USER_ADDR_CEIL {
            return Err("Invalid ELF PT_LOAD segment bounds");
        }
        let seg_end = p_vaddr
            .checked_add(p_memsz)
            .ok_or("ELF PT_LOAD virtual address overflow")?;
        if seg_end >= USER_ADDR_CEIL {
            return Err("ELF PT_LOAD segment exceeds user space limit");
        }
        let file_end = p_offset
            .checked_add(p_filesz)
            .ok_or("ELF PT_LOAD file offset overflow")?;
        if file_end > bytes.len() as u64 {
            return Err("ELF PT_LOAD file data truncated");
        }

        if p_memsz == 0 {
            continue;
        }

        loaded_segments += 1;
        let writable = (p_flags & PF_W) != 0;
        let start_page = p_vaddr & !(PAGE_SIZE - 1);
        let end_page = (seg_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let mut page_virt = start_page;
        while page_virt < end_page {
            let phys = user_space.ensure_user_page(page_virt, writable)?;
            let page_end = page_virt + PAGE_SIZE;
            let file_vaddr_end = p_vaddr + p_filesz;

            let copy_start = page_virt.max(p_vaddr);
            let copy_end = page_end.min(file_vaddr_end);
            if copy_end > copy_start {
                let len = (copy_end - copy_start) as usize;
                let dst_off = (copy_start - page_virt) as usize;
                let src_off = (p_offset + (copy_start - p_vaddr)) as usize;
                unsafe {
                    copy_nonoverlapping(
                        bytes.as_ptr().add(src_off),
                        ((phys + hhdm) as *mut u8).add(dst_off),
                        len,
                    );
                }
            }

            page_virt += PAGE_SIZE;
        }
    }

    if loaded_segments == 0 {
        return Err("ELF image contains no PT_LOAD segments");
    }

    for page_idx in 0..USER_STACK_PAGES {
        user_space.ensure_user_page(USER_STACK_BASE + page_idx * PAGE_SIZE, true)?;
    }

    let (exit_code, stdout) =
        syscall::enter_user_mode(e_entry, USER_RSP_INIT, user_space.pml4_phys());
    drop(user_space);

    serial_println!("[wmos] user elf exit={}", exit_code);
    Ok(ExecOutput { exit_code, stdout })
}
