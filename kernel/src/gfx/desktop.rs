use core::fmt::Write;

use crate::boot_info::BootSummary;
use crate::gfx::color::{self, Color};
use crate::gfx::fb::Framebuffer;
use crate::gfx::ui::{draw_progress_bar, CursorLayer, StackBuf, Window, CURSOR_H, CURSOR_W};

const TASKBAR_H: usize = 36;
const MIB: u64 = 1024 * 1024;

pub fn render_initial_desktop(summary: &BootSummary) {
    if summary.fb.addr.is_null() || summary.fb.width == 0 || summary.fb.height == 0 {
        return;
    }

    let mut fb = Framebuffer::new(summary.fb);
    let w = fb.width();
    let h = fb.height();

    // 1. Vertical gradient wallpaper
    let work_h = h.saturating_sub(TASKBAR_H);
    fb.fill_gradient_v(0, 0, w, work_h, color::DESKTOP_TOP, color::DESKTOP_BOTTOM);

    // Subtle desktop header watermark when enough vertical headroom exists
    if w >= 640 && work_h >= 460 {
        fb.draw_str(24, 14, "WMOS // x86_64 Framebuffer Desktop", color::TEXT_MUTED, None);
    }

    // 2. Responsive window geometry (supports 640x480, 800x600, 1024x768, 1280x800+)
    let (sys_win, term_win) = compute_window_layout(w, work_h);

    // 3. Window 1: System Info
    sys_win.draw(&mut fb);
    render_system_info(&mut fb, &sys_win, summary);

    // 4. Window 2: WMOS Terminal
    term_win.draw(&mut fb);
    render_terminal(&mut fb, &term_win, summary);

    // 5. Bottom Taskbar
    render_taskbar(&mut fb, w, h);

    // 6. Mouse cursor via anti-ghosting CursorLayer
    let mut cursor = CursorLayer::new();
    let cx = ((w * 56) / 100).min(w.saturating_sub(CURSOR_W));
    let cy = ((work_h * 45) / 100).min(work_h.saturating_sub(CURSOR_H));
    cursor.save_and_draw(&mut fb, cx, cy);
}

fn compute_window_layout(w: usize, work_h: usize) -> (Window, Window) {
    if w >= 780 {
        let gap = if w >= 1000 { 24 } else { 16 };
        let side_margin = if w >= 1000 { 32 } else { 16 };
        let usable_w = w.saturating_sub(side_margin * 2 + gap);
        let left_w = ((usable_w * 46) / 100).clamp(320, 480);
        let right_w = usable_w.saturating_sub(left_w).clamp(320, 560);
        let total_pair_w = left_w + gap + right_w;
        let start_x = w.saturating_sub(total_pair_w) / 2;

        let top_min = if work_h >= 460 { 40 } else { 16 };
        let win_h = work_h.saturating_sub(top_min + 20).clamp(240, 380);
        let win_y = top_min + work_h.saturating_sub(top_min + win_h) / 3;

        let sys_win = Window {
            x: start_x,
            y: win_y,
            w: left_w,
            h: win_h,
            title: "System Info",
            active: true,
        };
        let term_win = Window {
            x: start_x + left_w + gap,
            y: win_y,
            w: right_w,
            h: win_h,
            title: "WMOS Terminal",
            active: false,
        };
        (sys_win, term_win)
    } else {
        // Compact vertical stack for 640x480
        let margin_x = 12usize.min(w / 16);
        let win_w = w.saturating_sub(margin_x * 2).max(160);
        let gap_y = 8usize;
        let top_y = 10usize;
        let avail_h = work_h.saturating_sub(top_y + gap_y + 8);
        let top_h = ((avail_h * 54) / 100).max(120);
        let bot_h = avail_h.saturating_sub(top_h).max(100);

        let sys_win = Window {
            x: margin_x,
            y: top_y,
            w: win_w,
            h: top_h,
            title: "System Info",
            active: true,
        };
        let term_win = Window {
            x: margin_x,
            y: top_y + top_h + gap_y,
            w: win_w,
            h: bot_h,
            title: "WMOS Terminal",
            active: false,
        };
        (sys_win, term_win)
    }
}

fn draw_clipped_line(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    max_w: usize,
    max_y: usize,
    s: &str,
    fg: Color,
) {
    if y + 8 > max_y || max_w < 8 {
        return;
    }
    let max_chars = max_w / 8;
    for (idx, ch) in s.chars().take(max_chars).enumerate() {
        fb.draw_char(x + idx * 8, y, ch, fg, None);
    }
}

fn render_system_info(fb: &mut Framebuffer, win: &Window, summary: &BootSummary) {
    let (ox, oy) = win.client_origin();
    let pad_x = 12usize;
    let pad_y = 10usize;
    let x = ox + pad_x;
    let mut y = oy + pad_y;
    let max_w = win.w.saturating_sub(pad_x * 2 + 2);
    let max_y = win.y + win.h.saturating_sub(6);
    let line_step = if win.h >= 300 { 16usize } else { 13usize };

    let mut line = StackBuf::<96>::new();

    draw_clipped_line(fb, x, y, max_w, max_y, "OS      : wmos v0.1.0-mvp", color::TEXT_PRIMARY);
    y += line_step;

    draw_clipped_line(fb, x, y, max_w, max_y, "Arch    : x86_64 Rust no_std", color::TEXT_PRIMARY);
    y += line_step;

    let _ = write!(
        line,
        "Limine  : rev {} ({} memmap)",
        summary.limine_revision, summary.memmap_entries
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_MUTED);
    y += line_step;

    line.clear();
    let _ = write!(
        line,
        "Display : {}x{} {}bpp (pitch {})",
        summary.fb.width, summary.fb.height, summary.fb.bpp, summary.fb.pitch
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_CYAN);
    y += line_step;

    let usable_mib = summary.usable_ram_bytes / MIB;
    let total_mib = summary.total_ram_bytes / MIB;
    let pct = if summary.total_ram_bytes > 0 {
        ((summary.usable_ram_bytes as u128 * 100) / (summary.total_ram_bytes as u128)) as u64
    } else {
        0
    };
    line.clear();
    let _ = write!(
        line,
        "RAM     : {} / {} MiB ({}% usable)",
        usable_mib, total_mib, pct
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_GREEN);
    y += line_step;

    // Memory usage bar
    let bar_h = 12usize;
    if y + bar_h <= max_y {
        draw_progress_bar(
            fb,
            x,
            y,
            max_w.min(360),
            bar_h,
            summary.usable_ram_bytes,
            summary.total_ram_bytes.max(1),
            color::ACCENT,
            color::PROGRESS_BG,
            color::PROGRESS_BORDER,
        );
        y += bar_h + 6;
    }

    line.clear();
    let _ = write!(line, "HHDM    : {:#018x}", summary.hhdm_offset);
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_MUTED);
    y += line_step + 2;

    if y + 4 <= max_y {
        fb.draw_hline(x, y, max_w, color::WIN_BORDER);
        y += 8;
    }

    let checklist: [(&str, Color); 4] = [
        ("[OK] Step 1: Limine boot + FB GUI", color::TERM_GREEN),
        ("[..] Step 2: IDT + Keyboard + Shell", color::TEXT_MUTED),
        ("[..] Step 3: PMM/VMM + Scheduler", color::TEXT_MUTED),
        ("[..] Step 4: VFS + Userland Apps", color::TEXT_MUTED),
    ];
    for &(item, col) in &checklist {
        if y + 8 > max_y {
            break;
        }
        draw_clipped_line(fb, x, y, max_w, max_y, item, col);
        y += line_step;
    }
}

fn render_terminal(fb: &mut Framebuffer, win: &Window, summary: &BootSummary) {
    let (ox, oy) = win.client_origin();
    let term_pad = 6usize;
    let tx = ox + term_pad;
    let ty = oy + term_pad;
    let tw = win.w.saturating_sub(2 + term_pad * 2);
    let th = win.y + win.h - oy - term_pad - 2;

    if tw < 32 || th < 32 {
        return;
    }

    // Dark terminal pane inside the window
    fb.fill_rect(tx, ty, tw, th, color::TERM_BG);
    fb.draw_rect_outline(tx, ty, tw, th, 1, color::WIN_BORDER);

    let x = tx + 10;
    let mut y = ty + 10;
    let max_w = tw.saturating_sub(20);
    let max_y = ty + th.saturating_sub(6);
    let line_step = if win.h >= 300 { 16usize } else { 14usize };

    let mut line = StackBuf::<96>::new();

    draw_clipped_line(fb, x, y, max_w, max_y, "[wmos] boot ok", color::TERM_GREEN);
    y += line_step;

    line.clear();
    let _ = write!(
        line,
        "[wmos] limine rev={} hhdm={:#x}",
        summary.limine_revision, summary.hhdm_offset
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_PRIMARY);
    y += line_step;

    let usable_mib = summary.usable_ram_bytes / MIB;
    let total_mib = summary.total_ram_bytes / MIB;
    line.clear();
    let _ = write!(
        line,
        "[wmos] memmap={} usable={}MiB total={}MiB",
        summary.memmap_entries, usable_mib, total_mib
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_PRIMARY);
    y += line_step;

    line.clear();
    let _ = write!(
        line,
        "[wmos] fb={}x{} bpp={} pitch={}",
        summary.fb.width, summary.fb.height, summary.fb.bpp, summary.fb.pitch
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_CYAN);
    y += line_step;

    draw_clipped_line(fb, x, y, max_w, max_y, "[wmos] desktop rendered", color::TERM_GREEN);
    y += line_step;

    draw_clipped_line(
        fb,
        x,
        y,
        max_w,
        max_y,
        "[info] ready; input driver queued (Step 2)",
        color::TERM_AMBER,
    );
    y += line_step + 4;

    draw_clipped_line(fb, x, y, max_w, max_y, "wmos@kernel:~$ _", color::TEXT_PRIMARY);
}

fn render_taskbar(fb: &mut Framebuffer, w: usize, h: usize) {
    if h < TASKBAR_H {
        return;
    }
    let ty = h - TASKBAR_H;
    fb.fill_rect(0, ty, w, TASKBAR_H, color::TASKBAR_BG);
    fb.draw_hline(0, ty, w, color::TASKBAR_BORDER);

    // Start pill
    let pill_y = ty + 5;
    let pill_h = 26;
    fb.fill_rect(8, pill_y, 64, pill_h, color::ACCENT);
    fb.draw_rect_outline(8, pill_y, 64, pill_h, 1, color::ACCENT_BRIGHT);
    fb.draw_str(24, pill_y + 9, "WMOS", color::TEXT_PRIMARY, None);

    // Window tabs
    let tab1_x = 80;
    let tab1_w = 116;
    if tab1_x + tab1_w < w {
        fb.fill_rect(tab1_x, pill_y, tab1_w, pill_h, color::TASKBAR_TAB_ACTIVE);
        fb.draw_rect_outline(tab1_x, pill_y, tab1_w, pill_h, 1, color::WIN_BORDER_ACTIVE);
        fb.draw_hline(tab1_x + 1, pill_y + pill_h - 2, tab1_w - 2, color::ACCENT_BRIGHT);
        fb.draw_str(tab1_x + 6, pill_y + 9, "[System Info]", color::TEXT_PRIMARY, None);
    }

    let tab2_x = 204;
    let tab2_w = 132;
    if tab2_x + tab2_w + 180 < w {
        fb.fill_rect(tab2_x, pill_y, tab2_w, pill_h, color::TASKBAR_TAB_INACTIVE);
        fb.draw_rect_outline(tab2_x, pill_y, tab2_w, pill_h, 1, color::WIN_BORDER);
        fb.draw_str(tab2_x + 6, pill_y + 9, "[WMOS Terminal]", color::TEXT_MUTED, None);
    }

    // Right status tray
    let tray = "x86_64 | FB | READY";
    let tray_w = tray.len() * 8;
    if w > tray_w + 20 {
        let tx = w - tray_w - 12;
        fb.draw_str(tx, pill_y + 9, tray, color::TERM_CYAN, None);
    }
}
