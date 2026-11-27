use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::cpu;

const TASK_STACK_SIZE: usize = 16 * 1024;

/// Scheduling state of a cooperative kernel task.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
}

/// Snapshot metadata for a kernel task.
#[derive(Clone, Copy)]
pub struct TaskInfo {
    pub id: usize,
    pub name: &'static str,
    pub state: TaskState,
    pub switches: u64,
    pub ticks: u64,
}

struct TaskControlBlock {
    id: usize,
    name: &'static str,
    state: TaskState,
    rsp: u64,
    switches: u64,
    ticks: u64,
    _stack: Option<Box<[u8]>>,
}

struct SchedulerInner {
    tasks: Vec<TaskControlBlock>,
    current: usize,
}

struct SchedLock {
    locked: AtomicBool,
    inner: UnsafeCell<Option<SchedulerInner>>,
}

unsafe impl Sync for SchedLock {}

static SCHED: SchedLock = SchedLock {
    locked: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

static TOTAL_SWITCHES: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
.section .text,"ax",@progbits

.global switch_context
switch_context:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.global task_trampoline
task_trampoline:
    call r12
1:
    hlt
    jmp 1b
"#
);

extern "C" {
    fn switch_context(old_rsp: *mut u64, new_rsp: u64);
    fn task_trampoline();
}

#[inline]
fn save_if_and_cli() -> bool {
    let rflags: u64;
    unsafe {
        asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    cpu::cli();
    (rflags & 0x200) != 0
}

#[inline]
fn acquire_raw() {
    while SCHED
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
}

#[inline]
fn release_raw() {
    SCHED.locked.store(false, Ordering::Release);
}

fn spawn_task(id: usize, name: &'static str, entry: extern "C" fn() -> !) -> TaskControlBlock {
    let mut stack = vec![0u8; TASK_STACK_SIZE].into_boxed_slice();
    let base = stack.as_mut_ptr() as usize;
    let top = (base + TASK_STACK_SIZE) & !0xF;

    unsafe {
        let top_ptr = top as *mut u64;
        // Return address popped by `switch_context` -> enters `task_trampoline` with 16-byte aligned RSP
        top_ptr.sub(1).write(task_trampoline as *const () as usize as u64);
        top_ptr.sub(2).write(0); // rbp
        top_ptr.sub(3).write(0); // rbx
        top_ptr.sub(4).write(entry as *const () as usize as u64); // r12 -> entry point
        top_ptr.sub(5).write(0); // r13
        top_ptr.sub(6).write(0); // r14
        top_ptr.sub(7).write(0); // r15
    }

    TaskControlBlock {
        id,
        name,
        state: TaskState::Ready,
        rsp: (top - 7 * 8) as u64,
        switches: 0,
        ticks: 0,
        _stack: Some(stack),
    }
}

extern "C" fn sys_monitor_task() -> ! {
    loop {
        yield_now();
    }
}

extern "C" fn idle_worker_task() -> ! {
    loop {
        yield_now();
    }
}

/// Initializes the cooperative 3-task scheduler and performs 2 initial round-robin yields.
pub fn init() {
    let saved_if = save_if_and_cli();
    acquire_raw();

    let tasks = vec![
        TaskControlBlock {
            id: 0,
            name: "desktop-ui",
            state: TaskState::Running,
            rsp: 0,
            switches: 0,
            ticks: 0,
            _stack: None,
        },
        spawn_task(1, "sys-monitor", sys_monitor_task),
        spawn_task(2, "idle-worker", idle_worker_task),
    ];

    unsafe {
        *SCHED.inner.get() = Some(SchedulerInner { tasks, current: 0 });
    }
    TOTAL_SWITCHES.store(0, Ordering::Release);

    release_raw();
    if saved_if {
        cpu::sti();
    }

    yield_now();
    yield_now();
}

/// Cooperatively yields the CPU to the next ready kernel task in round-robin order.
pub fn yield_now() {
    let saved_if = save_if_and_cli();
    acquire_raw();

    let transition = unsafe {
        match &mut *SCHED.inner.get() {
            Some(inner) if inner.tasks.len() >= 2 => {
                let cur = inner.current;
                let next = (cur + 1) % inner.tasks.len();

                inner.tasks[cur].ticks = inner.tasks[cur].ticks.saturating_add(1);
                inner.tasks[cur].state = TaskState::Ready;
                inner.tasks[next].state = TaskState::Running;
                inner.tasks[next].switches = inner.tasks[next].switches.saturating_add(1);
                inner.current = next;

                TOTAL_SWITCHES.fetch_add(1, Ordering::Relaxed);

                let old_rsp_ptr = core::ptr::addr_of_mut!(inner.tasks[cur].rsp);
                let new_rsp = inner.tasks[next].rsp;
                Some((old_rsp_ptr, new_rsp))
            }
            _ => None,
        }
    };

    release_raw();

    if let Some((old_rsp_ptr, new_rsp)) = transition {
        unsafe {
            switch_context(old_rsp_ptr, new_rsp);
        }
    }

    if saved_if {
        cpu::sti();
    }
}

/// Returns a snapshot list of all kernel tasks.
pub fn list() -> Vec<TaskInfo> {
    let saved_if = save_if_and_cli();
    acquire_raw();

    let out = unsafe {
        match &*SCHED.inner.get() {
            Some(inner) => inner
                .tasks
                .iter()
                .map(|t| TaskInfo {
                    id: t.id,
                    name: t.name,
                    state: t.state,
                    switches: t.switches,
                    ticks: t.ticks,
                })
                .collect(),
            None => Vec::new(),
        }
    };

    release_raw();
    if saved_if {
        cpu::sti();
    }
    out
}

/// Returns the cumulative number of cooperative context switches since scheduler init.
#[inline]
pub fn total_switches() -> u64 {
    TOTAL_SWITCHES.load(Ordering::Acquire)
}
