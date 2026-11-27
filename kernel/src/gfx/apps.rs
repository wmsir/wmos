use alloc::string::String;
use alloc::vec::Vec;

use crate::drivers::keyboard::KeyEvent;
use crate::fs;
use crate::gfx::color::{self, Color};
use crate::gfx::fb::Framebuffer;
use crate::gfx::ui::{BORDER_W, Window};

const TOOLBAR_H: usize = 28;

pub const PAINT_CANVAS_W: usize = 360;
pub const PAINT_CANVAS_H: usize = 220;
const PAINT_BG: Color = Color::rgb(245, 247, 250);

const SWATCH_COLORS: [Color; 6] = [
    Color::rgb(24, 28, 38),
    color::DOT_RED,
    color::DOT_YELLOW,
    color::DOT_GREEN,
    color::TERM_CYAN,
    color::ACCENT,
];

pub struct PaintApp {
    canvas: Vec<Color>,
    active_swatch: usize,
    brush_radius: usize,
    last_pt: Option<(isize, isize)>,
}

impl PaintApp {
    pub fn new() -> Self {
        Self {
            canvas: alloc::vec![PAINT_BG; PAINT_CANVAS_W * PAINT_CANVAS_H],
            active_swatch: 5,
            brush_radius: 1,
            last_pt: None,
        }
    }

    fn clear_canvas(&mut self) {
        self.canvas.fill(PAINT_BG);
    }

    fn plot_canvas_brush(&mut self, cx: isize, cy: isize, radius: isize, col: Color) {
        for oy in (cy - radius)..=(cy + radius) {
            if oy < 0 || oy >= PAINT_CANVAS_H as isize {
                continue;
            }
            for ox in (cx - radius)..=(cx + radius) {
                if ox < 0 || ox >= PAINT_CANVAS_W as isize {
                    continue;
                }
                self.canvas[oy as usize * PAINT_CANVAS_W + ox as usize] = col;
            }
        }
    }

    fn stroke_canvas(&mut self, x0: isize, y0: isize, x1: isize, y1: isize, radius: usize, col: Color) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut x = x0;
        let mut y = y0;
        let r = radius as isize;

        loop {
            self.plot_canvas_brush(x, y, r, col);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    fn canvas_viewport(win: &Window) -> Option<(usize, usize, usize, usize)> {
        let (ox, oy) = win.client_origin();
        let cx = ox + 8;
        let cy = oy + TOOLBAR_H + 6;
        let max_w = win.w.saturating_sub(BORDER_W * 2 + 16);
        let max_h = (win.y + win.h).saturating_sub(cy + 8);
        let vw = PAINT_CANVAS_W.min(max_w);
        let vh = PAINT_CANVAS_H.min(max_h);
        if vw < 16 || vh < 16 {
            None
        } else {
            Some((cx, cy, vw, vh))
        }
    }

    pub fn render(&self, fb: &mut Framebuffer, win: &Window) {
        let (ox, oy) = win.client_origin();
        let cw = win.w.saturating_sub(BORDER_W * 2);
        let ch = (win.y + win.h).saturating_sub(oy + BORDER_W);
        if cw < 32 || ch < TOOLBAR_H + 16 {
            return;
        }

        fb.fill_rect(ox, oy, cw, ch, color::WIN_BG);
        fb.fill_rect(ox, oy, cw, TOOLBAR_H, color::WIN_TITLE_INACTIVE);
        fb.draw_hline(ox, oy + TOOLBAR_H, cw, color::WIN_BORDER);

        // 6 color swatches
        for (i, &col) in SWATCH_COLORS.iter().enumerate() {
            let sx = ox + 8 + i * 24;
            let sy = oy + 5;
            if sx + 18 >= ox + cw {
                break;
            }
            fb.fill_rect(sx, sy, 18, 18, col);
            let border = if i == self.active_swatch {
                color::TEXT_PRIMARY
            } else {
                color::WIN_BORDER
            };
            fb.draw_rect_outline(sx, sy, 18, 18, 1, border);
        }

        // Brush size toggle [S] / [L]
        let size_x = ox + 158;
        let size_y = oy + 5;
        if size_x + 36 < ox + cw {
            let size_label = if self.brush_radius <= 1 { "[S]" } else { "[L]" };
            draw_button(fb, size_x, size_y, 32, 18, size_label, color::TASKBAR_TAB_ACTIVE);
        }

        // [Clear] button
        let clr_x = ox + 198;
        if clr_x + 56 < ox + cw {
            draw_button(fb, clr_x, size_y, 52, 18, "Clear", color::TASKBAR_TAB_INACTIVE);
        }

        if let Some((cx, cy, vw, vh)) = Self::canvas_viewport(win) {
            fb.draw_rect_outline(cx - 1, cy - 1, vw + 2, vh + 2, 1, color::WIN_BORDER);
            for py in 0..vh {
                let row_off = py * PAINT_CANVAS_W;
                for px in 0..vw {
                    fb.put_pixel(cx + px, cy + py, self.canvas[row_off + px]);
                }
            }
        }
    }

    pub fn handle_mouse(
        &mut self,
        win: &Window,
        mx: usize,
        my: usize,
        left: bool,
        right: bool,
        just_pressed: bool,
    ) -> bool {
        if !left && !right {
            self.last_pt = None;
            return false;
        }

        let (ox, oy) = win.client_origin();
        if just_pressed && left && my >= oy && my < oy + TOOLBAR_H {
            for i in 0..SWATCH_COLORS.len() {
                let sx = ox + 8 + i * 24;
                let sy = oy + 5;
                if mx >= sx && mx < sx + 18 && my >= sy && my < sy + 18 {
                    self.active_swatch = i;
                    return true;
                }
            }
            let size_x = ox + 158;
            let size_y = oy + 5;
            if mx >= size_x && mx < size_x + 32 && my >= size_y && my < size_y + 18 {
                self.brush_radius = if self.brush_radius <= 1 { 3 } else { 1 };
                return true;
            }
            let clr_x = ox + 198;
            if mx >= clr_x && mx < clr_x + 52 && my >= size_y && my < size_y + 18 {
                self.clear_canvas();
                return true;
            }
        }

        if let Some((cx, cy, vw, vh)) = Self::canvas_viewport(win) {
            if mx >= cx && mx < cx + vw && my >= cy && my < cy + vh {
                let lx = (mx - cx) as isize;
                let ly = (my - cy) as isize;
                let col = if right {
                    PAINT_BG
                } else {
                    SWATCH_COLORS[self.active_swatch]
                };
                let rad = if right { 3 } else { self.brush_radius };
                let (px0, py0) = self.last_pt.unwrap_or((lx, ly));
                self.stroke_canvas(px0, py0, lx, ly, rad, col);
                self.last_pt = Some((lx, ly));
                return true;
            }
        }

        self.last_pt = None;
        false
    }
}

pub struct NotepadApp {
    text: String,
    status: &'static str,
}

impl NotepadApp {
    pub fn new() -> Self {
        let text = fs::read_to_string("notes.txt").unwrap_or_default();
        Self {
            text,
            status: "Loaded",
        }
    }

    pub fn handle_key(&mut self, ev: KeyEvent) -> bool {
        match ev {
            KeyEvent::Char(b) if (0x20..=0x7E).contains(&b) => {
                self.text.push(b as char);
                self.status = "Modified";
                true
            }
            KeyEvent::Enter => {
                self.text.push('\n');
                self.status = "Modified";
                true
            }
            KeyEvent::Backspace => {
                if self.text.pop().is_some() {
                    self.status = "Modified";
                    true
                } else {
                    false
                }
            }
            KeyEvent::Tab => {
                self.text.push_str("  ");
                self.status = "Modified";
                true
            }
            _ => false,
        }
    }

    pub fn handle_mouse(
        &mut self,
        win: &Window,
        mx: usize,
        my: usize,
        just_pressed: bool,
    ) -> bool {
        if !just_pressed {
            return false;
        }
        let (ox, oy) = win.client_origin();
        let by = oy + 5;
        if my < by || my >= by + 18 {
            return false;
        }
        let load_x = ox + 92;
        let save_x = ox + 144;
        let clear_x = ox + 196;

        if mx >= load_x && mx < load_x + 46 {
            self.text = fs::read_to_string("notes.txt").unwrap_or_default();
            self.status = "Loaded";
            return true;
        }
        if mx >= save_x && mx < save_x + 46 {
            let _ = fs::write("notes.txt", self.text.as_bytes());
            crate::serial_println!("[wmos] notepad saved notes.txt ({} B)", self.text.len());
            self.status = "Saved";
            return true;
        }
        if mx >= clear_x && mx < clear_x + 52 {
            self.text.clear();
            self.status = "Cleared";
            return true;
        }
        false
    }

    pub fn render(&self, fb: &mut Framebuffer, win: &Window, blink: bool) {
        let (ox, oy) = win.client_origin();
        let cw = win.w.saturating_sub(BORDER_W * 2);
        let ch = (win.y + win.h).saturating_sub(oy + BORDER_W);
        if cw < 64 || ch < TOOLBAR_H + 24 {
            return;
        }

        fb.fill_rect(ox, oy, cw, ch, color::WIN_BG);
        fb.fill_rect(ox, oy, cw, TOOLBAR_H, color::WIN_TITLE_INACTIVE);
        fb.draw_hline(ox, oy + TOOLBAR_H, cw, color::WIN_BORDER);

        fb.draw_str(ox + 8, oy + 10, "notes.txt", color::TERM_CYAN, None);

        let by = oy + 5;
        if ox + 252 < ox + cw {
            draw_button(fb, ox + 92, by, 46, 18, "Load", color::TASKBAR_TAB_INACTIVE);
            draw_button(fb, ox + 144, by, 46, 18, "Save", color::TASKBAR_TAB_ACTIVE);
            draw_button(fb, ox + 196, by, 52, 18, "Clear", color::TASKBAR_TAB_INACTIVE);
        }

        let status_w = self.status.len() * 8;
        if cw > 264 + status_w {
            let st_col = match self.status {
                "Saved" => color::TERM_GREEN,
                "Modified" => color::TERM_AMBER,
                _ => color::TEXT_MUTED,
            };
            fb.draw_str(ox + cw - status_w - 8, oy + 10, self.status, st_col, None);
        }

        // Editor body
        let ex = ox + 6;
        let ey = oy + TOOLBAR_H + 6;
        let ew = cw.saturating_sub(12);
        let eh = ch.saturating_sub(TOOLBAR_H + 12);
        if ew < 24 || eh < 24 {
            return;
        }

        fb.fill_rect(ex, ey, ew, eh, color::TERM_BG);
        fb.draw_rect_outline(ex, ey, ew, eh, 1, color::WIN_BORDER);

        let text_x = ex + 8;
        let text_y = ey + 8;
        let max_cols = ew.saturating_sub(16) / 8;
        let line_step = 14usize;
        let max_rows = eh.saturating_sub(12) / line_step;
        if max_cols == 0 || max_rows == 0 {
            return;
        }

        let mut wrapped: Vec<String> = Vec::new();
        let mut cur = String::new();
        for ch_val in self.text.chars() {
            if ch_val == '\n' {
                wrapped.push(cur);
                cur = String::new();
            } else {
                if cur.len() >= max_cols {
                    wrapped.push(cur);
                    cur = String::new();
                }
                cur.push(ch_val);
            }
        }
        wrapped.push(cur);

        let skip = wrapped.len().saturating_sub(max_rows);
        let mut row_idx = 0usize;
        for (i, line) in wrapped.iter().skip(skip).enumerate() {
            let y = text_y + i * line_step;
            fb.draw_str(text_x, y, line.as_str(), color::TEXT_PRIMARY, None);
            row_idx = i;
        }

        if blink {
            if let Some(last_line) = wrapped.last() {
                let cx = text_x + last_line.len().min(max_cols.saturating_sub(1)) * 8;
                let cy = text_y + row_idx * line_step;
                fb.draw_char(cx, cy, '_', color::TERM_CYAN, None);
            }
        }
    }
}

fn draw_button(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    label: &str,
    bg: Color,
) {
    fb.fill_rect(x, y, w, h, bg);
    fb.draw_rect_outline(x, y, w, h, 1, color::WIN_BORDER);
    let text_w = label.len() * 8;
    let tx = x + w.saturating_sub(text_w) / 2;
    let ty = y + h.saturating_sub(8) / 2;
    fb.draw_str(tx, ty, label, color::TEXT_PRIMARY, None);
}
