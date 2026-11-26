use core::fmt::Write;

use crate::arch::{cpu, pit};
use crate::boot_info::BootSummary;
use crate::drivers::keyboard::{self, KeyEvent};
use crate::drivers::mouse;
use crate::gfx::color::{self, Color};
use crate::gfx::ui::StackBuf;
use crate::mem::{pmm, vmm};

pub const MAX_LINES: usize = 18;
pub const MAX_LINE_LEN: usize = 88;
pub const MAX_INPUT_LEN: usize = 64;
const MIB: u64 = 1024 * 1024;
const PAGE_SIZE: u64 = 4096;

#[derive(Clone, Copy)]
struct TermLine {
    buf: [u8; MAX_LINE_LEN],
    len: usize,
    color: Color,
}

impl TermLine {
    const fn empty() -> Self {
        Self {
            buf: [0u8; MAX_LINE_LEN],
            len: 0,
            color: color::TEXT_PRIMARY,
        }
    }

    fn set(&mut self, text: &str, col: Color) {
        let bytes = text.as_bytes();
        let mut copy_len = bytes.len().min(MAX_LINE_LEN);
        while copy_len > 0 && !text.is_char_boundary(copy_len) {
            copy_len -= 1;
        }
        self.buf[..copy_len].copy_from_slice(&bytes[..copy_len]);
        self.len = copy_len;
        self.color = col;
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

pub struct TerminalState {
    lines: [TermLine; MAX_LINES],
    head: usize,
    count: usize,
    input: [u8; MAX_INPUT_LEN],
    input_len: usize,
}

impl TerminalState {
    pub fn new(summary: &BootSummary, vmm_ok: bool) -> Self {
        let mut term = Self {
            lines: [TermLine::empty(); MAX_LINES],
            head: 0,
            count: 0,
            input: [0u8; MAX_INPUT_LEN],
            input_len: 0,
        };

        term.push_line("[wmos] boot ok", color::TERM_GREEN);
        if vmm_ok {
            term.push_line("[wmos] gdt+tss/idt+pic/pmm/vmm/pit+ps2 ok", color::TERM_GREEN);
        } else {
            term.push_line("[wmos] vmm self-test FAILED", color::DOT_RED);
        }

        let mut buf = StackBuf::<MAX_LINE_LEN>::new();
        let _ = write!(
            buf,
            "[wmos] limine rev={} hhdm={:#x}",
            summary.limine_revision, summary.hhdm_offset
        );
        term.push_line(buf.as_str(), color::TEXT_PRIMARY);

        buf.clear();
        let _ = write!(
            buf,
            "[wmos] fb={}x{} bpp={} pitch={}",
            summary.fb.width, summary.fb.height, summary.fb.bpp, summary.fb.pitch
        );
        term.push_line(buf.as_str(), color::TERM_CYAN);

        term.push_line("[wmos] desktop rendered", color::TERM_GREEN);
        term.push_line("Type 'help' for commands.", color::TERM_AMBER);
        term
    }

    pub fn push_line(&mut self, text: &str, col: Color) {
        for raw_line in text.split('\n') {
            let slot = if self.count < MAX_LINES {
                let idx = (self.head + self.count) % MAX_LINES;
                self.count += 1;
                idx
            } else {
                let idx = self.head;
                self.head = (self.head + 1) % MAX_LINES;
                idx
            };
            self.lines[slot].set(raw_line, col);
        }
    }

    pub fn line_count(&self) -> usize {
        self.count
    }

    pub fn line(&self, idx: usize) -> Option<(&str, Color)> {
        if idx >= self.count {
            return None;
        }
        let slot = (self.head + idx) % MAX_LINES;
        Some((self.lines[slot].as_str(), self.lines[slot].color))
    }

    pub fn input_str(&self) -> &str {
        core::str::from_utf8(&self.input[..self.input_len]).unwrap_or("")
    }

    pub fn format_prompt(&self, buf: &mut StackBuf<96>, blink_on: bool) {
        buf.clear();
        let _ = write!(buf, "wmos@kernel:~$ {}", self.input_str());
        if blink_on {
            let _ = buf.write_str("_");
        }
    }

    pub fn handle_key(&mut self, ev: KeyEvent, summary: &BootSummary, _vmm_ok: bool) {
        match ev {
            KeyEvent::Char(b) => {
                if (0x20..=0x7E).contains(&b) && self.input_len < MAX_INPUT_LEN {
                    self.input[self.input_len] = b;
                    self.input_len += 1;
                }
            }
            KeyEvent::Backspace => {
                if self.input_len > 0 {
                    self.input_len -= 1;
                }
            }
            KeyEvent::Escape => {
                self.input_len = 0;
            }
            KeyEvent::Enter => {
                let mut cmd_copy = [0u8; MAX_INPUT_LEN];
                let len = self.input_len;
                cmd_copy[..len].copy_from_slice(&self.input[..len]);
                self.input_len = 0;

                let raw = core::str::from_utf8(&cmd_copy[..len]).unwrap_or("");
                let mut echo_line = StackBuf::<MAX_LINE_LEN>::new();
                let _ = write!(echo_line, "wmos@kernel:~$ {}", raw);
                self.push_line(echo_line.as_str(), color::TEXT_PRIMARY);

                let cmd = raw.trim();
                if !cmd.is_empty() {
                    crate::serial_println!("[wmos] cmd: {}", cmd);
                    self.execute_command(cmd, summary);
                }
            }
            KeyEvent::Tab | KeyEvent::Up | KeyEvent::Down | KeyEvent::Left | KeyEvent::Right => {}
        }
    }

    fn execute_command(&mut self, cmd: &str, summary: &BootSummary) {
        let mut buf = StackBuf::<MAX_LINE_LEN>::new();

        if cmd == "help" {
            self.push_line(
                "Commands: help, info, mem, uptime, echo, clear, about, reboot",
                color::TERM_CYAN,
            );
            self.push_line(
                "Tip: drag titlebars to move windows or click taskbar tabs.",
                color::TEXT_MUTED,
            );
        } else if cmd == "info" {
            self.push_line("OS: wmos v1.0.0-mvp | Arch: x86_64 (Rust no_std)", color::TEXT_PRIMARY);
            let _ = write!(
                buf,
                "Limine rev {} | HHDM {:#x} | MemMap {} entries",
                summary.limine_revision, summary.hhdm_offset, summary.memmap_entries
            );
            self.push_line(buf.as_str(), color::TEXT_MUTED);
            buf.clear();
            let _ = write!(
                buf,
                "Display: {}x{} {}bpp (pitch {})",
                summary.fb.width, summary.fb.height, summary.fb.bpp, summary.fb.pitch
            );
            self.push_line(buf.as_str(), color::TERM_CYAN);
        } else if cmd == "mem" {
            let p = pmm::stats();
            let live_vmm = vmm::self_test();
            let free_mib = (p.free_frames as u64 * PAGE_SIZE) / MIB;
            let total_mib = (p.total_frames as u64 * PAGE_SIZE) / MIB;

            let _ = write!(
                buf,
                "PMM: free={} used={} total={} frames",
                p.free_frames, p.used_frames, p.total_frames
            );
            self.push_line(buf.as_str(), color::TERM_GREEN);

            buf.clear();
            let _ = write!(
                buf,
                "RAM: {} MiB free / {} MiB total ({} MiB usable)",
                free_mib,
                total_mib,
                summary.usable_ram_bytes / MIB
            );
            self.push_line(buf.as_str(), color::TEXT_PRIMARY);

            let vmm_col = if live_vmm { color::TERM_GREEN } else { color::DOT_RED };
            let vmm_msg = if live_vmm {
                "VMM self-test: PASS"
            } else {
                "VMM self-test: FAIL"
            };
            self.push_line(vmm_msg, vmm_col);
        } else if cmd == "uptime" {
            let ticks = pit::ticks();
            let secs = pit::uptime_secs();
            let hrs = secs / 3600;
            let mins = (secs % 3600) / 60;
            let rem_s = secs % 60;
            let _ = write!(
                buf,
                "Uptime: {:02}:{:02}:{:02} ({} ticks @ {} Hz)",
                hrs,
                mins,
                rem_s,
                ticks,
                pit::HZ
            );
            self.push_line(buf.as_str(), color::TERM_CYAN);

            buf.clear();
            let _ = write!(
                buf,
                "IRQs: keyboard={}, mouse={}",
                keyboard::irq_count(),
                mouse::irq_count()
            );
            self.push_line(buf.as_str(), color::TEXT_PRIMARY);
        } else if cmd == "echo" {
            self.push_line("", color::TEXT_PRIMARY);
        } else if let Some(rest) = cmd.strip_prefix("echo ") {
            self.push_line(rest, color::TEXT_PRIMARY);
        } else if cmd == "clear" {
            self.head = 0;
            self.count = 0;
        } else if cmd == "about" {
            self.push_line(
                "wmos v1.0.0-mvp - Minimal x86_64 Rust no_std OS",
                color::TERM_CYAN,
            );
            self.push_line(
                "GDT/TSS + IDT/PIC + PMM + VMM + PIT + PS/2 + Framebuffer GUI",
                color::TEXT_MUTED,
            );
        } else if cmd == "reboot" {
            self.push_line("Rebooting system", color::TERM_AMBER);
            crate::serial_println!("[wmos] reboot requested");
            cpu::reboot();
        } else {
            let _ = write!(buf, "Unknown command: '{}'. Type 'help'.", cmd);
            self.push_line(buf.as_str(), color::TERM_AMBER);
        }
    }
}
