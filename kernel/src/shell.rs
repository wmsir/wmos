use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::arch::{cpu, pit, rtc};
use crate::boot_info::BootSummary;
use crate::drivers::keyboard::{self, KeyEvent};
use crate::drivers::{ahci, mouse, pci};
use crate::fs;
use crate::gfx::color::{self, Color, Theme};
use crate::mem::{heap, pmm, vmm};
use crate::task::{self, TaskState};

const MAX_LINES: usize = 64;
const MAX_HISTORY: usize = 64;
const MAX_INPUT_LEN: usize = 96;
const MIB: u64 = 1024 * 1024;
const PAGE_SIZE: u64 = 4096;

const COMMANDS: [&str; 22] = [
    "help", "info", "mem", "date", "uptime", "ls", "cat", "write", "append", "touch", "rm",
    "stat", "open", "lspci", "ps", "disks", "theme", "echo", "clear", "about", "reboot",
    "poweroff",
];

const APP_NAMES: [&str; 5] = ["info", "term", "paint", "notepad", "files"];
const THEME_NAMES: [&str; 3] = ["dark", "ocean", "amber"];

#[derive(Clone, PartialEq, Eq)]
pub enum ShellAction {
    None,
    OpenApp(usize),
    OpenInNotepad(String),
    SetTheme(Theme),
}

struct TermLine {
    text: String,
    color: Color,
}

pub struct TerminalState {
    lines: Vec<TermLine>,
    history: Vec<String>,
    history_idx: Option<usize>,
    draft: String,
    input: String,
    cursor_pos: usize,
}

impl TerminalState {
    pub fn new(summary: &BootSummary, vmm_ok: bool) -> Self {
        let mut term = Self {
            lines: Vec::with_capacity(MAX_LINES),
            history: Vec::with_capacity(16),
            history_idx: None,
            draft: String::new(),
            input: String::new(),
            cursor_pos: 0,
        };

        term.push_line("[wmos] boot ok", color::TERM_GREEN);
        if vmm_ok {
            term.push_line(
                "[wmos] gdt/idt/pmm/vmm/heap/pci/ahci/sched ok",
                color::TERM_GREEN,
            );
        } else {
            term.push_line("[wmos] vmm self-test FAILED", color::DOT_RED);
        }
        term.push_line(
            &format!(
                "[wmos] limine rev={} hhdm={:#x}",
                summary.limine_revision, summary.hhdm_offset
            ),
            color::TEXT_PRIMARY,
        );
        term.push_line(
            &format!(
                "[wmos] fb={}x{} bpp={} pitch={}",
                summary.fb.width, summary.fb.height, summary.fb.bpp, summary.fb.pitch
            ),
            color::TERM_CYAN,
        );
        if fs::is_disk_mounted() {
            term.push_line(
                &format!("[wmos] /disk/ FAT32 mounted ({} files)", fs::list_disk().len()),
                color::TERM_CYAN,
            );
        }
        term.push_line("Type 'help' for commands.", color::TERM_AMBER);
        term
    }

    pub fn push_line(&mut self, text: &str, col: Color) {
        for raw_line in text.split('\n') {
            if self.lines.len() >= MAX_LINES {
                self.lines.remove(0);
            }
            self.lines.push(TermLine {
                text: String::from(raw_line),
                color: col,
            });
        }
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, idx: usize) -> Option<(&str, Color)> {
        self.lines
            .get(idx)
            .map(|l| (l.text.as_str(), l.color))
    }

    pub fn input_str(&self) -> &str {
        self.input.as_str()
    }

    pub fn cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    pub fn handle_key(
        &mut self,
        ev: KeyEvent,
        summary: &BootSummary,
        _vmm_ok: bool,
    ) -> ShellAction {
        match ev {
            KeyEvent::Char(b) => {
                if (0x20..=0x7E).contains(&b) && self.input.len() < MAX_INPUT_LEN {
                    self.input.insert(self.cursor_pos, b as char);
                    self.cursor_pos += 1;
                    self.history_idx = None;
                }
                ShellAction::None
            }
            KeyEvent::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input.remove(self.cursor_pos);
                    self.history_idx = None;
                }
                ShellAction::None
            }
            KeyEvent::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
                ShellAction::None
            }
            KeyEvent::Right => {
                if self.cursor_pos < self.input.len() {
                    self.cursor_pos += 1;
                }
                ShellAction::None
            }
            KeyEvent::Escape => {
                self.input.clear();
                self.cursor_pos = 0;
                self.history_idx = None;
                ShellAction::None
            }
            KeyEvent::Up => {
                if !self.history.is_empty() {
                    let next_idx = match self.history_idx {
                        None => {
                            self.draft = self.input.clone();
                            self.history.len() - 1
                        }
                        Some(idx) => idx.saturating_sub(1),
                    };
                    self.history_idx = Some(next_idx);
                    self.input = self.history[next_idx].clone();
                    self.cursor_pos = self.input.len();
                }
                ShellAction::None
            }
            KeyEvent::Down => {
                if let Some(idx) = self.history_idx {
                    if idx + 1 < self.history.len() {
                        let next_idx = idx + 1;
                        self.history_idx = Some(next_idx);
                        self.input = self.history[next_idx].clone();
                    } else {
                        self.history_idx = None;
                        self.input = core::mem::take(&mut self.draft);
                    }
                    self.cursor_pos = self.input.len();
                }
                ShellAction::None
            }
            KeyEvent::Tab => {
                self.autocomplete();
                ShellAction::None
            }
            KeyEvent::Enter => {
                let raw = core::mem::take(&mut self.input);
                self.cursor_pos = 0;
                self.history_idx = None;
                self.draft.clear();

                self.push_line(&format!("wmos@kernel:~$ {}", raw), color::TEXT_PRIMARY);
                let cmd = raw.trim();
                if cmd.is_empty() {
                    return ShellAction::None;
                }

                if self.history.last().map(|s| s.as_str()) != Some(cmd) {
                    if self.history.len() >= MAX_HISTORY {
                        self.history.remove(0);
                    }
                    self.history.push(String::from(cmd));
                }

                crate::serial_println!("[wmos] cmd: {}", cmd);
                self.execute_command(cmd, summary)
            }
        }
    }

    fn autocomplete(&mut self) {
        let trimmed = self.input.trim_start();
        if let Some((first, rest)) = trimmed.split_once(' ') {
            if !rest.contains(' ') {
                let prefix = rest;
                let has_leading_slash = prefix.starts_with('/');
                let clean_prefix = prefix.trim_start_matches('/');
                let mut candidates: Vec<String> = Vec::new();
                if first == "open" {
                    for &app in &APP_NAMES {
                        if app.starts_with(prefix) {
                            candidates.push(String::from(app));
                        }
                    }
                } else if first == "theme" {
                    for &th in &THEME_NAMES {
                        if th.starts_with(prefix) {
                            candidates.push(String::from(th));
                        }
                    }
                } else {
                    for entry in fs::list() {
                        if entry.name.starts_with(clean_prefix) {
                            candidates.push(entry.name);
                        }
                    }
                    for entry in fs::list_disk() {
                        if entry.name.starts_with(clean_prefix) {
                            if has_leading_slash {
                                candidates.push(format!("/{}", entry.name));
                            } else {
                                candidates.push(entry.name);
                            }
                        }
                    }
                }
                if let Some(comp) = common_prefix(&candidates) {
                    if comp.len() > prefix.len() {
                        self.input = format!("{} {}", first, comp);
                        if candidates.len() == 1 {
                            self.input.push(' ');
                        }
                        self.cursor_pos = self.input.len();
                    }
                }
            }
        } else {
            let mut candidates: Vec<String> = Vec::new();
            for &cmd in &COMMANDS {
                if cmd.starts_with(trimmed) {
                    candidates.push(String::from(cmd));
                }
            }
            if let Some(comp) = common_prefix(&candidates) {
                if comp.len() >= trimmed.len() {
                    self.input = comp;
                    if candidates.len() == 1 {
                        self.input.push(' ');
                    }
                    self.cursor_pos = self.input.len();
                }
            }
        }
    }

    fn execute_command(&mut self, cmd: &str, summary: &BootSummary) -> ShellAction {
        if cmd == "help" {
            self.push_line(
                "Sys : help, info, mem, date, uptime, lspci, ps, disks, theme, reboot, poweroff",
                color::TERM_CYAN,
            );
            self.push_line(
                "FS  : ls [/disk], cat <f>, write <f> <t>, append <f> <t>, touch/rm/stat <f>",
                color::TERM_GREEN,
            );
            self.push_line(
                "Apps: open <info|term|paint|notepad|files>, echo, clear, about",
                color::TEXT_MUTED,
            );
        } else if cmd == "info" {
            self.push_line(
                "OS: wmos v3.0.0 | Arch: x86_64 (Rust no_std)",
                color::TEXT_PRIMARY,
            );
            self.push_line(
                &format!(
                    "Limine rev {} | HHDM {:#x} | MemMap {} entries",
                    summary.limine_revision, summary.hhdm_offset, summary.memmap_entries
                ),
                color::TEXT_MUTED,
            );
            self.push_line(
                &format!(
                    "Display: {}x{} {}bpp (pitch {}) | Double-buffer: ON",
                    summary.fb.width, summary.fb.height, summary.fb.bpp, summary.fb.pitch
                ),
                color::TERM_CYAN,
            );
        } else if cmd == "mem" {
            let p = pmm::stats();
            let h = heap::stats();
            let live_vmm = vmm::self_test();
            let free_mib = (p.free_frames as u64 * PAGE_SIZE) / MIB;
            let total_mib = (p.total_frames as u64 * PAGE_SIZE) / MIB;

            self.push_line(
                &format!(
                    "PMM : free={} used={} total={} frames ({}/{} MiB)",
                    p.free_frames, p.used_frames, p.total_frames, free_mib, total_mib
                ),
                color::TERM_GREEN,
            );
            self.push_line(
                &format!(
                    "Heap: used={} KiB free={} KiB total={} KiB (allocs={})",
                    h.used_bytes / 1024,
                    h.free_bytes / 1024,
                    h.total_bytes / 1024,
                    h.alloc_count
                ),
                color::TERM_CYAN,
            );
            let (vmm_msg, vmm_col) = if live_vmm {
                ("VMM : self-test PASS", color::TERM_GREEN)
            } else {
                ("VMM : self-test FAIL", color::DOT_RED)
            };
            self.push_line(vmm_msg, vmm_col);
        } else if cmd == "date" {
            let dt = rtc::read();
            self.push_line(
                &format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02} (UTC/Hardware)",
                    dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
                ),
                color::TERM_CYAN,
            );
        } else if cmd == "uptime" {
            let ticks = pit::ticks();
            let secs = pit::uptime_secs();
            self.push_line(
                &format!(
                    "Uptime: {:02}:{:02}:{:02} ({} ticks @ {} Hz)",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60,
                    ticks,
                    pit::HZ
                ),
                color::TERM_CYAN,
            );
            self.push_line(
                &format!(
                    "IRQs: keyboard={}, mouse={}",
                    keyboard::irq_count(),
                    mouse::irq_count()
                ),
                color::TEXT_PRIMARY,
            );
        } else if cmd == "lspci" {
            let devs = pci::devices();
            if devs.is_empty() {
                self.push_line("No PCI devices detected", color::TEXT_MUTED);
            } else {
                for d in &devs {
                    self.push_line(
                        &format!(
                            "{:02x}:{:02x}.{} {:04x}:{:04x} [{}] {}",
                            d.bus,
                            d.device,
                            d.function,
                            d.vendor_id,
                            d.device_id,
                            pci::vendor_name(d.vendor_id),
                            pci::class_name(d.class_code, d.subclass)
                        ),
                        color::TEXT_PRIMARY,
                    );
                }
                self.push_line(&format!("{} PCI device(s)", devs.len()), color::TERM_CYAN);
            }
        } else if cmd == "ps" {
            let tasks = task::list();
            self.push_line("ID | STATE   | SWITCHES | TICKS    | NAME", color::TERM_CYAN);
            for t in &tasks {
                let st = match t.state {
                    TaskState::Running => "Running",
                    TaskState::Ready => "Ready  ",
                };
                self.push_line(
                    &format!(
                        "{:<2} | {} | {:<8} | {:<8} | {}",
                        t.id, st, t.switches, t.ticks, t.name
                    ),
                    color::TEXT_PRIMARY,
                );
            }
            self.push_line(
                &format!("Total context switches: {}", task::total_switches()),
                color::TERM_GREEN,
            );
        } else if cmd == "disks" {
            match ahci::disk_info() {
                Some(d) => {
                    let kib = (d.sectors.saturating_mul(512)) / 1024;
                    self.push_line(
                        &format!(
                            "AHCI Port {}: {} ({} sectors, {} KiB)",
                            d.port,
                            d.model_str(),
                            d.sectors,
                            kib
                        ),
                        color::TERM_GREEN,
                    );
                }
                None => {
                    self.push_line("AHCI Disk: none detected", color::TEXT_MUTED);
                }
            }
            if fs::is_disk_mounted() {
                self.push_line(
                    &format!("FAT32 /disk/: mounted ({} files)", fs::list_disk().len()),
                    color::TERM_CYAN,
                );
            } else {
                self.push_line("FAT32 /disk/: not mounted", color::TERM_AMBER);
            }
        } else if cmd == "ls" || cmd == "ls /disk" || cmd == "ls disk" || cmd == "ls /disk/" {
            if cmd != "ls" {
                let disk_entries = fs::list_disk();
                if disk_entries.is_empty() {
                    self.push_line("/disk/ not mounted or empty", color::TEXT_MUTED);
                } else {
                    for entry in &disk_entries {
                        self.push_line(
                            &format!("  {:32} {} B", entry.name, entry.size),
                            color::TEXT_PRIMARY,
                        );
                    }
                    self.push_line(
                        &format!("{} file(s) on FAT32 /disk/", disk_entries.len()),
                        color::TERM_CYAN,
                    );
                }
            } else {
                let entries = fs::list();
                let st = fs::stats();
                for entry in entries {
                    self.push_line(
                        &format!("  {:18} {} B", entry.name, entry.size),
                        color::TEXT_PRIMARY,
                    );
                }
                if fs::is_disk_mounted() {
                    self.push_line(
                        &format!(
                            "RAMFS: {} files ({} B) | FAT32 /disk/: {} files ('ls /disk')",
                            st.file_count,
                            st.total_bytes,
                            fs::list_disk().len()
                        ),
                        color::TERM_CYAN,
                    );
                } else {
                    self.push_line(
                        &format!("{} files, {} bytes total", st.file_count, st.total_bytes),
                        color::TERM_CYAN,
                    );
                }
            }
        } else if let Some(arg) = cmd.strip_prefix("cat ") {
            let raw_name = arg.trim();
            let norm = raw_name.trim_start_matches('/');
            if let Some(content) = fs::read_to_string(raw_name) {
                crate::serial_println!("[wmos] fs cat {} ok", norm);
                if content.is_empty() {
                    self.push_line("(empty file)", color::TEXT_MUTED);
                } else {
                    for line in content.lines() {
                        self.push_line(line, color::TEXT_PRIMARY);
                    }
                }
            } else {
                self.push_line(&format!("File not found: {}", raw_name), color::DOT_RED);
            }
        } else if let Some(rest) = cmd.strip_prefix("write ") {
            if let Some((raw_name, text)) = rest.trim_start().split_once(' ') {
                let name = raw_name.trim();
                let mut payload = String::from(text);
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                match fs::write(name, payload.as_bytes()) {
                    Ok(n) => self.push_line(&format!("Wrote {} B to {}", n, name), color::TERM_GREEN),
                    Err(e) => self.push_line(e, color::DOT_RED),
                }
            } else {
                self.push_line("Usage: write <file> <text>", color::TERM_AMBER);
            }
        } else if let Some(rest) = cmd.strip_prefix("append ") {
            if let Some((raw_name, text)) = rest.trim_start().split_once(' ') {
                let name = raw_name.trim();
                let mut payload = String::from(text);
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                match fs::append(name, payload.as_bytes()) {
                    Ok(n) => {
                        self.push_line(&format!("Appended {} B to {}", n, name), color::TERM_GREEN)
                    }
                    Err(e) => self.push_line(e, color::DOT_RED),
                }
            } else {
                self.push_line("Usage: append <file> <text>", color::TERM_AMBER);
            }
        } else if let Some(arg) = cmd.strip_prefix("touch ") {
            let name = arg.trim();
            match fs::touch(name) {
                Ok(()) => self.push_line(&format!("Touched {}", name), color::TERM_GREEN),
                Err(e) => self.push_line(e, color::DOT_RED),
            }
        } else if let Some(arg) = cmd.strip_prefix("rm ") {
            let name = arg.trim();
            match fs::remove(name) {
                Ok(()) => self.push_line(&format!("Removed {}", name), color::TERM_GREEN),
                Err(e) => self.push_line(e, color::DOT_RED),
            }
        } else if let Some(arg) = cmd.strip_prefix("stat ") {
            let name = arg.trim();
            if let Some(data) = fs::read(name) {
                self.push_line(&format!("{}: {} bytes", name, data.len()), color::TERM_CYAN);
            } else {
                self.push_line(&format!("{}: not found", name), color::DOT_RED);
            }
        } else if let Some(arg) = cmd.strip_prefix("open ") {
            let target = arg.trim();
            if let Some(file_arg) = target.strip_prefix("notepad ") {
                let f = file_arg.trim();
                if !f.is_empty() {
                    self.push_line(&format!("Opening {} in Notepad", f), color::TERM_GREEN);
                    return ShellAction::OpenInNotepad(String::from(f));
                }
            }
            let idx = match target {
                "info" | "sys" => Some(0),
                "term" | "terminal" => Some(1),
                "paint" => Some(2),
                "notepad" | "note" => Some(3),
                "files" | "explorer" => Some(4),
                _ => None,
            };
            if let Some(app_idx) = idx {
                self.push_line(&format!("Opening {}", target), color::TERM_GREEN);
                return ShellAction::OpenApp(app_idx);
            } else {
                self.push_line(
                    "Unknown app. Use: open <info|term|paint|notepad|files>",
                    color::TERM_AMBER,
                );
            }
        } else if cmd == "theme" {
            self.push_line("Usage: theme <dark|ocean|amber>", color::TERM_AMBER);
        } else if let Some(arg) = cmd.strip_prefix("theme ") {
            let t = arg.trim();
            let chosen = match t {
                "dark" => Some(Theme::Dark),
                "ocean" => Some(Theme::Ocean),
                "amber" => Some(Theme::Amber),
                _ => None,
            };
            if let Some(theme) = chosen {
                self.push_line(&format!("Theme set to {}", theme.name()), color::TERM_GREEN);
                return ShellAction::SetTheme(theme);
            } else {
                self.push_line("Unknown theme. Use: theme <dark|ocean|amber>", color::TERM_AMBER);
            }
        } else if cmd == "echo" {
            self.push_line("", color::TEXT_PRIMARY);
        } else if let Some(rest) = cmd.strip_prefix("echo ") {
            self.push_line(rest, color::TEXT_PRIMARY);
        } else if cmd == "clear" {
            self.lines.clear();
        } else if cmd == "about" {
            self.push_line(
                "wmos v3.0.0 - Minimal x86_64 Rust no_std OS",
                color::TERM_CYAN,
            );
            self.push_line(
                "GDT/IDT/PMM/VMM/Heap + PCI/AHCI/FAT32 + Scheduler + Double-Buf GUI",
                color::TEXT_MUTED,
            );
        } else if cmd == "reboot" {
            self.push_line("Rebooting system", color::TERM_AMBER);
            crate::serial_println!("[wmos] reboot requested");
            cpu::reboot();
        } else if cmd == "poweroff" {
            self.push_line("Shutting down system", color::TERM_AMBER);
            crate::serial_println!("[wmos] poweroff requested");
            cpu::shutdown();
        } else {
            self.push_line(
                &format!("Unknown command: '{}'. Type 'help'.", cmd),
                color::TERM_AMBER,
            );
        }

        ShellAction::None
    }
}

fn common_prefix(items: &[String]) -> Option<String> {
    let first = items.first()?;
    let mut prefix = first.as_str();
    for item in &items[1..] {
        let mut end = 0usize;
        for (a, b) in prefix.bytes().zip(item.bytes()) {
            if a == b {
                end += 1;
            } else {
                break;
            }
        }
        prefix = &prefix[..end];
    }
    Some(String::from(prefix))
}
