use crate::boot_info::FramebufferInfo;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Packs this RGB color into a raw pixel integer matching the firmware framebuffer layout.
    pub fn pack(self, fb: &FramebufferInfo) -> u32 {
        pack_channel(self.r, fb.red_mask_size, fb.red_mask_shift)
            | pack_channel(self.g, fb.green_mask_size, fb.green_mask_shift)
            | pack_channel(self.b, fb.blue_mask_size, fb.blue_mask_shift)
    }
}

#[inline]
fn pack_channel(val: u8, mask_size: u8, mask_shift: u8) -> u32 {
    if mask_size == 0 || mask_shift >= 32 {
        return 0;
    }
    let scaled = if mask_size == 8 {
        val as u32
    } else if mask_size < 8 {
        let max_val = (1u32 << mask_size) - 1;
        ((val as u32) * max_val + 127) / 255
    } else {
        let capped = if mask_size >= 16 { 16 } else { mask_size };
        let max_val = (1u32 << capped) - 1;
        ((val as u32) * max_val + 127) / 255
    };
    scaled.checked_shl(mask_shift as u32).unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub desktop_top: Color,
    pub desktop_bottom: Color,
    pub accent: Color,
    pub accent_bright: Color,
    pub title_active: Color,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Ocean,
    Amber,
}

impl Theme {
    pub fn next(self) -> Self {
        match self {
            Self::Dark => Self::Ocean,
            Self::Ocean => Self::Amber,
            Self::Amber => Self::Dark,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Ocean => "Ocean",
            Self::Amber => "Amber",
        }
    }

    pub fn palette(self) -> ThemePalette {
        match self {
            Self::Dark => ThemePalette {
                desktop_top: DESKTOP_TOP,
                desktop_bottom: DESKTOP_BOTTOM,
                accent: ACCENT,
                accent_bright: ACCENT_BRIGHT,
                title_active: WIN_TITLE_ACTIVE,
            },
            Self::Ocean => ThemePalette {
                desktop_top: Color::rgb(8, 28, 44),
                desktop_bottom: Color::rgb(14, 68, 96),
                accent: Color::rgb(14, 165, 233),
                accent_bright: Color::rgb(56, 189, 248),
                title_active: Color::rgb(20, 60, 88),
            },
            Self::Amber => ThemePalette {
                desktop_top: Color::rgb(34, 20, 14),
                desktop_bottom: Color::rgb(78, 44, 22),
                accent: Color::rgb(217, 119, 6),
                accent_bright: Color::rgb(251, 191, 36),
                title_active: Color::rgb(72, 46, 28),
            },
        }
    }
}

#[allow(dead_code)]
pub const BLACK: Color = Color::rgb(0, 0, 0);
#[allow(dead_code)]
pub const WHITE: Color = Color::rgb(255, 255, 255);

// Modern dark-slate / indigo OS palette
pub const DESKTOP_TOP: Color = Color::rgb(13, 19, 36);
pub const DESKTOP_BOTTOM: Color = Color::rgb(31, 44, 82);

pub const TASKBAR_BG: Color = Color::rgb(15, 21, 38);
pub const TASKBAR_BORDER: Color = Color::rgb(52, 67, 99);
pub const TASKBAR_TAB_ACTIVE: Color = Color::rgb(42, 56, 92);
pub const TASKBAR_TAB_INACTIVE: Color = Color::rgb(24, 32, 54);

pub const ACCENT: Color = Color::rgb(99, 102, 241);
pub const ACCENT_BRIGHT: Color = Color::rgb(129, 140, 248);

pub const WIN_SHADOW: Color = Color::rgb(7, 10, 20);
pub const WIN_BG: Color = Color::rgb(22, 29, 46);
pub const WIN_BORDER: Color = Color::rgb(64, 79, 112);
pub const WIN_BORDER_ACTIVE: Color = Color::rgb(104, 122, 184);
pub const WIN_TITLE_ACTIVE: Color = Color::rgb(34, 45, 74);
pub const WIN_TITLE_INACTIVE: Color = Color::rgb(26, 34, 56);

pub const TEXT_PRIMARY: Color = Color::rgb(241, 245, 249);
pub const TEXT_MUTED: Color = Color::rgb(148, 163, 184);

pub const TERM_BG: Color = Color::rgb(10, 14, 24);
pub const TERM_GREEN: Color = Color::rgb(74, 222, 128);
pub const TERM_CYAN: Color = Color::rgb(56, 189, 248);
pub const TERM_AMBER: Color = Color::rgb(251, 191, 36);

pub const DOT_RED: Color = Color::rgb(239, 68, 68);
pub const DOT_YELLOW: Color = Color::rgb(245, 158, 11);
pub const DOT_GREEN: Color = Color::rgb(34, 197, 94);

pub const PROGRESS_BG: Color = Color::rgb(14, 20, 34);
pub const PROGRESS_BORDER: Color = Color::rgb(58, 72, 104);

pub const CURSOR_OUTLINE: Color = Color::rgb(10, 14, 24);
pub const CURSOR_FILL: Color = Color::rgb(248, 250, 252);
