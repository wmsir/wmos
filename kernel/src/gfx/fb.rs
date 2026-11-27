use alloc::vec::Vec;

use crate::boot_info::FramebufferInfo;
use crate::gfx::color::Color;
use crate::gfx::font;

const MAX_BACKBUFFER_PIXELS: usize = 3_000_000;

pub struct Framebuffer {
    info: FramebufferInfo,
    backbuffer: Option<Vec<u32>>,
}

impl Framebuffer {
    pub fn new(info: FramebufferInfo) -> Self {
        let backbuffer = match info.width.checked_mul(info.height) {
            Some(n)
                if !info.addr.is_null()
                    && n > 0
                    && n <= MAX_BACKBUFFER_PIXELS
                    && (info.bpp == 32 || info.bpp == 24) =>
            {
                Some(alloc::vec![0u32; n])
            }
            _ => None,
        };
        Self { info, backbuffer }
    }

    pub fn width(&self) -> usize {
        self.info.width
    }

    pub fn height(&self) -> usize {
        self.info.height
    }

    pub fn is_double_buffered(&self) -> bool {
        self.backbuffer.is_some()
    }

    pub fn backbuffer_bytes(&self) -> usize {
        self.backbuffer
            .as_ref()
            .map_or(0, |v| v.len().saturating_mul(core::mem::size_of::<u32>()))
    }

    #[inline]
    fn pixel_offset(&self, x: usize, y: usize) -> Option<usize> {
        if self.info.addr.is_null() || x >= self.info.width || y >= self.info.height {
            return None;
        }
        let bpp_bytes = match self.info.bpp {
            32 => 4usize,
            24 => 3usize,
            _ => return None,
        };
        y.checked_mul(self.info.pitch)?
            .checked_add(x.checked_mul(bpp_bytes)?)
    }

    #[inline]
    unsafe fn write_vram_raw_at(addr: *mut u8, bpp: u16, offset: usize, raw: u32) {
        let p = addr.add(offset);
        match bpp {
            32 => {
                if (p as usize) & 3 == 0 {
                    p.cast::<u32>().write_volatile(raw);
                } else {
                    let bytes = raw.to_le_bytes();
                    p.add(0).write_volatile(bytes[0]);
                    p.add(1).write_volatile(bytes[1]);
                    p.add(2).write_volatile(bytes[2]);
                    p.add(3).write_volatile(bytes[3]);
                }
            }
            24 => {
                let bytes = raw.to_le_bytes();
                p.add(0).write_volatile(bytes[0]);
                p.add(1).write_volatile(bytes[1]);
                p.add(2).write_volatile(bytes[2]);
            }
            _ => {}
        }
    }

    pub fn put_pixel(&mut self, x: usize, y: usize, color: Color) {
        let raw = color.pack(&self.info);
        self.put_pixel_raw(x, y, raw);
    }

    pub fn put_pixel_raw(&mut self, x: usize, y: usize, raw: u32) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        if let Some(ref mut bb) = self.backbuffer {
            bb[y * self.info.width + x] = raw;
            return;
        }
        if let Some(offset) = self.pixel_offset(x, y) {
            unsafe {
                Self::write_vram_raw_at(self.info.addr, self.info.bpp, offset, raw);
            }
        }
    }

    pub fn get_pixel_raw(&self, x: usize, y: usize) -> u32 {
        if x >= self.info.width || y >= self.info.height {
            return 0;
        }
        if let Some(ref bb) = self.backbuffer {
            return bb[y * self.info.width + x];
        }
        let Some(offset) = self.pixel_offset(x, y) else {
            return 0;
        };
        unsafe {
            let p = self.info.addr.add(offset);
            match self.info.bpp {
                32 => {
                    if (p as usize) & 3 == 0 {
                        p.cast::<u32>().read_volatile()
                    } else {
                        let b0 = p.add(0).read_volatile();
                        let b1 = p.add(1).read_volatile();
                        let b2 = p.add(2).read_volatile();
                        let b3 = p.add(3).read_volatile();
                        u32::from_le_bytes([b0, b1, b2, b3])
                    }
                }
                24 => {
                    let b0 = p.add(0).read_volatile();
                    let b1 = p.add(1).read_volatile();
                    let b2 = p.add(2).read_volatile();
                    u32::from_le_bytes([b0, b1, b2, 0])
                }
                _ => 0,
            }
        }
    }

    /// Flushes a dirty rectangle from the RAM backbuffer to MMIO VRAM.
    pub fn present_rect(&self, x: usize, y: usize, w: usize, h: usize) {
        let Some(ref bb) = self.backbuffer else {
            return;
        };
        if self.info.addr.is_null() || w == 0 || h == 0 {
            return;
        }
        let bpp_bytes = match self.info.bpp {
            32 => 4usize,
            24 => 3usize,
            _ => return,
        };
        let x_end = x.saturating_add(w).min(self.info.width);
        let y_end = y.saturating_add(h).min(self.info.height);
        if x >= x_end || y >= y_end {
            return;
        }

        for py in y..y_end {
            let Some(row_base) = py.checked_mul(self.info.pitch) else {
                break;
            };
            let Some(col_off) = x.checked_mul(bpp_bytes) else {
                break;
            };
            let Some(dst_off) = row_base.checked_add(col_off) else {
                break;
            };
            let src_start = py * self.info.width + x;
            let src_end = py * self.info.width + x_end;
            let src_row = &bb[src_start..src_end];

            unsafe {
                let dst_ptr = self.info.addr.add(dst_off);
                if self.info.bpp == 32 && ((dst_ptr as usize) & 3 == 0) {
                    let dst_u32 = dst_ptr.cast::<u32>();
                    for (i, &raw) in src_row.iter().enumerate() {
                        dst_u32.add(i).write_volatile(raw);
                    }
                } else if self.info.bpp == 32 {
                    for (i, &raw) in src_row.iter().enumerate() {
                        let bytes = raw.to_le_bytes();
                        let p = dst_ptr.add(i * 4);
                        p.add(0).write_volatile(bytes[0]);
                        p.add(1).write_volatile(bytes[1]);
                        p.add(2).write_volatile(bytes[2]);
                        p.add(3).write_volatile(bytes[3]);
                    }
                } else {
                    for (i, &raw) in src_row.iter().enumerate() {
                        let bytes = raw.to_le_bytes();
                        let p = dst_ptr.add(i * 3);
                        p.add(0).write_volatile(bytes[0]);
                        p.add(1).write_volatile(bytes[1]);
                        p.add(2).write_volatile(bytes[2]);
                    }
                }
            }
        }
    }

    pub fn present_all(&self) {
        self.present_rect(0, 0, self.info.width, self.info.height);
    }

    #[allow(dead_code)]
    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.info.width, self.info.height, color);
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Color) {
        if w == 0 || h == 0 {
            return;
        }
        let x_end = x.saturating_add(w).min(self.info.width);
        let y_end = y.saturating_add(h).min(self.info.height);
        if x >= x_end || y >= y_end {
            return;
        }
        let raw = color.pack(&self.info);

        if let Some(ref mut bb) = self.backbuffer {
            let width = self.info.width;
            for py in y..y_end {
                let row_start = py * width + x;
                let row_end = py * width + x_end;
                bb[row_start..row_end].fill(raw);
            }
            return;
        }

        if self.info.addr.is_null() {
            return;
        }
        let bpp_bytes = match self.info.bpp {
            32 => 4usize,
            24 => 3usize,
            _ => return,
        };
        for py in y..y_end {
            let Some(row_base) = py.checked_mul(self.info.pitch) else {
                break;
            };
            for px in x..x_end {
                if let Some(offset) = px.checked_mul(bpp_bytes).and_then(|dx| row_base.checked_add(dx)) {
                    unsafe {
                        Self::write_vram_raw_at(self.info.addr, self.info.bpp, offset, raw);
                    }
                }
            }
        }
    }

    fn fill_gradient_rows(
        &mut self,
        rx: usize,
        ry: usize,
        rw: usize,
        rh: usize,
        grad_y0: usize,
        grad_h: usize,
        top: Color,
        bottom: Color,
    ) {
        if rw == 0 || rh == 0 || grad_h == 0 {
            return;
        }
        let x_end = rx.saturating_add(rw).min(self.info.width);
        let y_end = ry.saturating_add(rh).min(self.info.height);
        if rx >= x_end || ry >= y_end {
            return;
        }
        let denom = grad_h.saturating_sub(1);

        if let Some(ref mut bb) = self.backbuffer {
            let width = self.info.width;
            for py in ry..y_end {
                let num = py.saturating_sub(grad_y0).min(denom);
                let c = if denom == 0 {
                    top
                } else {
                    Color::rgb(
                        lerp_u8(top.r, bottom.r, num, denom),
                        lerp_u8(top.g, bottom.g, num, denom),
                        lerp_u8(top.b, bottom.b, num, denom),
                    )
                };
                let raw = c.pack(&self.info);
                bb[py * width + rx..py * width + x_end].fill(raw);
            }
            return;
        }

        if self.info.addr.is_null() {
            return;
        }
        let bpp_bytes = match self.info.bpp {
            32 => 4usize,
            24 => 3usize,
            _ => return,
        };
        for py in ry..y_end {
            let num = py.saturating_sub(grad_y0).min(denom);
            let c = if denom == 0 {
                top
            } else {
                Color::rgb(
                    lerp_u8(top.r, bottom.r, num, denom),
                    lerp_u8(top.g, bottom.g, num, denom),
                    lerp_u8(top.b, bottom.b, num, denom),
                )
            };
            let raw = c.pack(&self.info);
            let Some(row_base) = py.checked_mul(self.info.pitch) else {
                break;
            };
            for px in rx..x_end {
                if let Some(offset) = px.checked_mul(bpp_bytes).and_then(|dx| row_base.checked_add(dx)) {
                    unsafe {
                        Self::write_vram_raw_at(self.info.addr, self.info.bpp, offset, raw);
                    }
                }
            }
        }
    }

    pub fn fill_gradient_v(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        top: Color,
        bottom: Color,
    ) {
        self.fill_gradient_rows(x, y, w, h, y, h, top, bottom);
    }

    #[allow(dead_code)]
    pub fn fill_gradient_v_region(
        &mut self,
        rx: usize,
        ry: usize,
        rw: usize,
        rh: usize,
        total_h: usize,
        top: Color,
        bottom: Color,
    ) {
        let clamped_end = ry.saturating_add(rh).min(total_h);
        if ry >= clamped_end {
            return;
        }
        self.fill_gradient_rows(rx, ry, rw, clamped_end - ry, 0, total_h, top, bottom);
    }

    pub fn draw_hline(&mut self, x: usize, y: usize, w: usize, color: Color) {
        self.fill_rect(x, y, w, 1, color);
    }

    #[allow(dead_code)]
    pub fn draw_vline(&mut self, x: usize, y: usize, h: usize, color: Color) {
        self.fill_rect(x, y, 1, h, color);
    }

    pub fn draw_rect_outline(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        thickness: usize,
        color: Color,
    ) {
        if w == 0 || h == 0 || thickness == 0 {
            return;
        }
        let t = thickness.min(w).min(h);
        self.fill_rect(x, y, w, t, color);
        self.fill_rect(x, y + h - t, w, t, color);
        self.fill_rect(x, y, t, h, color);
        self.fill_rect(x + w - t, y, t, h, color);
    }

    #[allow(dead_code)]
    pub fn draw_line(
        &mut self,
        x0: isize,
        y0: isize,
        x1: isize,
        y1: isize,
        radius: usize,
        color: Color,
    ) {
        let raw = color.pack(&self.info);
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut x = x0;
        let mut y = y0;
        let r = radius as isize;

        loop {
            for oy in (y - r)..=(y + r) {
                if oy < 0 || oy >= self.info.height as isize {
                    continue;
                }
                for ox in (x - r)..=(x + r) {
                    if ox < 0 || ox >= self.info.width as isize {
                        continue;
                    }
                    self.put_pixel_raw(ox as usize, oy as usize, raw);
                }
            }
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

    pub fn draw_char(&mut self, x: usize, y: usize, ch: char, fg: Color, bg: Option<Color>) {
        let bits = font::glyph(ch);
        let fg_raw = fg.pack(&self.info);
        let bg_raw = bg.map(|c| c.pack(&self.info));

        for (row, &row_bits) in bits.iter().enumerate() {
            let py = y.saturating_add(row);
            for col in 0..8usize {
                let px = x.saturating_add(col);
                if (row_bits & (0x80 >> col)) != 0 {
                    self.put_pixel_raw(px, py, fg_raw);
                } else if let Some(b_raw) = bg_raw {
                    self.put_pixel_raw(px, py, b_raw);
                }
            }
        }
    }

    pub fn draw_str(&mut self, x: usize, y: usize, s: &str, fg: Color, bg: Option<Color>) {
        let mut cx = x;
        let mut cy = y;
        for ch in s.chars() {
            if ch == '\n' {
                cx = x;
                cy = cy.saturating_add(8);
                continue;
            }
            self.draw_char(cx, cy, ch, fg, bg);
            cx = cx.saturating_add(8);
        }
    }

    #[allow(dead_code)]
    pub fn draw_str_scaled(
        &mut self,
        x: usize,
        y: usize,
        s: &str,
        fg: Color,
        bg: Option<Color>,
        scale: usize,
    ) {
        if scale == 0 {
            return;
        }
        if scale == 1 {
            self.draw_str(x, y, s, fg, bg);
            return;
        }
        let mut cx = x;
        let mut cy = y;
        let step = 8usize.saturating_mul(scale);
        for ch in s.chars() {
            if ch == '\n' {
                cx = x;
                cy = cy.saturating_add(step);
                continue;
            }
            let bits = font::glyph(ch);
            for (row, &row_bits) in bits.iter().enumerate() {
                let py = cy.saturating_add(row.saturating_mul(scale));
                for col in 0..8usize {
                    let px = cx.saturating_add(col.saturating_mul(scale));
                    if (row_bits & (0x80 >> col)) != 0 {
                        self.fill_rect(px, py, scale, scale, fg);
                    } else if let Some(bg_col) = bg {
                        self.fill_rect(px, py, scale, scale, bg_col);
                    }
                }
            }
            cx = cx.saturating_add(step);
        }
    }
}

#[inline]
fn lerp_u8(a: u8, b: u8, num: usize, denom: usize) -> u8 {
    let a64 = a as i64;
    let b64 = b as i64;
    let v = a64 + ((b64 - a64) * (num as i64)) / (denom as i64);
    v.clamp(0, 255) as u8
}
