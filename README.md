# WMOS (`v4.0.0`) — x86_64 Rust Micro-OS

基于 Rust (`#![no_std]`, 100% Stable, `alloc`) 与 Limine 引导协议构建的独立 `x86_64` 微内核桌面操作系统，集成了 PCI 总线枚举、AHCI SATA DMA 读写驱动（`READ_DMA_EXT` / `WRITE_DMA_EXT` / `FLUSH_CACHE_EXT`）、GPT FAT32 `/disk/<8.3>` 磁盘持久化、3 任务协作式调度器、PMM/VMM/内核堆、CMOS RTC、RAMFS、双缓冲多主题桌面（`System Info` / `WMOS Terminal` / `WMOS Paint` / `WMOS Notepad` / `WMOS Files`）、ACPI 关机以及无头 QEMU BIOS/UEFI + 磁盘冷重启持久化全自动 CI 验证。

GitHub 仓库：`https://github.com/wmsir/wmos`

---

## 文档与维护规范

> **强制同步原则**：任何代码、配置、脚本或 CI 工作流变更，**必须在同一次提交中同步更新文档**，确保文档与实现 100% 一致。

- **[docs/DESIGN.md](docs/DESIGN.md)** — 完整系统架构、内存/MMIO 映射、启动时序与串口契约、PCI/AHCI/FAT32 读写/调度器/GUI 子系统设计、镜像构建及 QEMU 校验体系
- **[docs/CHANGELOG.md](docs/CHANGELOG.md)** — 版本里程碑与每次变更的代码级追踪记录
- **[AGENTS.md](AGENTS.md)** — 强制文档同步规则、Rust Stable `#![no_std]` 内核约束与 `.sh` 纯 LF 行尾规范

---

## 核心特性（`v4.0.0`）

- **引导、架构与调度**：
  - Limine v10 高半核 ELF64 引导（`0xffffffff80000000`），解析 HHDM、物理内存映射与线性帧缓冲。
  - **64 位 GDT / TSS / IDT / 8259 PIC / 100 Hz PIT**：IST1 独立 16 KiB `#DF` 栈，汇编 `global_asm!` 15 寄存器中断桩。
  - **CMOS RTC & ACPI 电源管理**：读取 `0x70`/`0x71` 实时时钟；支持 PS/2 `reboot` 与 QEMU/Bochs/VirtualBox ACPI I/O 端口 `shutdown`。
  - **协作式多任务调度器**：基于 `global_asm!` `switch_context` 运行 3 个 16 KiB 栈内核任务（`desktop-ui`、`sys-monitor`、`idle-worker`）。
- **内存管理与存储**：
  - **PMM / VMM / Kernel Heap**：4 KiB 位图 PMM（最高 4 GiB）、4 级页表 VMM（支持 `WRITE_THROUGH` / `CACHE_DISABLE` MMIO 映射）与 `0xFFFF_A000_0000_0000` 合并式全局堆。
  - **PCI + AHCI SATA DMA + GPT FAT32 `/disk/` 持久化**：扫描 PCI 总线 `0..=7`，通过 AHCI `IDENTIFY`、`READ_DMA_EXT`、`WRITE_DMA_EXT` 与 `FLUSH_CACHE_EXT` 读写 SATA 磁盘；支持根目录 `/disk/<8.3>` 文件的创建、覆盖、追加与删除落盘（`/disk/boot` 与 `/disk/EFI` 子目录受写保护），并与可读写 RAMFS（`readme.txt`、`motd.txt`、`notes.txt`）统一访问。
- **双缓冲多主题桌面与 22 命令终端**：
  - **5 窗口与 3 套主题**：支持 `Dark` / `Ocean` / `Amber` 实时切换，集成 `System Info`、`WMOS Terminal`、`WMOS Paint`、`WMOS Notepad`（支持直接保存根目录 `/disk/<8.3>` 文件至 FAT32 磁盘，子目录受保护为 `ReadOnly`）与 `WMOS Files`（浏览 RAMFS 与 FAT32 `/disk/`、打开至 Notepad 及删除可写磁盘文件）。
  - **8 项 Start 菜单与动态任务栏**：支持窗口关闭/最小化/重置、动态防遮挡任务栏标签及右下角 CMOS RTC 时钟与光标托盘。
  - **`WMOS Terminal`（22 条命令 + 历史 + Tab 补全）**：
    - 系统与硬件：`help`、`info`、`mem`、`date`、`uptime`、`lspci`、`ps`、`disks`、`theme <dark|ocean|amber>`、`reboot`、`poweroff`
    - 存储与文件：`ls [/disk]`、`cat <file>`（支持 `/disk/<path>`）、`write <file> <text>`、`append <file> <text>`、`touch <file>`、`rm <file>`、`stat <file>`（支持根目录 `/disk/<8.3>` 持久读写与删除）
    - 应用与工具：`open <info|term|paint|notepad|files>`、`open notepad <file>`、`echo <text>`、`clear`、`about`

---

## 目录结构

```text
.
├── .github/workflows/build.yml   # GitHub Actions 自动构建、打包、QEMU 双模式验证与产物上传
├── .gitattributes                # 强制 *.sh 脚本使用 LF 行尾
├── .gitignore                    # 忽略构建产物、Limine 缓存及临时镜像
├── AGENTS.md                     # 仓库工程约束与文档同步维护规范
├── README.md                     # 项目说明与运行指南
├── rust-toolchain.toml           # 锁定 Rust stable + x86_64-unknown-none 目标
├── boot/
│   └── limine.conf               # Limine v10 引导配置（指向 boot():/boot/wmos-kernel）
├── docs/
│   ├── DESIGN.md                 # 代码核验版系统设计与架构文档
│   └── CHANGELOG.md              # 版本与变更追踪日志
├── scripts/
│   ├── build-images.sh           # 生成 ISO / RAW / QCOW2 / VMDK / VHD 与 SHA256SUMS.txt
│   └── verify-qemu.sh            # 无头 QEMU BIOS + UEFI（含 SATA 持久化与冷重启）断言、命令注入与截图导出
└── kernel/
    ├── .cargo/config.toml        # x86_64-unknown-none 静态内核编译参数
    ├── Cargo.toml                # wmos-kernel crate 配置（依赖 limine 0.5）
    ├── build.rs                  # 传入 linker-x86_64.ld 链接脚本
    ├── linker-x86_64.ld          # 高半核 (0xffffffff80000000) ELF64 链接脚本
    └── src/
        ├── main.rs               # kmain 初始化、调度主循环与 panic_handler
        ├── boot_info.rs          # BootSummary / FramebufferInfo 引导信息抽象
        ├── shell.rs              # WMOS Terminal（22 条命令、历史记录、/disk/ Tab 补全）
        ├── task/mod.rs           # 3 任务协作式调度器与 global_asm! 上下文切换
        ├── fs/                   # 统一文件系统（RAMFS + fat32.rs GPT FAT32 读写与 8.3 持久化）
        ├── arch/                 # x86_64 架构支持（cpu、io、serial、gdt、pic、pit、rtc、idt）
        ├── mem/                  # 内存管理（pmm 位图、vmm 4 级页表、heap 全局分配器）
        ├── drivers/              # 设备驱动（pci、ahci SATA DMA 读写、ps2、keyboard、mouse）
        └── gfx/                  # 字体、主题色板、双缓冲 Framebuffer、Paint/Notepad/Files 与桌面管理
```

---

## 下载 GitHub Actions 构建产物

每次推送到 `https://github.com/wmsir/wmos` 的 `main` 分支都会自动触发 **Build and Verify WMOS** 工作流，执行编译、镜像打包以及 QEMU Legacy BIOS / UEFI（含真实 SATA 盘 FAT32 写入与冷重启持久化）交互验证：

1. 打开仓库页面：https://github.com/wmsir/wmos/actions
2. 选择最近一次绿色的 **Build and Verify WMOS** 运行记录；
3. 在页面底部的 **Artifacts** 区域下载：
   - **`wmos-images`**：包含以下全部镜像与校验文件：
     - `wmos.iso` — Hybrid BIOS + UEFI 可引导光盘镜像
     - `wmos.img` — 64 MiB GPT（`ef02` BIOS Boot + `ef00` FAT32 ESP）原始磁盘镜像
     - `wmos.qcow2` — QEMU / KVM 推荐镜像
     - `wmos.vmdk` — VMware 虚拟磁盘镜像
     - `wmos.vhd` — VirtualBox / Hyper-V 固定大小虚拟磁盘镜像
     - `SHA256SUMS.txt` — 所有镜像的 SHA-256 校验值
   - **`wmos-ci-logs-and-preview`**：包含 CI 验证日志与实时截图：
     - `qemu-bios.log` — QEMU Legacy BIOS 串口启动日志
     - `qemu-uefi.log` — QEMU UEFI (OVMF) + AHCI SATA 串口启动、交互写入 `/disk/persist.txt` 及界面日志
     - `boot-preview.png` — UEFI 模式下自动执行 `lspci`/`ps`/`disks`/`cat /disk/boot/limine/limine.conf`/`write`/`write /disk/persist.txt hello-v4`/`open files` 后的实时桌面截图

---

## 本地运行镜像指南

### 1. QEMU（推荐）

#### 运行 `.iso` + 挂载 SATA FAT32 磁盘（UEFI OVMF 模式，完整体验 `/disk/`）
```bash
# Linux（固件路径常见为 /usr/share/OVMF/OVMF_CODE_4M.fd 或 /usr/share/OVMF/OVMF_CODE.fd）
qemu-system-x86_64 -M q35 -m 256M \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -cdrom wmos.iso -drive file=wmos.img,format=raw -vga std -serial stdio
```

#### 运行 `.iso`（Legacy BIOS 模式）
```bash
qemu-system-x86_64 -M q35 -m 256M -cdrom wmos.iso -boot d -serial stdio
```

#### 运行 `.img` / `.qcow2` 磁盘镜像
```bash
# RAW 磁盘镜像（自带 GPT FAT32 分区，可直接在系统内挂载并持久读写根目录 /disk/<8.3>）
qemu-system-x86_64 -M q35 -m 256M -drive file=wmos.img,format=raw -serial stdio

# QCOW2 磁盘镜像（BIOS 或配合上方 OVMF 参数以 UEFI 启动）
qemu-system-x86_64 -M q35 -m 256M -drive file=wmos.qcow2,format=qcow2 -serial stdio
```

### 2. VirtualBox

- **方式 A（使用 `wmos.iso`）**：
  1. 新建虚拟机，类型选择 `Other`，版本选择 `Other/Unknown (64-bit)`，内存分配 `256 MB` 以上；
  2. 在 **设置 -> 存储** 中将 `wmos.iso` 挂载到光驱；若同时挂载 `wmos.vhd` 到 SATA 控制器，可在系统内通过 `WMOS Files` 或 `/disk/` 浏览磁盘文件；
  3. BIOS 与 EFI 模式均支持（可在 **设置 -> 系统 -> 启用 EFI** 切换）。
- **方式 B（使用 `wmos.vhd`）**：
  1. 新建虚拟机时选择 **使用已有的虚拟硬盘文件**，选中 `wmos.vhd`（推荐挂载在 AHCI SATA 控制器下）；
  2. 如需 UEFI 启动，勾选 **设置 -> 系统 -> 启用 EFI (仅限特殊操作系统)**。

### 3. VMware Workstation / Player / Fusion

- **方式 A（使用 `wmos.iso`）**：
  1. 创建新虚拟机（选择“稍后安装操作系统”），客户机类型选 `Other -> Other 64-bit`；
  2. 在虚拟机设置中将 CD/DVD 指向 `wmos.iso` 并启动。
- **方式 B（使用 `wmos.vmdk`）**：
  1. 创建自定义虚拟机，在“选择磁盘”步骤选择 **使用现有虚拟磁盘**（建议选 SATA 控制器），指定 `wmos.vmdk`；
  2. 在 **虚拟机设置 -> 选项 -> 高级** 中可自由选择 **BIOS** 或 **UEFI** 固件类型。

---

## 本地构建与验证（Linux / WSL2）

```bash
# 安装依赖（Ubuntu / Debian）
sudo apt-get update && sudo apt-get install -y \
  xorriso mtools gdisk qemu-utils qemu-system-x86 ovmf build-essential python3

# 添加裸机编译目标并编译内核
rustup target add x86_64-unknown-none
(cd kernel && cargo build --release)

# 打包镜像并运行 QEMU 双模式与 SATA 磁盘交互验证
chmod +x scripts/build-images.sh scripts/verify-qemu.sh
./scripts/build-images.sh
./scripts/verify-qemu.sh
```
