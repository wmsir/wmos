# WMOS 变更日志 (CHANGELOG)

记录 WMOS 从初始引导原型到 `v2.0.0` 及后续演进的全部里程碑与代码级变更。

---

## 变更条目格式规范

未来每次代码、脚本或配置变更**必须**按以下模板在本文件顶部（`Unreleased` 或对应版本小节下）追加一条记录：

```markdown
### [版本号或 Unreleased] - YYYY-MM-DD
- **变更类型**：`feat` / `fix` / `docs` / `ci` / `refactor`
- **改动文件**：列出受影响的文件路径
- **变更摘要**：简述实现逻辑或接口/契约变更（含新增串口标记、命令或配置）
- **验证结果**：记录本地编译/静态检查及 GitHub Actions 运行结果
```

---

## [v2.0.0] - 2026-11-27

- **变更类型**：`feat` / `ci` / `docs`
- **改动文件**：
  - 内核与基础设施：`kernel/src/main.rs`、`kernel/src/arch/{mod.rs,rtc.rs}`、`kernel/src/mem/{mod.rs,heap.rs}`、`kernel/src/fs/mod.rs`
  - 图形与交互终端：`kernel/src/shell.rs`、`kernel/src/gfx/{mod.rs,fb.rs,ui.rs,apps.rs,desktop.rs}`
  - 校验与文档：`scripts/verify-qemu.sh`、`docs/DESIGN.md`、`docs/CHANGELOG.md`、`README.md`
- **变更摘要**：
  - **CMOS RTC (`arch/rtc.rs`)**：读取 `0x70`/`0x71` 寄存器（含 UIP 等待、双读一致性、BCD 与 12h/24h 解码），提供 `RtcDateTime` 与任务栏实时日期时间显示。
  - **内核堆 (`mem/heap.rs`)**：在 `HEAP_START = 0xFFFF_A000_0000_0000` 按空闲帧动态映射 `2 MiB .. 16 MiB`，实现带 `IrqGuard` 与 `AllocHeader` 的合并式自由链表 `#[global_allocator]`，启用 `extern crate alloc`。
  - **内存文件系统 (`fs/mod.rs`)**：基于 `BTreeMap<String, Vec<u8>>` 实现 RAMFS（`list`/`read`/`write`/`append`/`touch`/`remove`/`stats`/`self_test`），预置 `readme.txt`、`motd.txt`、`notes.txt`。
  - **双缓冲 GUI 与应用 (`gfx/{fb,ui,apps,desktop}.rs`)**：新增 `Vec<u32>` RAM 后备帧缓存（`<= 3_000_000` 像素）与 `present_rect`/`present_all` MMIO 刷新；新增标题栏三按钮（`Close`/`Minimize`/`Reset`）、Start 菜单、4 窗口管理及 `PaintApp`（360×220 画板）与 `NotepadApp`（读写 `notes.txt`）。
  - **Shell 升级 (`shell.rs`)**：新增 64 条命令历史（`Up`/`Down`）、行内光标移动（`Left`/`Right`）、`Tab` 补全（命令/应用/文件名），扩展至 17 条命令（新增 `date`、`ls`、`cat`、`write`、`append`、`touch`、`rm`、`stat`、`open`）。
  - **CI 校验与文档同步**：`scripts/verify-qemu.sh` 校验 14 项启动标记并在 UEFI 模式注入 `mouse_move 40 25` 及 `ls` / `write demo.txt hello` / `cat demo.txt` / `date` / `open paint` 交互验证；同步更新 `docs/DESIGN.md`、`docs/CHANGELOG.md` 与 `README.md`。
- **验证结果**：
  - 本地 `cargo +stable-x86_64-pc-windows-gnu clean && cargo +stable-x86_64-pc-windows-gnu build --release`（inside `kernel/`）：0 errors, 0 warnings；ELF64 (`310000` B, `e_entry=0xffffffff800074c0`, 3 `PT_LOAD`)、`.sh` 纯 LF 与零禁用标记检查全部通过。
  - 待提交推送后由 GitHub Actions `Build and Verify WMOS` 执行云端 BIOS/UEFI 双模式及交互验证。

---

## [docs-baseline] - 2026-11-27

### `bec16e4` — 新增系统设计文档、变更日志与强制文档同步规范
- **变更类型**：`docs`
- **改动文件**：
  - `docs/DESIGN.md`
  - `docs/CHANGELOG.md`
  - `AGENTS.md`
  - `README.md`
- **变更摘要**：
  - 建立代码核验版 `docs/DESIGN.md`、`docs/CHANGELOG.md` 与 `AGENTS.md` 文档同步硬性规范，并在 `README.md` 顶部补充规范入口与目录树。
- **验证结果**：
  - 本地文档链接与 Mermaid 审计通过；GitHub Actions Run [`45922116722`](https://github.com/wmsir/wmos/actions/runs/45922116722) 全绿通过（`wmos-images` `3170777` B、`wmos-ci-logs-and-preview` `12454` B）。

---

## [v1.0.0-mvp] - 2026-11-27

### `c494b6d` — 修复 PS/2 鼠标初始化与中断状态同步竞争
- **变更类型**：`fix`
- **改动文件**：
  - `kernel/src/drivers/ps2.rs`
  - `kernel/src/drivers/mouse.rs`
  - `kernel/src/drivers/keyboard.rs`
- **变更摘要**：
  - `ps2::init()` 在发送鼠标 `0xF6` / `0xF4` 与 `flush()` 期间保持 IRQ1/IRQ12（`0x03`）屏蔽，待 ACK 清空后再启用 `0x03`；`mouse::on_irq()` 校验 `(inb(0x64) & 0x21) == 0x21` 且 `cycle == 0` 要求 `(byte & 0xC8) == 0x08`；`keyboard::on_irq()` 校验 `(status & 0x01) != 0 && (status & 0x20) == 0`。
- **验证结果**：
  - GitHub Actions Run [`45913901340`](https://github.com/wmsir/wmos/actions/runs/45913901340) 全绿通过（`[wmos] mouse active (756, 385)` 与 `[wmos] cmd: help` 均通过，产出 `wmos-images` `3170785` B）。

### `481a0ac` — 升级 `wmos v1.0.0-mvp` 完整内核、内存管理、PS/2 驱动与交互桌面终端
- **变更类型**：`feat`
- **改动文件**：
  - `kernel/src/arch/{mod.rs,cpu.rs,gdt.rs,idt.rs,pic.rs,pit.rs}`、`kernel/src/mem/{mod.rs,pmm.rs,vmm.rs}`
  - `kernel/src/drivers/{mod.rs,ps2.rs,keyboard.rs,mouse.rs}`、`kernel/src/shell.rs`、`kernel/src/gfx/{mod.rs,fb.rs,ui.rs,desktop.rs}`、`kernel/src/main.rs`
  - `scripts/verify-qemu.sh`、`README.md`
- **变更摘要**：
  - 新增 GDT/TSS（16 KiB IST1）、256 项 IDT、`8259 PIC`、`100 Hz PIT`、PMM 位图、VMM 4 级页表、PS/2 键鼠驱动、`CursorLayer` 防残影与交互终端。
- **验证结果**：
  - CI Run [`45913203157`](https://github.com/wmsir/wmos/actions/runs/45913203157) 暴露 PS/2 鼠标初始化锁存竞争，已在 `c494b6d` 修复。

---

## [v0.1.0 / Step 1] - 2026-11-27

### `af625d7` — 修复 GPT 磁盘镜像 BIOS Boot 分区以支持 `limine bios-install`
- **变更类型**：`fix`
- **改动文件**：
  - `scripts/build-images.sh`
- **变更摘要**：
  - 新增 GPT 分区 1（`2048:4095`，`ef02`）供 Limine 安装 BIOS 阶段，并将 FAT32 ESP（`ef00`）移至分区 2（`@@2M`, `-T 122880`）。
- **验证结果**：
  - GitHub Actions Run [`45862122916`](https://github.com/wmsir/wmos/actions/runs/45862122916) 全绿通过（`wmos-images` `3076037` B）。

### `106f29d` — 初始化 WMOS Step 1 裸机内核、静态桌面与 CI 打包流水线
- **变更类型**：`feat`
- **改动文件**：
  - `.gitattributes`、`.gitignore`、`.github/workflows/build.yml`、`README.md`、`rust-toolchain.toml`、`boot/limine.conf`、`scripts/{build-images.sh,verify-qemu.sh}`、`kernel/`
- **变更摘要**：
  - 建立 Rust `#![no_std]` + Limine v10 引导原型、静态桌面与多格式镜像 CI 流水线。
- **验证结果**：
  - 首次 CI Run [`45861395773`](https://github.com/wmsir/wmos/actions/runs/45861395773) 内核编译与 ISO 生成成功，GPT `ef02` 问题在 `af625d7` 修复。
