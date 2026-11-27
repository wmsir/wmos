use alloc::string::String;
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;

use crate::arch::{cpu, pit};
use crate::mem::vmm;
use crate::serial_println;

const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

const SYS_WRITE: u64 = 1;
const SYS_GETPID: u64 = 20;
const SYS_EXIT: u64 = 60;
const SYS_UPTIME: u64 = 96;

const USER_ADDR_LIMIT: u64 = 0x0000_7FFF_FFFF_0000;
const SYSCALL_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct SyscallStack([u8; SYSCALL_STACK_SIZE]);

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static SYSCALL_STACK: SyncCell<SyscallStack> =
    SyncCell(UnsafeCell::new(SyscallStack([0; SYSCALL_STACK_SIZE])));

static USER_CAPTURE: SyncCell<Option<UserCapture>> = SyncCell(UnsafeCell::new(None));

struct UserCapture {
    stdout: Vec<u8>,
    line_buf: Vec<u8>,
}

#[no_mangle]
static mut SYSCALL_STACK_TOP: u64 = 0;

#[no_mangle]
static mut USER_RSP_SCRATCH: u64 = 0;

#[no_mangle]
static mut KERNEL_SAVE_RSP: u64 = 0;

#[no_mangle]
static mut KERNEL_CR3_VAL: u64 = 0;

global_asm!(
    r#"
.section .text,"ax",@progbits

.global syscall_entry
syscall_entry:
    mov [rip + USER_RSP_SCRATCH], rsp
    mov rsp, [rip + SYSCALL_STACK_TOP]

    push qword ptr [rip + USER_RSP_SCRATCH]
    push rcx
    push r11
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    push rsi
    push rdx
    push rdi

    mov rcx, rdx
    mov rdx, rsi
    mov rsi, rdi
    mov rdi, rax
    cld
    call syscall_dispatch

    pop rdi
    pop rdx
    pop rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    pop r11
    pop rcx
    pop rsp
    sysretq

.global enter_user_trampoline
enter_user_trampoline:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rip + KERNEL_SAVE_RSP], rsp

    mov cr3, rdx

    push 0x1B
    push rsi
    push 0x202
    push 0x23
    push rdi
    iretq

.global syscall_return_to_kernel
syscall_return_to_kernel:
    cli
    mov rax, rdi
    mov rcx, [rip + KERNEL_CR3_VAL]
    mov cr3, rcx
    mov rsp, [rip + KERNEL_SAVE_RSP]
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
"#
);

extern "C" {
    fn syscall_entry();
    fn enter_user_trampoline(entry_rip: u64, user_rsp: u64, user_cr3: u64) -> i64;
    fn syscall_return_to_kernel(code: i64) -> !;
}

#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

#[inline]
unsafe fn wrmsr(msr: u32, val: u64) {
    let lo = (val & 0xFFFF_FFFF) as u32;
    let hi = (val >> 32) as u32;
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") lo,
            in("edx") hi,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn verify_user_readable(ptr: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }
    if ptr == 0 || ptr >= USER_ADDR_LIMIT || len > 4096 {
        return false;
    }
    let Some(end) = ptr.checked_add(len) else {
        return false;
    };
    if end >= USER_ADDR_LIMIT {
        return false;
    }

    let mut page = ptr & !0xFFF;
    while page < end {
        if vmm::translate(page).is_none() {
            return false;
        }
        page += 4096;
    }
    true
}

fn handle_sys_write(fd: u64, ptr: u64, len: u64) -> i64 {
    if fd != 1 && fd != 2 {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    if !verify_user_readable(ptr, len) {
        return -1;
    }

    let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    unsafe {
        if let Some(cap) = &mut *USER_CAPTURE.0.get() {
            cap.stdout.extend_from_slice(slice);
            for &b in slice {
                if b == b'\n' {
                    let line = String::from_utf8_lossy(&cap.line_buf);
                    serial_println!("[wmos] user: {}", line);
                    cap.line_buf.clear();
                } else if b != b'\r' {
                    cap.line_buf.push(b);
                }
            }
        }
    }

    len as i64
}

/// Dispatches Ring-3 `syscall` requests (`SYS_WRITE`, `SYS_GETPID`, `SYS_EXIT`, `SYS_UPTIME`).
#[no_mangle]
pub extern "C" fn syscall_dispatch(num: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    match num {
        SYS_WRITE => handle_sys_write(a1, a2, a3),
        SYS_GETPID => 1,
        SYS_UPTIME => pit::uptime_secs() as i64,
        SYS_EXIT => unsafe { syscall_return_to_kernel(a1 as i64) },
        _ => -38,
    }
}

/// Enables `IA32_EFER.SCE` and programs `STAR`, `LSTAR`, and `FMASK` for Ring-3 `syscall`/`sysret`.
pub fn init() {
    unsafe {
        SYSCALL_STACK_TOP = (SYSCALL_STACK.0.get() as u64) + SYSCALL_STACK_SIZE as u64;

        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | 1);

        let star = (0x0010u64 << 48) | (0x0008u64 << 32);
        wrmsr(IA32_STAR, star);

        let lstar = syscall_entry as *const () as usize as u64;
        wrmsr(IA32_LSTAR, lstar);

        wrmsr(IA32_FMASK, 0x200);
    }
}

/// Enters CPL=3 user mode in `user_cr3` at `entry_rip`/`user_rsp` and returns `(exit_code, stdout)`.
pub fn enter_user_mode(entry_rip: u64, user_rsp: u64, user_cr3: u64) -> (i64, String) {
    let rflags: u64;
    unsafe {
        asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    let saved_if = (rflags & 0x200) != 0;

    cpu::cli();
    unsafe {
        KERNEL_CR3_VAL = vmm::kernel_cr3();
        *USER_CAPTURE.0.get() = Some(UserCapture {
            stdout: Vec::new(),
            line_buf: Vec::new(),
        });
    }

    let exit_code = unsafe { enter_user_trampoline(entry_rip, user_rsp, user_cr3) };

    let stdout_bytes = unsafe {
        match (*USER_CAPTURE.0.get()).take() {
            Some(mut cap) => {
                if !cap.line_buf.is_empty() {
                    let line = String::from_utf8_lossy(&cap.line_buf);
                    serial_println!("[wmos] user: {}", line);
                    cap.line_buf.clear();
                }
                cap.stdout
            }
            None => Vec::new(),
        }
    };

    if saved_if {
        cpu::sti();
    }

    let stdout = match String::from_utf8(stdout_bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };

    (exit_code, stdout)
}

/// Aborts the active Ring-3 process after a CPL=3 CPU fault and returns to the kernel caller.
pub fn abort_user_from_fault(code: i64) -> ! {
    unsafe { syscall_return_to_kernel(code) }
}
