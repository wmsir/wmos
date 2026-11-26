#![no_std]
#![no_main]

mod arch;
mod boot_info;
mod gfx;

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

    gfx::render_initial_desktop(&summary);
    serial_println!("[wmos] desktop rendered");
    serial_println!("[wmos] ready");

    arch::cpu::halt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("[wmos] PANIC: {}", info);
    arch::cpu::halt_loop();
}
