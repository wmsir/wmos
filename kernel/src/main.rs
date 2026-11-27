#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod boot_info;
mod drivers;
mod fs;
mod gfx;
mod mem;
mod net;
mod shell;
mod task;
mod user;

use core::panic::PanicInfo;

use limine::request::{
    FramebufferRequest, HhdmRequest, MemoryMapRequest, RequestsEndMarker, RequestsStartMarker,
};
use limine::BaseRevision;

use crate::boot_info::BootSummary;

#[used]
#[link_section = ".requests_start_marker"]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[link_section = ".requests"]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[link_section = ".requests"]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[link_section = ".requests_end_marker"]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    arch::cpu::cli();
    arch::serial::init();

    serial_println!("[wmos] boot ok");

    assert!(BASE_REVISION.is_supported(), "Unsupported Limine base revision");

    arch::gdt::init();
    serial_println!("[wmos] gdt+tss ok");

    arch::pic::init();
    arch::idt::init();
    serial_println!("[wmos] idt+pic ok");

    arch::syscall::init();
    serial_println!("[wmos] syscall+ring3 ok");

    let hhdm_response = HHDM_REQUEST
        .get_response()
        .expect("Missing Limine HHDM response");
    let memmap_response = MEMORY_MAP_REQUEST
        .get_response()
        .expect("Missing Limine memory map response");
    let fb_response = FRAMEBUFFER_REQUEST
        .get_response()
        .expect("Missing Limine framebuffer response");

    let fb = fb_response
        .framebuffers()
        .next()
        .expect("No framebuffer provided by Limine");

    let limine_revision = BASE_REVISION.loaded_revision().unwrap_or(0);
    let summary = BootSummary::from_limine(
        limine_revision,
        hhdm_response.offset(),
        memmap_response,
        &fb,
    );

    mem::pmm::init(memmap_response);
    let pstats = mem::pmm::stats();
    serial_println!(
        "[wmos] pmm ok free={} total={} frames",
        pstats.free_frames,
        pstats.total_frames
    );

    mem::vmm::init(summary.hhdm_offset);
    let vmm_ok = mem::vmm::self_test();
    assert!(vmm_ok, "VMM self-test failed");
    serial_println!("[wmos] vmm self-test ok");

    let heap_bytes = mem::heap::init();
    serial_println!("[wmos] heap ok size={} KiB", heap_bytes / 1024);
    assert!(mem::heap::self_test(), "Heap self-test failed");
    serial_println!("[wmos] heap self-test ok");

    let rtc_now = arch::rtc::read();
    serial_println!(
        "[wmos] rtc ok {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        rtc_now.year,
        rtc_now.month,
        rtc_now.day,
        rtc_now.hour,
        rtc_now.minute,
        rtc_now.second
    );

    fs::init();
    assert!(fs::self_test(), "RAMFS self-test failed");
    let fs_st = fs::stats();
    serial_println!(
        "[wmos] fs ok files={} bytes={}",
        fs_st.file_count,
        fs_st.total_bytes
    );

    let pci_cnt = drivers::pci::init();
    serial_println!("[wmos] pci ok devices={}", pci_cnt);

    if let Some(disk) = drivers::ahci::init() {
        serial_println!(
            "[wmos] ahci ok disks=1 model={} sectors={}",
            disk.model_str(),
            disk.sectors
        );
    } else {
        serial_println!("[wmos] ahci ok disks=0");
    }

    let fat_files = fs::mount_fat32_disk();
    if fs::is_disk_mounted() {
        serial_println!("[wmos] disk fat32 ok files={}", fat_files);
    } else {
        serial_println!("[wmos] disk fat32 skipped");
    }

    task::init();
    assert!(task::total_switches() >= 2, "Scheduler self-test failed");
    serial_println!("[wmos] sched ok tasks={}", task::list().len());

    if let Some(nic) = drivers::e1000::init() {
        serial_println!(
            "[wmos] e1000 ok mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            nic.mac[0], nic.mac[1], nic.mac[2], nic.mac[3], nic.mac[4], nic.mac[5]
        );
        net::init();
        let cfg = net::config();
        serial_println!(
            "[wmos] net ok ip={}.{}.{}.{} gw={}.{}.{}.{}",
            cfg.ip[0], cfg.ip[1], cfg.ip[2], cfg.ip[3],
            cfg.gateway[0], cfg.gateway[1], cfg.gateway[2], cfg.gateway[3]
        );
    } else {
        serial_println!("[wmos] e1000 skipped");
    }

    arch::pit::init();
    drivers::mouse::set_bounds(summary.fb.width, summary.fb.height);
    drivers::ps2::init();
    serial_println!("[wmos] pit+ps2 ok");

    serial_println!(
        "[wmos] limine rev={} hhdm={:#x}",
        summary.limine_revision,
        summary.hhdm_offset
    );
    serial_println!(
        "[wmos] memmap entries={} usable={} MiB total={} MiB",
        summary.memmap_entries,
        summary.usable_ram_bytes / (1024 * 1024),
        summary.total_ram_bytes / (1024 * 1024)
    );
    serial_println!(
        "[wmos] fb={}x{} bpp={} pitch={}",
        summary.fb.width,
        summary.fb.height,
        summary.fb.bpp,
        summary.fb.pitch
    );

    let mut desktop = gfx::DesktopState::new(summary, vmm_ok);
    serial_println!("[wmos] desktop rendered");

    arch::cpu::sti();
    serial_println!("[wmos] ready");

    let mut last_ui_tick = arch::pit::ticks();
    loop {
        if net::is_up() {
            net::poll();
        }
        while let Some(ev) = drivers::keyboard::pop_event() {
            desktop.handle_key(ev);
        }
        if drivers::mouse::take_dirty() {
            let ms = drivers::mouse::state();
            desktop.handle_mouse(ms);
        }
        let now = arch::pit::ticks();
        if now.wrapping_sub(last_ui_tick) >= 20 {
            last_ui_tick = now;
            desktop.handle_tick(now);
        }
        task::yield_now();
        arch::cpu::cli();
        if !drivers::keyboard::has_pending()
            && !drivers::mouse::is_dirty()
            && arch::pit::ticks().wrapping_sub(last_ui_tick) < 20
        {
            arch::cpu::sti_and_hlt();
        } else {
            arch::cpu::sti();
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("[wmos] PANIC: {}", info);
    arch::cpu::halt_loop();
}
