# WMOS (`v1.0.0-mvp`) — x86_64 Rust Micro-OS

基于 Rust (`#![no_std]`, 100% Stable) 与 Limine 引导协议构建的独立 x86_64 微内核桌面操作系统，已完成硬件中断、物理/虚拟内存管理、PS/2 键鼠交互、防残影窗口桌面以及无头 QEMU BIOS/UEFI 全自动 CI 打包与交互验证。

GitHub 仓库：`https://github.com/wmsir/wmos`

---

## 文档与维护规范

> **强制同步原则**：任何代码、配置、脚本或 CI 工作流变更，**必须在同一次提交中同步更新文档**，确保文档与实现 100% 一致。

- **[docs/DESIGN.md](docs/DESIGN.md)** — 完整系统架构、高半核链接与 HHDM/PMM/VMM 内存布局、启动时序与串口契约、核心子系统设计、镜像构建及 QEMU 校验体系
- **[docs/CHANGELOG.md](docs/CHANGELOG.md)** — 版本里程碑与每次变更的代码级追踪记录
- **[AGENTS.md](AGENTS.md)** — 强制文档同步规则、Rust Stable `#![no_std]` 内核约束与 `.sh` 纯 LF 行尾规范

---

## 核心特性（`v1.0.0-mvp`）

- **引导与架构初始化**：
  - Limine v10 高半核 ELF64 引导（`0xffffffff80000000`），解析 HHDM、物理内存映射与线性帧缓冲（Framebuffer）。
  - **64 位 GDT / TSS**：配置内核代码/数据段与 16 字节 TSS 描述符，并为 Double Fault（`#DF`，向量 8）分配独立 16 KiB IST 栈。
  - **IDT 与 8259 PIC**：256 项中断描述符表 + 汇编 `global_asm!` 入口桩，重映射主/从 8259 PIC（`0x20` / `0x28`），开启 IRQ0（PIT）、IRQ1（键盘）、IRQ2（级联）与 IRQ12（鼠标）。
- **内存管理**：
  - **PMM（物理页框分配器）**：基于 4 KiB 页帧位图（最大支持 4 GiB）管理可用物理内存，支持 `alloc_frame` / `free_frame` 与实时用量统计。
  - **VMM（4 级页表虚拟内存）**：支持 `map_page` / `unmap_page` / `translate`，开机自动执行写入-校验-释放自检（`[wmos] vmm self-test ok`）。
- **定时器与 PS/2 键鼠驱动**：
  - **PIT 100 Hz 定时器**：提供全局 `ticks()` 与 `uptime_secs()` 计时源。
  - **PS/2 键盘与鼠标**：8042 控制器初始化，支持 Set 1 美式键盘（大小写/Shift/CapsLock/方向键/功能键）与 3 字节 PS/2 鼠标数据包解码。
- **图形桌面与交互终端**：
  - **防残影光标层**：`CursorLayer` 在光标移动或局部重绘前先保存/恢复背景像素，避免光标拖影。
  - **可拖拽窗口与任务栏切换**：支持 `System Info`（实时内存、VMM、运行时间、IRQ 与光标状态）和 `WMOS Terminal` 窗口拖拽、Z 序置顶及底部任务栏标签切换。
  - **交互式 `WMOS Terminal`**：内置命令行解释器，支持以下命令：
    - `help` — 查看内置命令列表
    - `info` — 查看系统版本、架构、Limine 版本、显示分辨率与 HHDM 偏移
    - `mem` — 查看 PMM 物理页框统计并实时运行 VMM 自检
    - `uptime` — 查看 PIT 滴答数、运行时间及键盘/鼠标中断计数
    - `echo <text>` — 回显指定文本
    - `clear` — 清空终端输出缓冲区
    - `about` — 查看 WMOS `v1.0.0-mvp` 简介
    - `reboot` — 通过 PS/2 控制器触发系统重启

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
│   └── verify-qemu.sh            # 无头 QEMU BIOS + UEFI 启动断言、键鼠交互注入与截图导出
└── kernel/
    ├── .cargo/config.toml        # x86_64-unknown-none 静态内核编译参数
    ├── Cargo.toml                # wmos-kernel crate 配置（依赖 limine 0.5）
    ├── build.rs                  # 传入 linker-x86_64.ld 链接脚本
    ├── linker-x86_64.ld          # 高半核 (0xffffffff80000000) ELF64 链接脚本
    └── src/
        ├── main.rs               # 内核入口 kmain、系统初始化与低功耗中断事件循环
        ├── boot_info.rs          # BootSummary / FramebufferInfo 引导信息抽象
        ├── shell.rs              # WMOS Terminal 命令行状态机与内置命令
        ├── arch/                 # x86_64 架构支持（cpu、io、serial、gdt、pic、pit、idt）
        ├── mem/                  # 内存管理（pmm 物理页框分配器、vmm 4 级页表）
        ├── drivers/              # 设备驱动（ps2 控制器、keyboard、mouse）
        └── gfx/                  # 字体、Framebuffer 画布、窗口/防残影光标 UI 与桌面管理器
```

---

## 下载 GitHub Actions 构建产物

每次推送到 `https://github.com/wmsir/wmos` 的 `main` 分支都会自动触发 **Build and Verify WMOS** 工作流，执行编译、镜像打包以及 QEMU Legacy BIOS / UEFI 双模式交互验证：

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
     - `qemu-uefi.log` — QEMU UEFI (OVMF) 串口启动及键鼠交互日志
     - `boot-preview.png` — UEFI 模式下注入鼠标移动与 `help` 命令后的实时桌面截图

---

## 本地运行镜像指南

### 1. QEMU（推荐）

#### 运行 `.iso`（Legacy BIOS 模式）
```bash
qemu-system-x86_64 -M q35 -m 256M -cdrom wmos.iso -boot d -serial stdio
```

#### 运行 `.iso`（UEFI OVMF 模式）
```bash
# Linux（固件路径常见为 /usr/share/OVMF/OVMF_CODE_4M.fd 或 /usr/share/OVMF/OVMF_CODE.fd）
qemu-system-x86_64 -M q35 -m 256M \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
  -cdrom wmos.iso -vga std -serial stdio
```

#### 运行 `.img` / `.qcow2` 磁盘镜像
```bash
# RAW 磁盘镜像
qemu-system-x86_64 -M q35 -m 256M -drive file=wmos.img,format=raw -serial stdio

# QCOW2 磁盘镜像（BIOS 或配合上方 OVMF 参数以 UEFI 启动）
qemu-system-x86_64 -M q35 -m 256M -drive file=wmos.qcow2,format=qcow2 -serial stdio
```

### 2. VirtualBox

- **方式 A（使用 `wmos.iso`）**：
  1. 新建虚拟机，类型选择 `Other`，版本选择 `Other/Unknown (64-bit)`，内存分配 `256 MB` 以上；
  2. 在 **设置 -> 存储** 中将 `wmos.iso` 挂载到光驱；
  3. BIOS 与 EFI 模式均支持（可在 **设置 -> 系统 -> 启用 EFI** 切换）。
- **方式 B（使用 `wmos.vhd`）**：
  1. 新建虚拟机时选择 **使用已有的虚拟硬盘文件**，选中 `wmos.vhd`；
  2. 如需 UEFI 启动，勾选 **设置 -> 系统 -> 启用 EFI (仅限特殊操作系统)**。

### 3. VMware Workstation / Player / Fusion

- **方式 A（使用 `wmos.iso`）**：
  1. 创建新虚拟机（选择“稍后安装操作系统”），客户机类型选 `Other -> Other 64-bit`；
  2. 在虚拟机设置中将 CD/DVD 指向 `wmos.iso` 并启动。
- **方式 B（使用 `wmos.vmdk`）**：
  1. 创建自定义虚拟机，在“选择磁盘”步骤选择 **使用现有虚拟磁盘**，指定 `wmos.vmdk`；
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

# 打包镜像并运行 QEMU 双模式与交互验证
chmod +x scripts/build-images.sh scripts/verify-qemu.sh
./scripts/build-images.sh
./scripts/verify-qemu.sh
```
