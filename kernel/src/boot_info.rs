use limine::framebuffer::Framebuffer;
use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;

/// Framebuffer layout and color channel masks shared across the kernel and `gfx`.
#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    pub addr: *mut u8,
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    pub bpp: u16,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

/// Summary of boot-time information provided by the Limine bootloader.
#[derive(Clone, Copy)]
pub struct BootSummary {
    pub limine_revision: u64,
    pub hhdm_offset: u64,
    pub memmap_entries: usize,
    pub total_ram_bytes: u64,
    pub usable_ram_bytes: u64,
    pub fb: FramebufferInfo,
}

impl BootSummary {
    /// Builds a [`BootSummary`] from the Limine memory map and primary framebuffer responses.
    pub fn from_limine(
        limine_revision: u64,
        hhdm_offset: u64,
        memmap: &MemoryMapResponse,
        fb: &Framebuffer<'_>,
    ) -> Self {
        let entries = memmap.entries();
        let mut total_ram_bytes: u64 = 0;
        let mut usable_ram_bytes: u64 = 0;

        for entry in entries {
            let len = entry.length;
            match entry.entry_type {
                EntryType::USABLE => {
                    usable_ram_bytes = usable_ram_bytes.saturating_add(len);
                    total_ram_bytes = total_ram_bytes.saturating_add(len);
                }
                EntryType::BOOTLOADER_RECLAIMABLE
                | EntryType::EXECUTABLE_AND_MODULES
                | EntryType::ACPI_RECLAIMABLE => {
                    total_ram_bytes = total_ram_bytes.saturating_add(len);
                }
                _ => {}
            }
        }

        let fb_info = FramebufferInfo {
            addr: fb.addr(),
            width: fb.width() as usize,
            height: fb.height() as usize,
            pitch: fb.pitch() as usize,
            bpp: fb.bpp(),
            red_mask_size: fb.red_mask_size(),
            red_mask_shift: fb.red_mask_shift(),
            green_mask_size: fb.green_mask_size(),
            green_mask_shift: fb.green_mask_shift(),
            blue_mask_size: fb.blue_mask_size(),
            blue_mask_shift: fb.blue_mask_shift(),
        };

        Self {
            limine_revision,
            hhdm_offset,
            memmap_entries: entries.len(),
            total_ram_bytes,
            usable_ram_bytes,
            fb: fb_info,
        }
    }
}
