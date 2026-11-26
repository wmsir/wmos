use core::fmt;

use crate::gfx::color::{self, Color};
use crate::gfx::fb::Framebuffer;

const TITLE_BAR_H: usize = 26;
const BORDER_W: usize = 1;

/// Fixed-capacity stack string buffer for `no_std` formatting without heap allocation.
pub struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let avail = N.saturating_sub(self.len);
        if avail == 0 {
            return Ok(());
        }
        let bytes = s.as_bytes();
        let mut copy_len = bytes.len().min(avail);
        while copy_len > 0 && !s.is_char_boundary(copy_len) {
            copy_len -= 1;
        }
        self.buf[self.len..self.len + copy_len].copy_from_slice(&bytes[..copy_len]);
        self.len += copy_len;
        Ok(())
    }
}

pub struct Window {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub title: &'static str,
    pub active: bool,
}

impl Window {
    pub fn draw(&self, fb: &mut Framebuffer) {
        if self.w < 24 || self.h < TITLE_BAR_H + 8 {
            return;
        }

        // Subtle drop shadow
        fb.fill_rect(
            self.x.saturating_add(4),
            self.y.saturating_add(4),
            self.w,
            self.h,
            color::WIN_SHADOW,
        );

        // Window body
        fb.fill_rect(self.x, self.y, self.w, self.h, color::WIN_BG);

        let border_col = if self.active {
            color::WIN_BORDER_ACTIVE
        } else {
            color::WIN_BORDER
        };
        let title_bg = if self.active {
            color::WIN_TITLE_ACTIVE
        } else {
            color::WIN_TITLE_INACTIVE
        };

        // Title bar background & separator
        let inner_w = self.w.saturating_sub(BORDER_W * 2);
        fb.fill_rect(
            self.x + BORDER_W,
            self.y + BORDER_W,
            inner_w,
            TITLE_BAR_H - BORDER_W,
            title_bg,
        );
        fb.draw_hline(
            self.x + BORDER_W,
            self.y + TITLE_BAR_H,
            inner_w,
            border_col,
        );

        // Outer border
        fb.draw_rect_outline(self.x, self.y, self.w, self.h, BORDER_W, border_col);

        // Three window control dots
        let dot_y = self.y + (TITLE_BAR_H.saturating_sub(8)) / 2;
        draw_dot(fb, self.x + 10, dot_y, color::DOT_RED);
        draw_dot(fb, self.x + 22, dot_y, color::DOT_YELLOW);
        draw_dot(fb, self.x + 34, dot_y, color::DOT_GREEN);

        // Title text (clipped to remaining title bar width)
        let title_x = self.x + 50;
        let title_y = self.y + (TITLE_BAR_H.saturating_sub(8)) / 2;
        let max_chars = self.w.saturating_sub(58) / 8;
        let title_fg = if self.active {
            color::TEXT_PRIMARY
        } else {
            color::TEXT_MUTED
        };
        for (idx, ch) in self.title.chars().take(max_chars).enumerate() {
            fb.draw_char(title_x + idx * 8, title_y, ch, title_fg, None);
        }
    }

    pub fn client_origin(&self) -> (usize, usize) {
        (self.x + BORDER_W, self.y + TITLE_BAR_H + 1)
    }
}

fn draw_dot(fb: &mut Framebuffer, x: usize, y: usize, col: Color) {
    fb.fill_rect(x + 1, y, 6, 8, col);
    fb.fill_rect(x, y + 1, 8, 6, col);
}

pub fn draw_progress_bar(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    numerator: u64,
    denominator: u64,
    fill_color: Color,
    bg_color: Color,
    border_color: Color,
) {
    if w < 4 || h < 4 {
        return;
    }
    fb.fill_rect(x, y, w, h, bg_color);
    fb.draw_rect_outline(x, y, w, h, 1, border_color);

    let inner_w = w.saturating_sub(2);
    let inner_h = h.saturating_sub(2);
    let fill_w = if denominator == 0 {
        0
    } else {
        let clamped = numerator.min(denominator);
        ((clamped as u128 * inner_w as u128) / (denominator as u128)) as usize
    };
    if fill_w > 0 && inner_h > 0 {
        fb.fill_rect(x + 1, y + 1, fill_w, inner_h, fill_color);
    }
}

pub const CURSOR_W: usize = 12;
pub const CURSOR_H: usize = 16;

// 0 = transparent, 1 = dark outline, 2 = bright pointer fill
const CURSOR_BITMAP: [[u8; CURSOR_W]; CURSOR_H] = [
    [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 1, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 1, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 2, 1, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 2, 2, 1, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 2, 2, 2, 1, 0, 0],
    [1, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 0],
    [1, 2, 2, 1, 2, 2, 1, 0, 0, 0, 0, 0],
    [1, 2, 1, 0, 1, 2, 2, 1, 0, 0, 0, 0],
    [1, 1, 0, 0, 1, 2, 2, 1, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 1, 2, 1, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0],
];

pub struct CursorLayer {
    saved_bg: [u32; CURSOR_W * CURSOR_H],
    saved_x: usize,
    saved_y: usize,
    has_saved: bool,
}

impl CursorLayer {
    pub const fn new() -> Self {
        Self {
            saved_bg: [0u32; CURSOR_W * CURSOR_H],
            saved_x: 0,
            saved_y: 0,
            has_saved: false,
        }
    }

    pub fn save_and_draw(&mut self, fb: &mut Framebuffer, x: usize, y: usize) {
        self.saved_x = x;
        self.saved_y = y;
        self.has_saved = true;

        for (row, pattern) in CURSOR_BITMAP.iter().enumerate() {
            let py = y.saturating_add(row);
            for (col, &pixel) in pattern.iter().enumerate() {
                let px = x.saturating_add(col);
                self.saved_bg[row * CURSOR_W + col] = fb.get_pixel_raw(px, py);
                match pixel {
                    1 => fb.put_pixel(px, py, color::CURSOR_OUTLINE),
                    2 => fb.put_pixel(px, py, color::CURSOR_FILL),
                    _ => {}
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn restore(&mut self, fb: &mut Framebuffer) {
        if !self.has_saved {
            return;
        }
        for row in 0..CURSOR_H {
            let py = self.saved_y.saturating_add(row);
            for col in 0..CURSOR_W {
                if CURSOR_BITMAP[row][col] != 0 {
                    let px = self.saved_x.saturating_add(col);
                    let raw = self.saved_bg[row * CURSOR_W + col];
                    fb.put_pixel_raw(px, py, raw);
                }
            }
        }
        self.has_saved = false;
    }
}
