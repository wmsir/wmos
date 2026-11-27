use core::fmt::Write;

use crate::arch::{cpu, pit, rtc};
use crate::boot_info::BootSummary;
use crate::drivers::keyboard::{self, KeyEvent};
use crate::drivers::{mouse, pci};
use crate::drivers::mouse::MouseState;
use crate::fs;
use crate::gfx::apps::{FilesAction, FilesApp, NotepadApp, PaintApp};
use crate::gfx::color::{self, Color, Theme, ThemePalette};
use crate::gfx::fb::Framebuffer;
use crate::gfx::ui::{
    draw_progress_bar, BORDER_W, CURSOR_H, CURSOR_W, CursorLayer, StackBuf, TITLE_BAR_H, Window,
    WindowButton,
};
use crate::mem::{heap, pmm};
use crate::shell::{ShellAction, TerminalState};
use crate::task;

const TASKBAR_H: usize = 36;
const MIB: u64 = 1024 * 1024;
const PAGE_SIZE: u64 = 4096;

const START_PILL_X: usize = 8;
const START_PILL_W: usize = 64;
const START_MENU_X: usize = 8;
const START_MENU_W: usize = 184;
const START_MENU_ITEM_H: usize = 24;
const START_MENU_COUNT: usize = 8;

const WIN_COUNT: usize = 5;
const TAB_LABELS: [&str; WIN_COUNT] = ["Info", "Term", "Paint", "Note", "Files"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowVis {
    Open,
    Minimized,
    Closed,
}

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
    theme: Theme,
    wins: [Window; WIN_COUNT],
    vis: [WindowVis; WIN_COUNT],
    default_rects: [(usize, usize, usize, usize); WIN_COUNT],
    z_order: [usize; WIN_COUNT],
    cursor: CursorLayer,
    terminal: TerminalState,
    paint: PaintApp,
    notepad: NotepadApp,
    files: FilesApp,
    start_menu_open: bool,
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

        if fb.is_double_buffered() {
            crate::serial_println!(
                "[wmos] fb double-buffer ok ({}x{}, {} KiB)",
                w,
                h,
                fb.backbuffer_bytes() / 1024
            );
        }

        let (wins, default_rects) = compute_initial_windows(w, work_h);
        let init_mx = ((w * 56) / 100).min(w.saturating_sub(CURSOR_W));
        let init_my = ((work_h * 45) / 100).min(work_h.saturating_sub(CURSOR_H));

        let mut desktop = Self {
            fb,
            summary,
            vmm_ok,
            theme: Theme::Dark,
            wins,
            vis: [
                WindowVis::Open,
                WindowVis::Open,
                WindowVis::Closed,
                WindowVis::Closed,
                WindowVis::Closed,
            ],
            default_rects,
            z_order: [2, 3, 4, 0, 1],
            cursor: CursorLayer::new(),
            terminal: TerminalState::new(&summary, vmm_ok),
            paint: PaintApp::new(),
            notepad: NotepadApp::new(),
            files: FilesApp::new(),
            start_menu_open: false,
            drag: None,
            prev_buttons: 0,
            mouse_logged: false,
            last_cursor_blink: true,
            last_mouse_pos: (init_mx, init_my),
        };

        if !summary.fb.addr.is_null() && w > 0 && h > 0 {
            desktop.redraw_scene_to_backbuffer();
            desktop.cursor.save_and_draw(&mut desktop.fb, init_mx, init_my);
            desktop.fb.present_all();
        }

        desktop
    }

    pub fn handle_key(&mut self, ev: KeyEvent) {
        if self.summary.fb.addr.is_null() || self.fb.width() == 0 || self.fb.height() == 0 {
            return;
        }
        let (mx, my) = self.cursor.position();
        self.cursor.restore(&mut self.fb);

        if self.vis[3] == WindowVis::Open && self.wins[3].active {
            if self.notepad.handle_key(ev) {
                self.notepad
                    .render(&mut self.fb, &self.wins[3], self.last_cursor_blink);
                if self.start_menu_open {
                    self.draw_start_menu();
                }
                self.cursor.save_and_draw(&mut self.fb, mx, my);
                self.fb.present_rect(
                    self.wins[3].x,
                    self.wins[3].y,
                    self.wins[3].w,
                    self.wins[3].h,
                );
                self.fb.present_rect(mx, my, CURSOR_W, CURSOR_H);
                return;
            }
            self.cursor.save_and_draw(&mut self.fb, mx, my);
            return;
        }

        let need_open_or_focus = self.vis[1] != WindowVis::Open || !self.wins[1].active;
        if need_open_or_focus {
            self.open_and_focus(1);
        }

        let action = self.terminal.handle_key(ev, &self.summary, self.vmm_ok);
        if ev == KeyEvent::Enter && self.vis[4] == WindowVis::Open {
            self.files.refresh();
        }

        match action {
            ShellAction::OpenApp(app_idx) if app_idx < WIN_COUNT => {
                if app_idx == 4 {
                    self.files.refresh();
                }
                self.open_and_focus(app_idx);
                self.redraw_scene_to_backbuffer();
                self.cursor.save_and_draw(&mut self.fb, mx, my);
                self.fb.present_all();
            }
            ShellAction::OpenInNotepad(file_name) => {
                self.notepad.open_file(file_name.as_str());
                self.open_and_focus(3);
                self.redraw_scene_to_backbuffer();
                self.cursor.save_and_draw(&mut self.fb, mx, my);
                self.fb.present_all();
            }
            ShellAction::SetTheme(new_theme) => {
                self.theme = new_theme;
                self.redraw_scene_to_backbuffer();
                self.cursor.save_and_draw(&mut self.fb, mx, my);
                self.fb.present_all();
            }
            _ if need_open_or_focus || (ev == KeyEvent::Enter && self.vis[4] == WindowVis::Open) => {
                self.redraw_scene_to_backbuffer();
                self.cursor.save_and_draw(&mut self.fb, mx, my);
                self.fb.present_all();
            }
            _ => {
                render_terminal_content(
                    &mut self.fb,
                    &self.wins[1],
                    &self.terminal,
                    self.last_cursor_blink,
                );
                if self.start_menu_open {
                    self.draw_start_menu();
                }
                self.cursor.save_and_draw(&mut self.fb, mx, my);
                self.fb.present_rect(
                    self.wins[1].x,
                    self.wins[1].y,
                    self.wins[1].w,
                    self.wins[1].h,
                );
                self.fb.present_rect(mx, my, CURSOR_W, CURSOR_H);
            }
        }
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
        let right = (ms.buttons & 2) != 0;
        let prev_left = (self.prev_buttons & 1) != 0;
        let prev_right = (self.prev_buttons & 2) != 0;
        let left_just = left && !prev_left;
        let right_just = right && !prev_right;
        self.prev_buttons = ms.buttons;

        let (old_mx, old_my) = self.cursor.position();
        let mut need_full_redraw = false;
        let mut dirty_win: Option<usize> = None;

        if left_just {
            let pill_y = work_h + 5;
            let pill_h = 26;
            if my >= work_h
                && my >= pill_y
                && my < pill_y + pill_h
                && mx >= START_PILL_X
                && mx < START_PILL_X + START_PILL_W
            {
                self.start_menu_open = !self.start_menu_open;
                need_full_redraw = true;
            } else if self.start_menu_open {
                if let Some(item_idx) = self.start_menu_hit(mx, my, work_h) {
                    match item_idx {
                        0..=4 => {
                            self.start_menu_open = false;
                            if item_idx == 4 {
                                self.files.refresh();
                            }
                            self.open_and_focus(item_idx);
                        }
                        5 => {
                            self.theme = self.theme.next();
                        }
                        6 => {
                            crate::serial_println!("[wmos] reboot requested");
                            cpu::reboot();
                        }
                        7 => {
                            crate::serial_println!("[wmos] poweroff requested");
                            cpu::shutdown();
                        }
                        _ => {
                            self.start_menu_open = false;
                        }
                    }
                } else {
                    self.start_menu_open = false;
                }
                need_full_redraw = true;
            } else if my >= work_h {
                if let Some(tab_idx) = self.taskbar_tab_hit(mx, my, w, work_h) {
                    if self.vis[tab_idx] == WindowVis::Open && self.wins[tab_idx].active {
                        self.minimize_window(tab_idx);
                    } else {
                        if tab_idx == 4 {
                            self.files.refresh();
                        }
                        self.open_and_focus(tab_idx);
                    }
                    need_full_redraw = true;
                }
            } else if let Some(idx) = self.topmost_open_hit(mx, my) {
                if self.wins[idx].title_bar_hit_test(mx, my) {
                    match self.wins[idx].button_hit_test(mx, my) {
                        Some(WindowButton::Close) => {
                            self.close_window(idx);
                            need_full_redraw = true;
                        }
                        Some(WindowButton::Minimize) => {
                            self.minimize_window(idx);
                            need_full_redraw = true;
                        }
                        Some(WindowButton::Reset) => {
                            let (rx, ry, rw, rh) = self.default_rects[idx];
                            self.wins[idx].x = rx;
                            self.wins[idx].y = ry;
                            self.wins[idx].w = rw;
                            self.wins[idx].h = rh;
                            self.open_and_focus(idx);
                            need_full_redraw = true;
                        }
                        None => {
                            if self.focus_window(idx) {
                                need_full_redraw = true;
                            }
                            self.drag = Some(DragState {
                                win_idx: idx,
                                off_x: mx.saturating_sub(self.wins[idx].x),
                                off_y: my.saturating_sub(self.wins[idx].y),
                            });
                        }
                    }
                } else {
                    if self.focus_window(idx) {
                        need_full_redraw = true;
                    }
                    if idx == 2 {
                        if self.paint.handle_mouse(&self.wins[2], mx, my, true, right, true) {
                            dirty_win = Some(2);
                        }
                    } else if idx == 3 {
                        if self.notepad.handle_mouse(&self.wins[3], mx, my, true) {
                            dirty_win = Some(3);
                        }
                    } else if idx == 4 {
                        let (changed, action) =
                            self.files.handle_mouse(&self.wins[4], mx, my, true);
                        if let Some(FilesAction::OpenFile(name)) = action {
                            self.notepad.open_file(name.as_str());
                            self.open_and_focus(3);
                            need_full_redraw = true;
                        } else if changed {
                            dirty_win = Some(4);
                        }
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

                if nx != self.wins[drag.win_idx].x || ny != self.wins[drag.win_idx].y {
                    self.cursor.restore(&mut self.fb);
                    self.wins[drag.win_idx].x = nx;
                    self.wins[drag.win_idx].y = ny;
                    self.redraw_scene_to_backbuffer();
                    self.cursor.save_and_draw(&mut self.fb, mx, my);
                    self.last_mouse_pos = (mx, my);
                    self.fb.present_all();
                    return;
                }
            } else if !left_just && self.vis[2] == WindowVis::Open && self.wins[2].active {
                if let Some(top_idx) = self.topmost_open_hit(mx, my) {
                    if top_idx == 2
                        && !self.wins[2].title_bar_hit_test(mx, my)
                        && self.paint.handle_mouse(&self.wins[2], mx, my, true, right, false)
                    {
                        dirty_win = Some(2);
                    }
                }
            }
        } else if right && self.vis[2] == WindowVis::Open {
            if let Some(top_idx) = self.topmost_open_hit(mx, my) {
                if top_idx == 2 && !self.wins[2].title_bar_hit_test(mx, my) {
                    if self.focus_window(2) {
                        need_full_redraw = true;
                    }
                    if self
                        .paint
                        .handle_mouse(&self.wins[2], mx, my, false, true, right_just)
                    {
                        dirty_win = Some(2);
                    }
                }
            }
        }

        self.cursor.restore(&mut self.fb);

        if need_full_redraw {
            self.redraw_scene_to_backbuffer();
            self.cursor.save_and_draw(&mut self.fb, mx, my);
            self.last_mouse_pos = (mx, my);
            self.fb.present_all();
            return;
        }

        if let Some(idx) = dirty_win {
            self.render_window_body(idx);
            self.cursor.save_and_draw(&mut self.fb, mx, my);
            self.last_mouse_pos = (mx, my);
            self.fb.present_rect(
                self.wins[idx].x,
                self.wins[idx].y,
                self.wins[idx].w,
                self.wins[idx].h,
            );
            self.fb.present_rect(old_mx, old_my, CURSOR_W, CURSOR_H);
            self.fb.present_rect(mx, my, CURSOR_W, CURSOR_H);
            return;
        }

        if (mx, my) != self.last_mouse_pos {
            self.redraw_taskbar_tray((mx, my));
        }

        self.cursor.save_and_draw(&mut self.fb, mx, my);
        self.last_mouse_pos = (mx, my);

        self.fb.present_rect(old_mx, old_my, CURSOR_W, CURSOR_H);
        self.fb.present_rect(mx, my, CURSOR_W, CURSOR_H);
        if work_h + TASKBAR_H <= h {
            let tray_w = tray_width_for(w);
            let tray_x = w.saturating_sub(tray_w + 8);
            self.fb
                .present_rect(tray_x, work_h + 5, w.saturating_sub(tray_x), 26);
        }
    }

    pub fn handle_tick(&mut self, ticks: u64) {
        if self.summary.fb.addr.is_null() || self.fb.width() == 0 || self.fb.height() == 0 {
            return;
        }
        let w = self.fb.width();
        let h = self.fb.height();
        let work_h = h.saturating_sub(TASKBAR_H);
        let (mx, my) = self.cursor.position();
        self.cursor.restore(&mut self.fb);

        let blink = (ticks / 50) % 2 == 0;
        let blink_changed = blink != self.last_cursor_blink;
        self.last_cursor_blink = blink;

        if self.vis[0] == WindowVis::Open {
            if self.any_higher_overlap(0) {
                self.redraw_scene_to_backbuffer();
                self.cursor.save_and_draw(&mut self.fb, mx, my);
                self.fb.present_all();
                return;
            } else {
                let (ox, oy) = self.wins[0].client_origin();
                let cw = self.wins[0].w.saturating_sub(BORDER_W * 2);
                let ch = (self.wins[0].y + self.wins[0].h).saturating_sub(oy + BORDER_W);
                self.fb.fill_rect(ox, oy, cw, ch, color::WIN_BG);
                render_system_info_content(
                    &mut self.fb,
                    &self.wins[0],
                    &self.summary,
                    self.vmm_ok,
                    self.theme,
                    self.last_mouse_pos,
                    self.prev_buttons,
                );
                if self.start_menu_open {
                    self.draw_start_menu();
                }
                self.fb.present_rect(ox, oy, cw, ch);
            }
        }

        if blink_changed {
            if self.vis[1] == WindowVis::Open && !self.any_higher_overlap(1) {
                render_terminal_prompt_only(&mut self.fb, &self.wins[1], &self.terminal, blink);
                self.fb.present_rect(
                    self.wins[1].x,
                    self.wins[1].y,
                    self.wins[1].w,
                    self.wins[1].h,
                );
            }
            if self.vis[3] == WindowVis::Open && self.wins[3].active && !self.any_higher_overlap(3)
            {
                self.notepad.render(&mut self.fb, &self.wins[3], blink);
                self.fb.present_rect(
                    self.wins[3].x,
                    self.wins[3].y,
                    self.wins[3].w,
                    self.wins[3].h,
                );
            }
        }

        self.redraw_taskbar_tray(self.last_mouse_pos);
        let tray_w = tray_width_for(w);
        let tray_x = w.saturating_sub(tray_w + 8);
        self.fb
            .present_rect(tray_x, work_h + 5, w.saturating_sub(tray_x), 26);

        self.cursor.save_and_draw(&mut self.fb, mx, my);
        self.fb.present_rect(mx, my, CURSOR_W, CURSOR_H);
    }

    fn topmost_open_hit(&self, mx: usize, my: usize) -> Option<usize> {
        for &idx in self.z_order.iter().rev() {
            if self.vis[idx] == WindowVis::Open && self.wins[idx].hit_test(mx, my) {
                return Some(idx);
            }
        }
        None
    }

    fn any_higher_overlap(&self, target: usize) -> bool {
        let Some(pos) = self.z_order.iter().position(|&id| id == target) else {
            return false;
        };
        for &other in &self.z_order[pos + 1..] {
            if self.vis[other] == WindowVis::Open
                && rects_overlap(&self.wins[target], &self.wins[other])
            {
                return true;
            }
        }
        false
    }

    fn focus_window(&mut self, target: usize) -> bool {
        if self.z_order[WIN_COUNT - 1] == target && self.wins[target].active {
            return false;
        }
        let mut new_order = [0usize; WIN_COUNT];
        let mut k = 0usize;
        for &id in &self.z_order {
            if id != target {
                new_order[k] = id;
                k += 1;
            }
            self.wins[id].active = false;
        }
        new_order[WIN_COUNT - 1] = target;
        self.z_order = new_order;
        self.wins[target].active = true;
        true
    }

    fn open_and_focus(&mut self, target: usize) {
        self.vis[target] = WindowVis::Open;
        self.focus_window(target);
    }

    fn minimize_window(&mut self, target: usize) {
        self.vis[target] = WindowVis::Minimized;
        self.wins[target].active = false;
        self.drag = None;
        self.focus_next_open();
    }

    fn close_window(&mut self, target: usize) {
        self.vis[target] = WindowVis::Closed;
        self.wins[target].active = false;
        self.drag = None;
        self.focus_next_open();
    }

    fn focus_next_open(&mut self) {
        for &id in self.z_order.iter().rev() {
            if self.vis[id] == WindowVis::Open {
                self.focus_window(id);
                return;
            }
        }
    }

    fn redraw_scene_to_backbuffer(&mut self) {
        let w = self.fb.width();
        let h = self.fb.height();
        let work_h = h.saturating_sub(TASKBAR_H);
        let pal = self.theme.palette();

        self.fb
            .fill_gradient_v(0, 0, w, work_h, pal.desktop_top, pal.desktop_bottom);
        render_desktop_header(&mut self.fb, w, work_h);

        for i in 0..WIN_COUNT {
            let idx = self.z_order[i];
            if self.vis[idx] == WindowVis::Open {
                draw_themed_window(&mut self.fb, &self.wins[idx], pal);
                self.render_window_body(idx);
            }
        }

        self.redraw_taskbar();
        if self.start_menu_open {
            self.draw_start_menu();
        }
    }

    fn render_window_body(&mut self, idx: usize) {
        match idx {
            0 => render_system_info_content(
                &mut self.fb,
                &self.wins[0],
                &self.summary,
                self.vmm_ok,
                self.theme,
                self.last_mouse_pos,
                self.prev_buttons,
            ),
            1 => render_terminal_content(
                &mut self.fb,
                &self.wins[1],
                &self.terminal,
                self.last_cursor_blink,
            ),
            2 => self.paint.render(&mut self.fb, &self.wins[2]),
            3 => self
                .notepad
                .render(&mut self.fb, &self.wins[3], self.last_cursor_blink),
            4 => self.files.render(&mut self.fb, &self.wins[4]),
            _ => {}
        }
    }

    fn start_menu_rect(&self, work_h: usize) -> (usize, usize, usize, usize) {
        let mh = START_MENU_COUNT * START_MENU_ITEM_H + 8;
        let my = work_h.saturating_sub(mh + 2);
        (START_MENU_X, my, START_MENU_W, mh)
    }

    fn start_menu_hit(&self, mx: usize, my: usize, work_h: usize) -> Option<usize> {
        let (sx, sy, sw, sh) = self.start_menu_rect(work_h);
        if mx < sx || mx >= sx + sw || my < sy + 4 || my >= sy + sh - 4 {
            return None;
        }
        let idx = (my - (sy + 4)) / START_MENU_ITEM_H;
        if idx < START_MENU_COUNT {
            Some(idx)
        } else {
            None
        }
    }

    fn draw_start_menu(&mut self) {
        let work_h = self.fb.height().saturating_sub(TASKBAR_H);
        let (sx, sy, sw, sh) = self.start_menu_rect(work_h);
        let pal = self.theme.palette();

        self.fb
            .fill_rect(sx + 4, sy + 4, sw, sh, color::WIN_SHADOW);
        self.fb.fill_rect(sx, sy, sw, sh, color::WIN_BG);
        self.fb
            .draw_rect_outline(sx, sy, sw, sh, 1, pal.accent_bright);

        let mut theme_buf = StackBuf::<32>::new();
        let _ = write!(theme_buf, "Theme: {}", self.theme.name());

        let labels: [&str; START_MENU_COUNT] = [
            "System Info",
            "WMOS Terminal",
            "WMOS Paint",
            "WMOS Notepad",
            "WMOS Files",
            theme_buf.as_str(),
            "Reboot System",
            "Shutdown",
        ];

        for (i, &label) in labels.iter().enumerate() {
            let iy = sy + 4 + i * START_MENU_ITEM_H;
            if i == 5 || i == 6 {
                self.fb
                    .draw_hline(sx + 6, iy, sw - 12, color::WIN_BORDER);
            }
            let fg = if i >= 6 {
                color::TERM_AMBER
            } else if i == 5 {
                pal.accent_bright
            } else if i < WIN_COUNT && self.vis[i] == WindowVis::Open {
                color::TERM_CYAN
            } else {
                color::TEXT_PRIMARY
            };
            self.fb.draw_str(sx + 12, iy + 8, label, fg, None);
        }
    }

    fn taskbar_tab_geom(&self, w: usize) -> (usize, usize, usize) {
        let open_tabs = self
            .vis
            .iter()
            .filter(|&&v| v != WindowVis::Closed)
            .count()
            .max(1);
        let avail = w.saturating_sub(tray_width_for(w) + 12 + 80);
        let gap = 4usize;
        let tab_w = (avail / open_tabs).saturating_sub(gap).clamp(44, 84);
        (80, tab_w, gap)
    }

    fn taskbar_tab_hit(&self, mx: usize, my: usize, w: usize, work_h: usize) -> Option<usize> {
        let pill_y = work_h + 5;
        let pill_h = 26;
        if my < pill_y || my >= pill_y + pill_h {
            return None;
        }
        let (mut tx, tab_w, gap) = self.taskbar_tab_geom(w);
        let tray_limit = w.saturating_sub(tray_width_for(w) + 12);

        for idx in 0..WIN_COUNT {
            if self.vis[idx] == WindowVis::Closed {
                continue;
            }
            if tx + tab_w > tray_limit {
                break;
            }
            if mx >= tx && mx < tx + tab_w {
                return Some(idx);
            }
            tx += tab_w + gap;
        }
        None
    }

    fn redraw_taskbar(&mut self) {
        let w = self.fb.width();
        let h = self.fb.height();
        if h < TASKBAR_H {
            return;
        }
        let ty = h - TASKBAR_H;
        let pal = self.theme.palette();

        self.fb.fill_rect(0, ty, w, TASKBAR_H, color::TASKBAR_BG);
        self.fb.draw_hline(0, ty, w, color::TASKBAR_BORDER);

        let pill_y = ty + 5;
        let pill_h = 26;
        let pill_bg = if self.start_menu_open {
            pal.accent_bright
        } else {
            pal.accent
        };
        self.fb
            .fill_rect(START_PILL_X, pill_y, START_PILL_W, pill_h, pill_bg);
        self.fb.draw_rect_outline(
            START_PILL_X,
            pill_y,
            START_PILL_W,
            pill_h,
            1,
            pal.accent_bright,
        );
        self.fb
            .draw_str(START_PILL_X + 16, pill_y + 9, "WMOS", color::TEXT_PRIMARY, None);

        let (mut tx, tab_w, gap) = self.taskbar_tab_geom(w);
        let tray_limit = w.saturating_sub(tray_width_for(w) + 12);

        for idx in 0..WIN_COUNT {
            if self.vis[idx] == WindowVis::Closed {
                continue;
            }
            if tx + tab_w > tray_limit {
                break;
            }
            let active = self.vis[idx] == WindowVis::Open && self.wins[idx].active;
            let minimized = self.vis[idx] == WindowVis::Minimized;
            draw_taskbar_tab(
                &mut self.fb,
                tx,
                pill_y,
                tab_w,
                pill_h,
                TAB_LABELS[idx],
                active,
                minimized,
                pal,
            );
            tx += tab_w + gap;
        }

        self.redraw_taskbar_tray(self.last_mouse_pos);
    }

    fn redraw_taskbar_tray(&mut self, mouse_pos: (usize, usize)) {
        let w = self.fb.width();
        let h = self.fb.height();
        if h < TASKBAR_H {
            return;
        }
        let ty = h - TASKBAR_H;
        let pill_y = ty + 5;
        let pill_h = 26;
        let tray_w = tray_width_for(w);
        let tray_x = w.saturating_sub(tray_w + 8);
        let clear_w = w.saturating_sub(tray_x);

        self.fb
            .fill_rect(tray_x, pill_y, clear_w, pill_h, color::TASKBAR_BG);

        let dt = rtc::read();
        let mut tray = StackBuf::<44>::new();
        if w >= 760 {
            let _ = write!(
                tray,
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02} | ({},{})",
                dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, mouse_pos.0, mouse_pos.1
            );
        } else {
            let _ = write!(
                tray,
                "{:02}:{:02}:{:02} ({},{})",
                dt.hour, dt.minute, dt.second, mouse_pos.0, mouse_pos.1
            );
        }

        let text_w = tray.as_str().len() * 8;
        let draw_x = if w > text_w + 10 {
            (w - text_w - 10).max(tray_x)
        } else {
            tray_x
        };
        let max_chars = w.saturating_sub(draw_x + 4) / 8;
        for (i, ch) in tray.as_str().chars().take(max_chars).enumerate() {
            self.fb
                .draw_char(draw_x + i * 8, pill_y + 9, ch, color::TERM_CYAN, None);
        }
    }
}

fn draw_themed_window(fb: &mut Framebuffer, win: &Window, pal: ThemePalette) {
    win.draw(fb);
    if win.active && win.w >= 24 && win.h >= TITLE_BAR_H + 8 {
        let inner_w = win.w.saturating_sub(BORDER_W * 2);
        fb.fill_rect(
            win.x + BORDER_W,
            win.y + BORDER_W,
            inner_w,
            TITLE_BAR_H - BORDER_W,
            pal.title_active,
        );
        fb.draw_rect_outline(win.x, win.y, win.w, win.h, BORDER_W, pal.accent_bright);
        fb.draw_hline(
            win.x + BORDER_W,
            win.y + TITLE_BAR_H,
            inner_w,
            pal.accent_bright,
        );

        let dot_y = win.y + (TITLE_BAR_H.saturating_sub(8)) / 2;
        draw_dot(fb, win.x + 10, dot_y, color::DOT_RED);
        draw_dot(fb, win.x + 22, dot_y, color::DOT_YELLOW);
        draw_dot(fb, win.x + 34, dot_y, color::DOT_GREEN);

        let title_x = win.x + 50;
        let title_y = win.y + (TITLE_BAR_H.saturating_sub(8)) / 2;
        let max_chars = win.w.saturating_sub(58) / 8;
        for (idx, ch) in win.title.chars().take(max_chars).enumerate() {
            fb.draw_char(title_x + idx * 8, title_y, ch, color::TEXT_PRIMARY, None);
        }
    }
}

fn draw_dot(fb: &mut Framebuffer, x: usize, y: usize, col: Color) {
    fb.fill_rect(x + 1, y, 6, 8, col);
    fb.fill_rect(x, y + 1, 8, 6, col);
}

fn tray_width_for(w: usize) -> usize {
    if w >= 760 {
        252
    } else {
        152
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
    minimized: bool,
    pal: ThemePalette,
) {
    let bg = if active {
        color::TASKBAR_TAB_ACTIVE
    } else {
        color::TASKBAR_TAB_INACTIVE
    };
    let border = if active {
        pal.accent_bright
    } else {
        color::WIN_BORDER
    };
    let fg = if active {
        color::TEXT_PRIMARY
    } else if minimized {
        color::TERM_AMBER
    } else {
        color::TEXT_MUTED
    };

    fb.fill_rect(x, y, w, h, bg);
    fb.draw_rect_outline(x, y, w, h, 1, border);
    if active {
        fb.draw_hline(x + 1, y + h - 2, w - 2, pal.accent_bright);
    }
    let text_w = label.len() * 8;
    let tx = x + w.saturating_sub(text_w) / 2;
    fb.draw_str(tx, y + 9, label, fg, None);
}

fn render_desktop_header(fb: &mut Framebuffer, w: usize, work_h: usize) {
    if w >= 640 && work_h >= 460 {
        fb.draw_str(
            24,
            14,
            "WMOS v4.0.0 // x86_64 PCI + AHCI FAT32 RW + Multitask Desktop",
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

fn compute_initial_windows(
    w: usize,
    work_h: usize,
) -> ([Window; WIN_COUNT], [(usize, usize, usize, usize); WIN_COUNT]) {
    let (r0, r1, r2, r3, r4) = if w >= 780 {
        let gap = if w >= 1000 { 24 } else { 16 };
        let side_margin = if w >= 1000 { 32 } else { 16 };
        let usable_w = w.saturating_sub(side_margin * 2 + gap);
        let left_w = ((usable_w * 46) / 100).clamp(320, 480);
        let right_w = usable_w.saturating_sub(left_w).clamp(320, 580);
        let total_pair_w = left_w + gap + right_w;
        let start_x = w.saturating_sub(total_pair_w) / 2;

        let top_min = if work_h >= 460 { 40 } else { 16 };
        let win_h = work_h.saturating_sub(top_min + 20).clamp(240, 430);
        let win_y = top_min + work_h.saturating_sub(top_min + win_h) / 3;

        let pw = 400usize.min(w.saturating_sub(40));
        let ph = 300usize.min(work_h.saturating_sub(32));
        let px = (start_x + 48).min(w.saturating_sub(pw + 8));
        let py = (win_y + 24).min(work_h.saturating_sub(ph + 8));

        let nw = 430usize.min(w.saturating_sub(40));
        let nh = 310usize.min(work_h.saturating_sub(32));
        let nx = (start_x + 110).min(w.saturating_sub(nw + 8));
        let ny = (win_y + 40).min(work_h.saturating_sub(nh + 8));

        let fw = 450usize.min(w.saturating_sub(40));
        let fh = 320usize.min(work_h.saturating_sub(32));
        let fx = (start_x + 80).min(w.saturating_sub(fw + 8));
        let fy = (win_y + 32).min(work_h.saturating_sub(fh + 8));

        (
            (start_x, win_y, left_w, win_h),
            (start_x + left_w + gap, win_y, right_w, win_h),
            (px, py, pw, ph),
            (nx, ny, nw, nh),
            (fx, fy, fw, fh),
        )
    } else {
        let margin_x = 12usize.min(w / 16);
        let win_w = w.saturating_sub(margin_x * 2).max(160);
        let gap_y = 8usize;
        let top_y = 10usize;
        let avail_h = work_h.saturating_sub(top_y + gap_y + 8);
        let top_h = ((avail_h * 52) / 100).max(120);
        let bot_h = avail_h.saturating_sub(top_h).max(100);

        let pw = w.saturating_sub(40).min(380);
        let ph = work_h.saturating_sub(40).min(290);

        (
            (margin_x, top_y, win_w, top_h),
            (margin_x, top_y + top_h + gap_y, win_w, bot_h),
            (20, 20, pw, ph),
            (28, 28, pw, ph),
            (36, 36, pw, ph),
        )
    };

    let wins = [
        Window {
            x: r0.0,
            y: r0.1,
            w: r0.2,
            h: r0.3,
            title: "System Info",
            active: false,
        },
        Window {
            x: r1.0,
            y: r1.1,
            w: r1.2,
            h: r1.3,
            title: "WMOS Terminal",
            active: true,
        },
        Window {
            x: r2.0,
            y: r2.1,
            w: r2.2,
            h: r2.3,
            title: "WMOS Paint",
            active: false,
        },
        Window {
            x: r3.0,
            y: r3.1,
            w: r3.2,
            h: r3.3,
            title: "WMOS Notepad",
            active: false,
        },
        Window {
            x: r4.0,
            y: r4.1,
            w: r4.2,
            h: r4.3,
            title: "WMOS Files",
            active: false,
        },
    ];

    (wins, [r0, r1, r2, r3, r4])
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
    theme: Theme,
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
    let line_step = if win.h >= 380 { 16usize } else { 13usize };
    let pal = theme.palette();

    let mut line = StackBuf::<96>::new();

    draw_clipped_line(
        fb,
        x,
        y,
        max_w,
        max_y,
        "OS      : wmos v4.0.0 (x86_64 no_std)",
        color::TEXT_PRIMARY,
    );
    y += line_step;

    let dt = rtc::read();
    let _ = write!(
        line,
        "RTC     : {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_CYAN);
    y += line_step;

    line.clear();
    let _ = write!(
        line,
        "Display : {}x{} {}bpp | DB={} ({} KiB)",
        summary.fb.width,
        summary.fb.height,
        summary.fb.bpp,
        if fb.is_double_buffered() { "ON" } else { "OFF" },
        fb.backbuffer_bytes() / 1024
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_PRIMARY);
    y += line_step;

    let p = pmm::stats();
    let free_mib = (p.free_frames as u64 * PAGE_SIZE) / MIB;
    let total_mib = (p.total_frames as u64 * PAGE_SIZE) / MIB;
    line.clear();
    let _ = write!(line, "PMM     : {} / {} MiB free", free_mib, total_mib);
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_GREEN);
    y += line_step;

    let bar_h = 10usize;
    if y + bar_h <= max_y {
        draw_progress_bar(
            fb,
            x,
            y,
            max_w.min(360),
            bar_h,
            p.free_frames as u64,
            (p.total_frames as u64).max(1),
            pal.accent,
            color::PROGRESS_BG,
            color::PROGRESS_BORDER,
        );
        y += bar_h + 5;
    }

    let hst = heap::stats();
    line.clear();
    let _ = write!(
        line,
        "Heap    : {}/{} KiB ({} allocs) | VMM {}",
        hst.used_bytes / 1024,
        hst.total_bytes / 1024,
        hst.alloc_count,
        if vmm_ok { "PASS" } else { "FAIL" }
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_CYAN);
    y += line_step;

    if y + bar_h <= max_y {
        draw_progress_bar(
            fb,
            x,
            y,
            max_w.min(360),
            bar_h,
            hst.used_bytes as u64,
            (hst.total_bytes as u64).max(1),
            pal.accent_bright,
            color::PROGRESS_BG,
            color::PROGRESS_BORDER,
        );
        y += bar_h + 5;
    }

    let fst = fs::stats();
    let disk_cnt = fs::list_disk().len();
    line.clear();
    let _ = write!(
        line,
        "Storage : RAMFS {} ({} B) | FAT32 {} files",
        fst.file_count, fst.total_bytes, disk_cnt
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_GREEN);
    y += line_step;

    let pci_cnt = pci::devices().len();
    let task_cnt = task::list().len();
    let switches = task::total_switches();
    line.clear();
    let _ = write!(
        line,
        "PCI/Task: {} devs | {} tasks ({} sw)",
        pci_cnt, task_cnt, switches
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_PRIMARY);
    y += line_step;

    let ticks = pit::ticks();
    let secs = pit::uptime_secs();
    line.clear();
    let _ = write!(
        line,
        "IRQs    : KBD {} | MSE {} | {:02}:{:02}:{:02} ({}t)",
        keyboard::irq_count(),
        mouse::irq_count(),
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        ticks
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TEXT_MUTED);
    y += line_step;

    line.clear();
    let _ = write!(
        line,
        "Mouse   : ({}, {}) btn={:#x} | Theme {}",
        mouse_pos.0,
        mouse_pos.1,
        mouse_buttons,
        theme.name()
    );
    draw_clipped_line(fb, x, y, max_w, max_y, line.as_str(), color::TERM_CYAN);
    y += line_step + 2;

    if y + 4 <= max_y {
        fb.draw_hline(x, y, max_w, color::WIN_BORDER);
        y += 8;
    }

    let checklist: [(&str, Color); 4] = [
        ("[OK] Kernel : GDT/IDT/PIC/PMM/VMM/Heap", color::TERM_GREEN),
        ("[OK] Drivers: PCI/AHCI/RTC/PIT/PS2", color::TERM_GREEN),
        ("[OK] Storage: RAMFS + FAT32 + Files", color::TERM_GREEN),
        ("[OK] Desktop: Double-Buf + Sched + Apps", color::TERM_GREEN),
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

fn draw_prompt_line(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    max_w: usize,
    max_y: usize,
    term: &TerminalState,
    blink_on: bool,
) {
    if y + 8 > max_y || max_w < 8 {
        return;
    }
    let prefix = "wmos@kernel:~$ ";
    let mut line_buf = StackBuf::<128>::new();
    let _ = write!(line_buf, "{}{}", prefix, term.input_str());
    draw_clipped_line(fb, x, y, max_w, max_y, line_buf.as_str(), color::TEXT_PRIMARY);

    if blink_on {
        let caret_col = prefix.len() + term.cursor_pos();
        let caret_x = x + caret_col * 8;
        if caret_x + 8 <= x + max_w {
            let under = term
                .input_str()
                .as_bytes()
                .get(term.cursor_pos())
                .copied()
                .map(|b| b as char)
                .unwrap_or(' ');
            fb.draw_char(caret_x, y, under, color::TERM_BG, Some(color::TERM_CYAN));
        }
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

    draw_prompt_line(fb, x, y, max_w, max_y, term, blink_on);
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

    if y + 10 > max_y || max_w < 8 {
        return;
    }

    fb.fill_rect(x, y, max_w, 10, color::TERM_BG);
    draw_prompt_line(fb, x, y, max_w, max_y, term, blink_on);
}
