use core::fmt::Write;

use crate::arch::pit;
use crate::boot_info::BootSummary;
use crate::drivers::keyboard::{self, KeyEvent};
use crate::drivers::mouse::{self, MouseState};
use crate::gfx::color::{self, Color};
use crate::gfx::fb::Framebuffer;
use crate::gfx::ui::{draw_progress_bar, BORDER_W, CURSOR_H, CURSOR_W, CursorLayer, StackBuf, Window};
use crate::mem::pmm;
use crate::shell::TerminalState;

const TASKBAR_H: usize = 36;
const MIB: u64 = 1024 * 1024;
const PAGE_SIZE: u64 = 4096;

const TAB1_X: usize = 80;
const TAB1_W: usize = 116;
const TAB2_X: usize = 204;
const TAB2_W: usize = 132;
const TRAY_W: usize = 208;

#[derive(Clone, Copy)]
struct DragState {
    win_idx: usize,
    off_x: usize,
    off_y: usize,
}

pub struct DesktopState {
    fb: Framebuffer,
    summary: BootSummary,
    vmm_ok: bool,
    wins: [Window; 2],
    z_order: [usize; 2],
    cursor: CursorLayer,
    terminal: TerminalState,
    drag: Option<DragState>,
    prev_buttons: u8,
    mouse_logged: bool,
    last_cursor_blink: bool,
    last_mouse_pos: (usize, usize),
}

impl DesktopState {
    pub fn new(summary: BootSummary, vmm_ok: bool) -> Self {
        let fb = Framebuffer::new(summary.fb);
        let w = fb.width();
        let h = fb.height();
        let work_h = h.saturating_sub(TASKBAR_H);

        let (mut sys_win, mut term_win) = compute_window_layout(w, work_h);
        sys_win.active = false;
        term_win.active = true;

        let init_mx = ((w * 56) / 100).min(w.saturating_sub(CURSOR_W));
        let init_my = ((work_h * 45) / 100).min(work_h.saturating_sub(CURSOR_H));

        let mut desktop = Self {
            fb,
            summary,
            vmm_ok,
            wins: [sys_win, term_win],
            z_order: [0, 1],
            cursor: CursorLayer::new(),
            terminal: TerminalState::new(&summary, vmm_ok),
            drag: None,
            prev_buttons: 0,
            mouse_logged: false,
            last_cursor_blink: true,
            last_mouse_pos: (init_mx, init_my),
        };

        if !summary.fb.addr.is_null() && w > 0 && h > 0 {
            desktop.fb.fill_gradient_v(0, 0, w, work_h, color::DESKTOP_TOP, color::DESKTOP_BOTTOM);
            render_desktop_header(&mut desktop.fb, w, work_h);
            desktop.redraw_windows();
            desktop.redraw_taskbar();
            desktop.cursor.save_and_draw(&mut desktop.fb, init_mx, init_my);
        }

        desktop
    }

    pub fn handle_key(&mut self, ev: KeyEvent) {
        if self.summary.fb.addr.is_null() || self.fb.width() == 0 || self.fb.height() == 0 {
            return;
        }
        let (mx, my) = self.cursor.position();
        self.cursor.restore(&mut self.fb);

        let term_on_top = self.z_order[1] == 1 && self.wins[1].active;
        self.terminal.handle_key(ev, &self.summary, self.vmm_ok);

        if !term_on_top {
            self.focus_window(1);
            self.redraw_windows();
            self.redraw_taskbar();
        } else {
            render_terminal_content(
                &mut self.fb,
                &self.wins[1],
                &self.terminal,
                self.last_cursor_blink,
            );
        }

        self.cursor.save_and_draw(&mut self.fb, mx, my);
    }

    pub fn handle_mouse(&mut self, ms: MouseState) {
        if self.summary.fb.addr.is_null() || self.fb.width() == 0 || self.fb.height() == 0 {
            return;
        }
        let w = self.fb.width();
        let h = self.fb.height();
        let work_h = h.saturating_sub(TASKBAR_H);

        let mx = ms.x.min(w.saturating_sub(1));
        let my = ms.y.min(h.saturating_sub(1));

        if !self.mouse_logged {
            self.mouse_logged = true;
            crate::serial_println!("[wmos] mouse active ({}, {})", mx, my);
        }

        let left = (ms.buttons & 1) != 0;
        let prev_left = (self.prev_buttons & 1) != 0;
        self.prev_buttons = ms.buttons;

        let mut need_redraw_all = false;

        if left && !prev_left {
            if my >= work_h {
                let pill_y = work_h + 5;
                let pill_h = 26;
                if my >= pill_y && my < pill_y + pill_h {
                    if mx >= TAB1_X && mx < TAB1_X + TAB1_W {
                        need_redraw_all = self.focus_window(0);
                    } else if mx >= TAB2_X && mx < TAB2_X + TAB2_W {
                        need_redraw_all = self.focus_window(1);
                    }
                }
            } else {
                let top_idx = self.z_order[1];
                let bot_idx = self.z_order[0];
                let hit_idx = if self.wins[top_idx].hit_test(mx, my) {
                    Some(top_idx)
                } else if self.wins[bot_idx].hit_test(mx, my) {
                    Some(bot_idx)
                } else {
                    None
                };

                if let Some(idx) = hit_idx {
                    if self.focus_window(idx) {
                        need_redraw_all = true;
                    }
                    if self.wins[idx].title_bar_hit_test(mx, my) {
                        self.drag = Some(DragState {
                            win_idx: idx,
                            off_x: mx.saturating_sub(self.wins[idx].x),
                            off_y: my.saturating_sub(self.wins[idx].y),
                        });
                    }
                }
            }
        } else if !left {
            self.drag = None;
        }

        if left {
            if let Some(drag) = self.drag {
                let win_w = self.wins[drag.win_idx].w;
                let win_h = self.wins[drag.win_idx].h;
                let max_x = w.saturating_sub(win_w);
                let max_y = work_h.saturating_sub(win_h);
                let nx = mx.saturating_sub(drag.off_x).min(max_x);
                let ny = my.saturating_sub(drag.off_y).min(max_y);

                let old_x = self.wins[drag.win_idx].x;
                let old_y = self.wins[drag.win_idx].y;

                if nx != old_x || ny != old_y {
                    self.cursor.restore(&mut self.fb);

                    let restore_w = win_w.saturating_add(4);
                    let restore_h = win_h.saturating_add(4);
                    self.fb.fill_gradient_v_region(
                        old_x,
                        old_y,
                        restore_w,
                        restore_h,
                        work_h,
                        color::DESKTOP_TOP,
                        color::DESKTOP_BOTTOM,
                    );
                    if old_y < 28 {
                        render_desktop_header(&mut self.fb, w, work_h);
                    }

                    self.wins[drag.win_idx].x = nx;
                    self.wins[drag.win_idx].y = ny;

                    self.redraw_windows();
                    self.redraw_taskbar_tray((mx, my), pit::uptime_secs());
                    self.cursor.save_and_draw(&mut self.fb, mx, my);
                    self.last_mouse_pos = (mx, my);
                    return;
                }
            }
        }

        self.cursor.restore(&mut self.fb);

        if need_redraw_all {
            self.redraw_windows();
            self.redraw_taskbar();
            self.redraw_taskbar_tray((mx, my), pit::uptime_secs());
        } else if (mx, my) != self.last_mouse_pos {
            self.redraw_taskbar_tray((mx, my), pit::uptime_secs());
        }

        self.cursor.save_and_draw(&mut self.fb, mx, my);
        self.last_mouse_pos = (mx, my);
    }

    pub fn handle_tick(&mut self, ticks: u64) {
        if self.summary.fb.addr.is_null() || self.fb.width() == 0 || self.fb.height() == 0 {
            return;
        }
        let (mx, my) = self.cursor.position();
        self.cursor.restore(&mut self.fb);

        let blink = (ticks / 50) % 2 == 0;
        let blink_changed = blink != self.last_cursor_blink;
        self.last_cursor_blink = blink;

        let overlap = rects_overlap(&self.wins[0], &self.wins[1]);
        if overlap && self.z_order[1] != 0 {
            // System Info is behind WMOS Terminal: redraw in z_order to preserve stacking
            self.redraw_windows();
        } else {
            // Safe to refresh System Info client area in place without border flicker
            let (ox, oy) = self.wins[0].client_origin();
            let cw = self.wins[0].w.saturating_sub(BORDER_W * 2);
            let ch = (self.wins[0].y + self.wins[0].h).saturating_sub(oy + BORDER_W);
            self.fb.fill_rect(ox, oy, cw, ch, color::WIN_BG);
            render_system_info_content(
                &mut self.fb,
                &self.wins[0],
                &self.summary,
                self.vmm_ok,
                self.last_mouse_pos,
                self.prev_buttons,
            );

            if blink_changed {
                render_terminal_prompt_only(&mut self.fb, &self.wins[1], &self.terminal, blink);
            }
        }

        self.redraw_taskbar_tray(self.last_mouse_pos, pit::uptime_secs());
        self.cursor.save_and_draw(&mut self.fb, mx, my);
    }

    fn focus_window(&mut self, target: usize) -> bool {
        let already_focused = self.z_order[1] == target && self.wins[target].active;
        if already_focused {
            return false;
        }
        let other = if target == 0 { 1 } else { 0 };
        self.z_order = [other, target];
        self.wins[target].active = true;
        self.wins[other].active = false;
        true
    }

    fn redraw_windows(&mut self) {
        for i in 0..2 {
            let idx = self.z_order[i];
            self.wins[idx].draw(&mut self.fb);
            if idx == 0 {
                render_system_info_content(
                    &mut self.fb,
                    &self.wins[0],
                    &self.summary,
                    self.vmm_ok,
                    self.last_mouse_pos,
                    self.prev_buttons,
                );
            } else {
                render_terminal_content(
                    &mut self.fb,
                    &self.wins[1],
                    &self.terminal,
                    self.last_cursor_blink,
                );
            }
        }
    }

    fn redraw_taskbar(&mut self) {
        let w = self.fb.width();
        let h = self.fb.height();
        if h < TASKBAR_H {
            return;
        }
        let ty = h - TASKBAR_H;
        self.fb.fill_rect(0, ty, w, TASKBAR_H, color::TASKBAR_BG);
        self.fb.draw_hline(0, ty, w, color::TASKBAR_BORDER);

        // Start pill
        let pill_y = ty + 5;
        let pill_h = 26;
        self.fb.fill_rect(8, pill_y, 64, pill_h, color::ACCENT);
        self.fb.draw_rect_outline(8, pill_y, 64, pill_h, 1, color::ACCENT_BRIGHT);
        self.fb.draw_str(24, pill_y + 9, "WMOS", color::TEXT_PRIMARY, None);

        // Window 0 tab
        if TAB1_X + TAB1_W < w {
            draw_taskbar_tab(
                &mut self.fb,
                TAB1_X,
                pill_y,
                TAB1_W,
                pill_h,
                "[System Info]",
                self.wins[0].active,
            );
        }

        // Window 1 tab
        if TAB2_X + TAB2_W < w {
            draw_taskbar_tab(
                &mut self.fb,
                TAB2_X,
                pill_y,
                TAB2_W,
                pill_h,
                "[WMOS Terminal]",
                self.wins[1].active,
            );
        }

        self.redraw_taskbar_tray(self.last_mouse_pos, pit::uptime_secs());
    }

    fn redraw_taskbar_tray(&mut self, mouse_pos: (usize, usize), uptime_secs: u64) {
        let w = self.fb.width();
        let h = self.fb.height();
        if h < TASKBAR_H || w <= TAB2_X + TAB2_W + 24 {
            return;
        }
        let ty = h - TASKBAR_H;
        let pill_y = ty + 5;
        let pill_h = 26;
        let tray_x = w.saturating_sub(TRAY_W + 8).max(TAB2_X + TAB2_W + 8);
        let clear_w = w.saturating_sub(tray_x);

        self.fb.fill_rect(tray_x, pill_y, clear_w, pill_h, color::TASKBAR_BG);

        let hrs = uptime_secs / 3600;
        let mins = (uptime_secs % 3600) / 60;
        let secs = uptime_secs % 60;
        let mut tray = StackBuf::<40>::new();
        let _ = write!(
            tray,
            "UP {:02}:{:02}:{:02} | ({},{})",
            hrs, mins, secs, mouse_pos.0, mouse_pos.1
        );
        let text_w = tray.as_str().len() * 8;
        let draw_x = if w > text_w + 12 {
            (w - text_w - 12).max(tray_x)
        } else {
            tray_x
        };
        let max_chars = w.saturating_sub(draw_x + 4) / 8;
        for (i, ch) in tray.as_str().chars().take(max_chars).enumerate() {
            self.fb.draw_char(draw_x + i * 8, pill_y + 9, ch, color::TERM_CYAN, None);
        }
    }
}

fn draw_taskbar_tab(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    label: &str,
    active: bool,
) {
    let bg = if active {
        color::TASKBAR_TAB_ACTIVE
    } else {
        color::TASKBAR_TAB_INACTIVE
    };
    let border = if active {
        color::WIN_BORDER_ACTIVE
    } else {
        color::WIN_BORDER
    };
    let fg = if active {
        color::TEXT_PRIMARY
    } else {
        color::TEXT_MUTED
    };

    fb.fill_rect(x, y, w, h, bg);
    fb.draw_rect_outline(x, y, w, h, 1, border);
    if active {
        fb.draw_hline(x + 1, y + h - 2, w - 2, color::ACCENT_BRIGHT);
    }
    fb.draw_str(x + 6, y + 9, label, fg, None);
}

fn render_desktop_header(fb: &mut Framebuffer, w: usize, work_h: usize) {
    if w >= 640 && work_h >= 460 {
        fb.draw_str(
            24,
            14,
            "WMOS v1.0.0-mvp // x86_64 Framebuffer Desktop",
            color::TEXT_MUTED,
            None,
        );
    }
}

fn rects_overlap(a: &Window, b: &Window) -> bool {
    let ax2 = a.x.saturating_add(a.w + 4);
    let ay2 = a.y.saturating_add(a.h + 4);
    let bx2 = b.x.saturating_add(b.w + 4);
    let by2 = b.y.saturating_add(b.h + 4);
    a.x < bx2 && ax2 > b.x && a.y < by2 && ay2 > b.y
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
        let win_h = work_h.saturating_sub(top_min + 20).clamp(240, 400);
        let win_y = top_min + work_h.saturating_sub(top_min + win_h) / 3;

        let sys_win = Window {
            x: start_x,
            y: win_y,
            w: left_w,
            h: win_h,
            title: "System Info",
            active: false,
        };
        let term_win = Window {
            x: start_x + left_w + gap,
            y: win_y,
            w: right_w,
            h: win_h,
            title: "WMOS Terminal",
            active: true,
        };
        (sys_win, term_win)
    } else {
        let margin_x = 12usize.min(w / 16);
        let win_w = w.saturating_sub(margin_x * 2).max(160);
        let gap_y = 8usize;
        let top_y = 10usize;
        let avail_h = work_h.saturating_sub(top_y + gap_y + 8);
        let top_h = ((avail_h * 52) / 100).max(120);
        let bot_h = avail_h.saturating_sub(top_h).max(100);

        let sys_win = Window {
            x: margin_x,
            y: top_y,
            w: win_w,
            h: top_h,
            title: "System Info",
            active: false,
        };
        let term_win = Window {
            x: margin_x,
            y: top_y + top_h + gap_y,
            w: win_w,
            h: bot_h,
            title: "WMOS Terminal",
            active: true,
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

fn render_system_info_content(
    fb: &mut Framebuffer,
    win: &Window,
    summary: &BootSummary,
    vmm_ok: bool,
    mouse_pos: (usize, usize),
    mouse_buttons: u8,
) {
    let (ox, oy) = win.client_origin();
    let pad_x = 12usize;
    let pad_y = 10usize;
    let x = ox + pad_x;
    let mut y = oy + pad_y;
    let max_w = win.w.saturating_sub(pad_x * 2 + 2);
    let max_y = win.y + win.h.saturating_sub(6);
    let line_step = if win.h >= 340 { 16usize } else { 13usize };

    let mut line = StackBuf::<96>::new();

    draw_clipped_line(fb, x, y, max_w, max_y, "OS      : wmos v1.0.0-mvp", color::TEXT_PRIMARY);
    y += line_step;

    draw_clipped_line(fb, x, y, max_w, max_y, "Arch    : x86_64 Rust no_std", color::TEXT_PRIMARY);
    y += line_step;

    let _ = write!(
        line,
        "Limine  : rev {} | HHDM {:#x}",
        summary.limine_revision, summary.hhdm_offset
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

    let p = pmm::stats();
    let free_mib = (p.free_frames as u64 * PAGE_SIZE) / MIB;
    let total_mib = (p.total_frames as u64 * PAGE_SIZE) / MIB;
    let pct = if p.total_frames > 0 {
        ((p.free_frames as u128 * 100) / (p.total_frames as u128)) as u64
    } else {
        0
    };
    line.clear();
    let _ = write!(
        line,
        "PMM     : {} / {} MiB free ({}%)",
        free_mib, total_mib, pct
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_GREEN);
    y += line_step;

    let bar_h = 12usize;
    if y + bar_h <= max_y {
        draw_progress_bar(
            fb,
            x,
            y,
            max_w.min(360),
            bar_h,
            p.free_frames as u64,
            (p.total_frames as u64).max(1),
            color::ACCENT,
            color::PROGRESS_BG,
            color::PROGRESS_BORDER,
        );
        y += bar_h + 6;
    }

    let (vmm_text, vmm_col) = if vmm_ok {
        ("VMM     : PASS (4-level paging)", color::TERM_GREEN)
    } else {
        ("VMM     : FAIL", color::DOT_RED)
    };
    draw_clipped_line(fb, x, y, max_w, max_y, vmm_text, vmm_col);
    y += line_step;

    let ticks = pit::ticks();
    let secs = pit::uptime_secs();
    line.clear();
    let _ = write!(
        line,
        "Uptime  : {:02}:{:02}:{:02} ({} ticks)",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        ticks
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_PRIMARY);
    y += line_step;

    line.clear();
    let _ = write!(
        line,
        "IRQs    : KBD {} | Mouse {}",
        keyboard::irq_count(),
        mouse::irq_count()
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_MUTED);
    y += line_step;

    line.clear();
    let _ = write!(
        line,
        "Mouse   : ({}, {}) btn={:#x}",
        mouse_pos.0, mouse_pos.1, mouse_buttons
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_CYAN);
    y += line_step + 2;

    if y + 4 <= max_y {
        fb.draw_hline(x, y, max_w, color::WIN_BORDER);
        y += 8;
    }

    let checklist: [(&str, Color); 4] = [
        ("[OK] Step 1: Limine boot + FB GUI", color::TERM_GREEN),
        ("[OK] Step 2: GDT/TSS + IDT/PIC + Shell", color::TERM_GREEN),
        ("[OK] Step 3: PMM bitmap + VMM 4-level", color::TERM_GREEN),
        ("[OK] Step 4: PIT + PS/2 Mouse/KBD", color::TERM_GREEN),
    ];
    for &(item, col) in &checklist {
        if y + 8 > max_y {
            break;
        }
        draw_clipped_line(fb, x, y, max_w, max_y, item, col);
        y += line_step;
    }
}

fn terminal_pane_rect(win: &Window) -> Option<(usize, usize, usize, usize)> {
    let (ox, oy) = win.client_origin();
    let term_pad = 6usize;
    let tx = ox + term_pad;
    let ty = oy + term_pad;
    let tw = win.w.saturating_sub(2 + term_pad * 2);
    let th = (win.y + win.h).saturating_sub(oy + term_pad + 2);
    if tw < 32 || th < 32 {
        None
    } else {
        Some((tx, ty, tw, th))
    }
}

fn render_terminal_content(
    fb: &mut Framebuffer,
    win: &Window,
    term: &TerminalState,
    blink_on: bool,
) {
    let Some((tx, ty, tw, th)) = terminal_pane_rect(win) else {
        return;
    };

    fb.fill_rect(tx, ty, tw, th, color::TERM_BG);
    fb.draw_rect_outline(tx, ty, tw, th, 1, color::WIN_BORDER);

    let x = tx + 10;
    let y0 = ty + 10;
    let max_w = tw.saturating_sub(20);
    let max_y = ty + th.saturating_sub(6);
    let line_step = if win.h >= 300 { 16usize } else { 14usize };

    let total_rows_avail = max_y.saturating_sub(y0) / line_step;
    let max_history_rows = total_rows_avail.saturating_sub(1);
    let line_count = term.line_count();
    let skip = line_count.saturating_sub(max_history_rows);

    let mut y = y0;
    for idx in skip..line_count {
        if let Some((text, col)) = term.line(idx) {
            draw_clipped_line(fb, x, y, max_w, max_y, text, col);
            y += line_step;
        }
    }

    let mut prompt_buf = StackBuf::<96>::new();
    term.format_prompt(&mut prompt_buf, blink_on);
    draw_clipped_line(fb, x, y, max_w, max_y, prompt_buf.as_str(), color::TEXT_PRIMARY);
}

fn render_terminal_prompt_only(
    fb: &mut Framebuffer,
    win: &Window,
    term: &TerminalState,
    blink_on: bool,
) {
    let Some((tx, ty, tw, th)) = terminal_pane_rect(win) else {
        return;
    };
    let x = tx + 10;
    let y0 = ty + 10;
    let max_w = tw.saturating_sub(20);
    let max_y = ty + th.saturating_sub(6);
    let line_step = if win.h >= 300 { 16usize } else { 14usize };

    let total_rows_avail = max_y.saturating_sub(y0) / line_step;
    let max_history_rows = total_rows_avail.saturating_sub(1);
    let shown = term.line_count().min(max_history_rows);
    let y = y0 + shown * line_step;

    if y + 8 > max_y || max_w < 8 {
        return;
    }

    fb.fill_rect(x, y, max_w, 10, color::TERM_BG);
    let mut prompt_buf = StackBuf::<96>::new();
    term.format_prompt(&mut prompt_buf, blink_on);
    draw_clipped_line(fb, x, y, max_w, max_y, prompt_buf.as_str(), color::TEXT_PRIMARY);
}
