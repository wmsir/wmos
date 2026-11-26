# WMOS (x86_64 Rust Micro-OS)

基于 Rust (`#![no_std]`) 与 Limine 引导协议构建的极简 x86_64 微内核操作系统（Step 1：可引导微内核 + 静态桌面渲染 + 全自动 CI 镜像打包与 QEMU 验证）。

---

## 目录结构

```text
.
├── .github/workflows/build.yml   # GitHub Actions 自动构建、打包、QEMU 双模式验证与产物上传
├── .gitignore                    # 忽略构建产物、Limine 缓存及临时镜像
├── README.md                     # 项目说明、GitHub 推送与多平台运行指南
├── rust-toolchain.toml           # 锁定 Rust stable + x86_64-unknown-none 目标
├── boot/
│   └── limine.conf               # Limine v10 引导配置（指向 boot():/boot/wmos-kernel）
├── scripts/
│   ├── build-images.sh           # 生成 ISO / RAW / QCOW2 / VMDK / VHD 与 SHA256SUMS.txt
│   └── verify-qemu.sh            # 无头 QEMU BIOS + UEFI 启动日志断言与桌面截图导出
└── kernel/
    ├── .cargo/config.toml        # x86_64-unknown-none 静态内核编译参数
    ├── Cargo.toml                # wmos-kernel crate 配置（依赖 limine 0.5）
    ├── build.rs                  # 传入 linker-x86_64.ld 链接脚本
    ├── linker-x86_64.ld          # 高半核 (0xffffffff80000000) ELF64 链接脚本
    └── src/
        ├── main.rs               # 内核入口 kmain、Limine 请求、串口日志与死循环停机
        ├── boot_info.rs          # BootSummary / FramebufferInfo 引导信息抽象
        ├── arch/                 # x86_64 底层架构支持（io、cpu、COM1 串口）
        └── gfx/                  # PSF2/内置位图字体、Framebuffer 画布与桌面界面绘制
```

---

## 创建并推送到 GitHub `wmos` 仓库

在本地初始化仓库并推送到你的 GitHub（假设已在 GitHub 创建空仓库 `wmos`）：

```bash
git init -b main
git add .
git commit -m "feat: initial wmos x86_64 Rust micro-OS with Limine boot and CI packaging"
git remote add origin https://github.com/<你的用户名>/wmos.git
git push -u origin main
```

推送后，GitHub Actions 会自动触发 `.github/workflows/build.yml` 中的 `build-and-verify` 任务：
1. 编译 `kernel/target/x86_64-unknown-none/release/wmos-kernel`；
2. 打包生成 5 种磁盘/光盘镜像与校验文件；
3. 在无头 QEMU 中分别执行 **Legacy BIOS** 与 **UEFI (OVMF)** 启动验证并截取开机画面。

---

## 下载 GitHub Actions 构建产物

1. 打开 GitHub 仓库页面，点击顶部 **Actions** 选项卡；
2. 选择最近一次成功运行的 **Build and Verify WMOS** 工作流；
3. 在页面底部的 **Artifacts** 区域下载：
   - **`wmos-images`**：包含以下全部镜像与校验文件：
     - `wmos.iso` — Hybrid BIOS + UEFI 可引导光盘镜像
     - `wmos.img` — 64 MiB GPT + FAT32 原始磁盘镜像（支持 BIOS + UEFI）
     - `wmos.qcow2` — QEMU / KVM 推荐镜像
     - `wmos.vmdk` — VMware 虚拟磁盘镜像
     - `wmos.vhd` — VirtualBox / Hyper-V 固定大小虚拟磁盘镜像
     - `SHA256SUMS.txt` — 所有镜像的 SHA-256 校验值
   - **`wmos-ci-logs-and-preview`**：包含 CI 验证日志与截图：
     - `qemu-bios.log` — QEMU Legacy BIOS 串口输出
     - `qemu-uefi.log` — QEMU UEFI (OVMF) 串口输出
     - `boot-preview.png` — UEFI 模式下的真实开机桌面截图

---

## 本地运行镜像指南

### 1. QEMU（推荐开发与调试使用）

#### 运行 `.iso`（Legacy BIOS 模式）
```bash
qemu-system-x86_64 -M q35 -m 256M -cdrom wmos.iso -boot d -serial stdio
```

#### 运行 `.iso`（UEFI OVMF 模式）
```bash
# Linux（路径可能为 /usr/share/OVMF/OVMF_CODE_4M.fd 或 /usr/share/OVMF/OVMF_CODE.fd）
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
  2. 无需创建新虚拟硬盘（或使用空硬盘），在 **设置 -> 存储** 中将 `wmos.iso` 挂载到光驱；
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

# 打包镜像并运行 QEMU 双模式验证
chmod +x scripts/build-images.sh scripts/verify-qemu.sh
./scripts/build-images.sh
./scripts/verify-qemu.sh
```

---

## 演进路线图（Steps 1–4）

- **Step 1（当前阶段）**：可引导 x86_64 Rust 微内核 + Limine 高半核引导 + 串口日志 + 静态桌面 UI 渲染 + 全自动镜像打包与 QEMU BIOS/UEFI 双模式 CI 验证。
- **Step 2（硬件中断与输入）**：GDT / IDT、8259 PIC / APIC 中断控制器、PIT 定时器与 PS/2 键盘鼠标驱动，实现可交互桌面光标与快捷键。
- **Step 3（内存管理与多任务）**：物理页框分配器（PMM）、4 级页表虚拟内存管理（VMM）、堆分配器（`alloc`）以及抢占式内核线程调度器。
- **Step 4（用户态与系统调用）**：Ring 3 用户态切换、`syscall/sysret` 系统调用接口、基础进程/窗口通信（IPC）与用户态桌面应用雏形。
