# WMOS 变更日志 (CHANGELOG)

记录 WMOS 从初始引导原型到 `v1.0.0-mvp` 及后续演进的全部里程碑与代码级变更。

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

## [docs-baseline] - 2026-11-27

- **变更类型**：`docs`
- **改动文件**：
  - `docs/DESIGN.md`
  - `docs/CHANGELOG.md`
  - `AGENTS.md`
  - `README.md`
- **变更摘要**：
  - 新增代码核验版系统设计文档 [docs/DESIGN.md](DESIGN.md)（覆盖工程约束、分层架构、高半核链接与 HHDM/PMM/VMM 内存布局、启动时序与全部串口契约、GDT/IDT/PIC/PIT/PS2/桌面/Shell 子系统细节、多格式镜像打包与 QEMU 校验流水线、演进路线图）。
  - 新增项目规范文件 [AGENTS.md](../AGENTS.md)，确立“代码/配置/脚本变更必须在同一次提交中同步更新 `docs/DESIGN.md`、`docs/CHANGELOG.md`（及受影响时的 `README.md`）”的强制规则，并固化 Rust Stable `#![no_std]` 与 `.sh` 纯 LF 约束。
  - 新增本变更日志 [docs/CHANGELOG.md](CHANGELOG.md)，并在 [README.md](../README.md) 顶部增加“文档与维护规范”入口及最新目录树。

---

## [v1.0.0-mvp] - 2026-11-27

### `c494b6d` — 修复 PS/2 鼠标初始化与中断状态同步竞争
- **变更类型**：`fix`
- **改动文件**：
  - `kernel/src/drivers/ps2.rs`
  - `kernel/src/drivers/mouse.rs`
  - `kernel/src/drivers/keyboard.rs`
- **变更摘要**：
  - `ps2::init()` 在开启 8042 双端口并发送鼠标 `0xF6` / `0xF4` 命令及 `flush()` 期间保持配置字节 IRQ1/IRQ12（bits `0x03`）屏蔽，待 ACK 全部清空后再启用 `0x03`，彻底消除 `sti()` 后投递陈旧 `IRQ12` 读取残留 `0xFA` 导致 3 字节数据包错位的问题。
  - `mouse::on_irq()` 读取端口 `0x64` 校验 `(status & 0x21) == 0x21`（OBF + AUX_OBF），并在 `cycle == 0` 要求 `(byte & 0xC8) == 0x08` 拒绝 `0xFA` / 溢出字节劫持同步位。
  - `keyboard::on_irq()` 读取端口 `0x64` 校验 `(status & 0x01) != 0 && (status & 0x20) == 0`。
- **验证结果**：
  - 本地 `cargo +stable-x86_64-pc-windows-gnu build --release` 0 错误 0 警告。
  - GitHub Actions Run [`45913901340`](https://github.com/wmsir/wmos/actions/runs/45913901340) 全绿通过（`Build wmos kernel`、`Package bootable images`、`Verify images in QEMU (BIOS and UEFI)` 均成功，`qemu-uefi.log` 输出 `[wmos] mouse active (756, 385)` 与 `[wmos] cmd: help`，产出 `wmos-images` `3170785` B 与 `wmos-ci-logs-and-preview` `12454` B）。

### `481a0ac` — 升级 `wmos v1.0.0-mvp` 完整内核、内存管理、PS/2 驱动与交互桌面终端
- **变更类型**：`feat`
- **改动文件**：
  - `kernel/src/arch/{mod.rs,cpu.rs,gdt.rs,idt.rs,pic.rs,pit.rs}`
  - `kernel/src/mem/{mod.rs,pmm.rs,vmm.rs}`
  - `kernel/src/drivers/{mod.rs,ps2.rs,keyboard.rs,mouse.rs}`
  - `kernel/src/shell.rs`
  - `kernel/src/gfx/{mod.rs,fb.rs,ui.rs,desktop.rs}`
  - `kernel/src/main.rs`
  - `scripts/verify-qemu.sh`
  - `README.md`
- **变更摘要**：
  - 架构层新增 64 位 GDT/TSS（16 KiB IST1 双重错误栈）、256 项 IDT 与 `global_asm!` 15 寄存器保存桩、8259 PIC 重映射（`0x20`/`0x28`）、PIT 100 Hz 计数器、`sti_and_hlt()` 原子休眠与 `reboot()` 双重回退复位。
  - 内存层新增 128 KiB 静态位图 PMM（最高 4 GiB）与 4 级页表 VMM（支持开机读写自检）。
  - 驱动与图形层新增 PS/2 键鼠驱动、`CursorLayer` 12×16 防残影保存/恢复、可拖拽与任务栏切换的 `System Info` + `WMOS Terminal` 双窗口，以及支持 `help`/`info`/`mem`/`uptime`/`echo`/`clear`/`about`/`reboot` 的交互终端。
  - `scripts/verify-qemu.sh` 升级为校验 9 项启动标记、禁止 `PANIC`/`EXCEPTION`，并在 UEFI 模式下自动注入 `mouse_move 40 25` 与 `help` 按键后生成桌面截图。
- **验证结果**：
  - 本地编译与 ELF64/LF 检查通过；CI Run [`45913203157`](https://github.com/wmsir/wmos/actions/runs/45913203157) 中内核编译、镜像打包、BIOS 启动与 `help` 命令均通过，暴露并定位到 PS/2 `mouse_move` 同步问题（已在 `c494b6d` 修复）。

---

## [v0.1.0 / Step 1] - 2026-11-27

### `af625d7` — 修复 GPT 磁盘镜像 BIOS Boot 分区以支持 `limine bios-install`
- **变更类型**：`fix`
- **改动文件**：
  - `scripts/build-images.sh`
- **变更摘要**：
  - 在 `dist/wmos.img` 的 GPT 分区表中新增分区 1（`2048:4095`，类型 `ef02` BIOS Boot Partition）用于 `./limine/limine bios-install dist/wmos.img 1`，并将 FAT32 EFI 系统分区（`ef00`）调整至分区 2（`4096:+60M`，对应 `mformat` / `mcopy` 偏移 `@@2M`、`-T 122880`）。
- **验证结果**：
  - GitHub Actions Run [`45862122916`](https://github.com/wmsir/wmos/actions/runs/45862122916) 全绿通过，成功产出 `wmos-images`（`3076037` B）与 `wmos-ci-logs-and-preview`（`10167` B）。

### `106f29d` — 初始化 WMOS Step 1 裸机内核、静态桌面与 CI 打包流水线
- **变更类型**：`feat`
- **改动文件**：
  - `.gitattributes`、`.gitignore`、`.github/workflows/build.yml`、`README.md`、`rust-toolchain.toml`、`boot/limine.conf`
  - `scripts/{build-images.sh,verify-qemu.sh}`
  - `kernel/{Cargo.toml,Cargo.lock,build.rs,linker-x86_64.ld,.cargo/config.toml}`
  - `kernel/src/{main.rs,boot_info.rs}`, `kernel/src/arch/*`, `kernel/src/gfx/*`
- **变更摘要**：
  - 建立基于 Rust `#![no_std]` 与 Limine v10 的高半核引导原型，实现 COM1 串口日志、8x8 位图字库、Framebuffer 静态桌面 UI、多格式镜像打包以及 GitHub Actions QEMU BIOS/UEFI 双模式自动化校验。
- **验证结果**：
  - 本地编译通过；首次 CI Run [`45861395773`](https://github.com/wmsir/wmos/actions/runs/45861395773) 内核编译与 ISO 生成成功，`wmos.img` GPT 缺少 `ef02` 分区问题在 `af625d7` 修复。
