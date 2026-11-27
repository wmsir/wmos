# WMOS 变更日志 (CHANGELOG)

记录 WMOS 从初始引导原型到 `v4.1.0` 及后续演进的全部里程碑与代码级变更。

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

## [v4.1.0] - 2026-11-27

- **变更类型**：`feat` / `ci` / `docs`
- **改动文件**：
  - PCI 与 Intel e1000 驱动：`kernel/src/drivers/pci.rs`、`kernel/src/drivers/mod.rs`、`kernel/src/drivers/e1000.rs`
  - 网络协议栈与主循环：`kernel/src/net/mod.rs`、`kernel/src/main.rs`
  - 终端网络命令与桌面监控：`kernel/src/shell.rs`、`kernel/src/gfx/desktop.rs`
  - CI 网络验证与文档：`scripts/verify-qemu.sh`、`docs/DESIGN.md`、`docs/CHANGELOG.md`、`README.md`
- **变更摘要**：
  - **PCI `bar0` 扩展 (`drivers/pci.rs`)**：`PciDevice` 新增 `pub bar0: u32`（读取配置空间偏移 `0x10`）。
  - **Intel e1000/e1000e DMA 驱动 (`drivers/e1000.rs`)**：识别 Intel `0x8086`（`0x100E`/`0x10D3`/`0x100F`/`0x1004`），开启 Memory Space + Bus Master (`0x04 |= 0x06`)，将 `bar0 & !0xFFF` 共 `32` 页（`128 KiB`）以非缓存属性映射至 `E1000_MMIO_VIRT (0xFFFF_B001_0000_0000)`；屏蔽中断、置位 `SLU`、读取 `RAL0`/`RAH0` 或 `EERD` EEPROM MAC 地址，并基于 PMM 物理帧建立 `16` 槽 `RxDesc` / `TxDesc` 环形缓冲区与 `2048 B` 收发缓冲，提供 `init`、`info`、`send_frame`、`poll_recv` 与 `stats`。
  - **IPv4/ARP/ICMP/UDP 协议栈 (`net/mod.rs` & `main.rs`)**：默认配置静态 QEMU SLIRP 地址 `10.0.2.15/24`（网关 `10.0.2.2`）；实现 RFC 1071 校验和、`ARP_CACHE_SIZE = 16` 缓存及解析重试、`ICMP_ECHO_ID = 0x574D` Echo 请求/回复（含自动响应入站 ARP/ICMP Echo）以及最大 `1472 B` 载荷 `send_udp`；`kmain` 输出 `[wmos] e1000 ok mac=<mac>`、`[wmos] net ok ip=<ip> gw=<gw>`（无网卡时输出 `[wmos] e1000 skipped`）并在事件循环每轮执行 `net::poll()`。
  - **Shell 网络命令与 `System Info` 集成 (`shell.rs` & `gfx/desktop.rs`)**：终端扩展至 `25` 条命令，新增 `ifconfig`、`ping [ip]`（发送 3 次 ICMP 探测并输出 `[wmos] ping <ip> ok rtt=<ms>ms`）与 `udp <ip> <port> <msg>`（输出 `[wmos] udp sent <ip>:<port>`）；桌面顶栏与 `System Info` 升级至 `wmos v4.1.0` 并实时展示 `Net` IPv4 与 TX/RX 包统计。
  - **CI e1000 + `ping` 闭环验证 (`scripts/verify-qemu.sh`)**：UEFI Boot #1 挂载 `-netdev user,id=net0 -device e1000,netdev=net0`，注入 `ifconfig` 与 `ping 10.0.2.2` 并严格断言 `[wmos] e1000 ok mac=`、`[wmos] net ok ip=10.0.2.15`、`[wmos] cmd: ifconfig`、`[wmos] cmd: ping 10.0.2.2` 及 `[wmos] ping 10.0.2.2 ok`，完整保留 BIOS、FAT32 写入、`mtype` 与 Boot #2 冷重启检查。
- **验证结果**：
  - 本地 `cargo +stable-x86_64-pc-windows-gnu clean && cargo +stable-x86_64-pc-windows-gnu build --release`（inside `kernel/`）：0 errors, 0 warnings（ELF64 `392184` B，`e_entry=0xffffffff80024ab0`），`scripts/*.sh` 纯 LF 与零禁用标记检查通过。
  - 待提交推送后由 GitHub Actions `Build and Verify WMOS` 执行云端 BIOS/UEFI + SATA 持久化 + e1000 网络交互验证。

---

## [v4.0.0] - 2026-11-27

### `2c01a9d` — AHCI DMA 写入、FAT32 根目录 `/disk/<8.3>` 持久化与冷重启 CI 校验

- **变更类型**：`feat` / `ci` / `docs`
- **改动文件**：
  - AHCI 写盘与 FAT32 持久化：`kernel/src/drivers/ahci.rs`、`kernel/src/fs/fat32.rs`、`kernel/src/fs/mod.rs`
  - 图形应用、桌面与终端：`kernel/src/gfx/apps.rs`、`kernel/src/gfx/desktop.rs`、`kernel/src/shell.rs`
  - 持久化校验与文档：`scripts/verify-qemu.sh`、`docs/DESIGN.md`、`docs/CHANGELOG.md`、`README.md`
- **变更摘要**：
  - **AHCI DMA 写扇区与缓存刷盘 (`drivers/ahci.rs`)**：`issue_dma_cmd` 支持写方向位 `flags = 5 | (1 << 6)` 与零 PRDT 刷盘命令；新增 `write_sectors(lba, count, buf)`（最大 8 扇区 / 4 KiB 分片经 `(dma_phys + hhdm)` 发送 `ATA_CMD_WRITE_DMA_EXT (0x35)`，末尾下发 `ATA_CMD_FLUSH_CACHE_EXT (0xEA)` 确保落盘）。
  - **FAT32 根目录 8.3 读写与簇链管理 (`fs/fat32.rs` & `fs/mod.rs`)**：`Fat32Volume` 记录 `num_fats`、`sectors_per_fat`、`root_cluster`、`total_clusters`；新增 `encode_short_name`、`set_fat_entry`（多 FAT 镜像同步）、`alloc_chain`、`free_chain`、`scan_root_dir`、`write_root_file`（支持空槽复用与目录簇扩容）与 `delete_root_file`（标记 `0xE5` 并释放簇链）。`split_disk_root` 限定仅根目录 `/disk/<8.3>` 可写可删，子目录（`/disk/boot`、`/disk/EFI`）继续受保护；读取增加大小写不敏感匹配兜底；新增串口标记 `[wmos] fs disk write <norm> ok (<B> B)` 与 `[wmos] fs disk rm <norm> ok`。
  - **GUI 与 Shell 可写 `/disk/` 集成 (`gfx/{apps,desktop}.rs` & `shell.rs`)**：`NotepadApp` 通过 `is_disk_readonly_path` 放行根目录 `/disk/<8.3>` 保存且在写入失败时显示 `Error`；`FilesApp` 支持删除根目录 `/disk/<8.3>` 磁盘文件并显示 `FAT32 RW` / `Protected` / `RAMFS` 状态；桌面顶栏、`System Info` 与终端 `help`/`info`/`about` 升级至 `wmos v4.0.0`，支持 `write`/`append`/`touch`/`rm` 操作 `/disk/<8.3>`。
  - **CI 持久化与冷重启闭环验证 (`scripts/verify-qemu.sh`)**：UEFI Boot #1 挂载副本镜像 `dist/wmos-persist-test.img` 注入 `write /disk/persist.txt hello-v4`；宿主 `mtype -i dist/wmos-persist-test.img@@2M ::/PERSIST.TXT` 验证 `hello-v4` 落盘；UEFI Boot #2 冷重启挂载同一镜像注入 `cat /disk/persist.txt` 并断言 `[wmos] fs cat disk/persist.txt ok`。
- **验证结果**：
  - GitHub Actions Run [`45989950613`](https://github.com/wmsir/wmos/actions/runs/45989950613) 全绿通过（BIOS + UEFI Boot #1 写入 `/disk/persist.txt` + `mtype ::/PERSIST.TXT => hello-v4` + UEFI Boot #2 冷重启读取 `[wmos] fs cat disk/persist.txt ok` 全部通过，产出 `wmos-images` `3517455` B 与 `wmos-ci-logs-and-preview` `17962` B）。

---

## [v3.0.0] - 2026-11-27

### `c592cb9` — PCI 枚举、AHCI SATA DMA、FAT32 `/disk/` 挂载、3 任务调度与多主题桌面

- **变更类型**：`feat` / `ci` / `docs`
- **改动文件**：
  - 内核、PCI/AHCI 与调度：`kernel/src/main.rs`、`kernel/src/arch/{io.rs,cpu.rs}`、`kernel/src/mem/vmm.rs`、`kernel/src/drivers/{mod.rs,pci.rs,ahci.rs}`、`kernel/src/task/mod.rs`
  - FAT32 与存储：`kernel/src/fs/{mod.rs,fat32.rs}`
  - 主题、文件管理器与终端：`kernel/src/gfx/{color.rs,apps.rs,desktop.rs}`、`kernel/src/shell.rs`
  - 校验与文档：`scripts/verify-qemu.sh`、`docs/DESIGN.md`、`docs/CHANGELOG.md`、`README.md`
- **变更摘要**：
  - **PCI 枚举 (`drivers/pci.rs`)**：基于 `0xCF8`/`0xCFC` 扫描总线 `0..=7`、设备 `0..32` 与多功能设备，输出 `[wmos] pci ok devices=<n>`。
  - **AHCI SATA DMA (`drivers/ahci.rs` & `mem/vmm.rs`)**：VMM 新增 `WRITE_THROUGH` / `CACHE_DISABLE` 与 `hhdm_offset()`；映射 HBA BAR5 至 `0xFFFF_B000_0000_0000`，使用 PMM 物理 DMA 中转帧执行 `IDENTIFY (0xEC)` 与 `READ_DMA_EXT (0x25)`，输出 `[wmos] ahci ok disks=0|1`。
  - **GPT + FAT32 只读 `/disk/` 挂载 (`fs/fat32.rs` & `fs/mod.rs`)**：解析 GPT `EFI PART` (`ef00` GUID) 与 FAT32 BPB/簇链/LFN+8.3 文件名，透明集成于 `disk/` 路径并强制写保护，输出 `[wmos] disk fat32 ok files=<n>` 或 `skipped`。
  - **协作式多任务调度器 (`task/mod.rs`)**：通过 `global_asm!` `switch_context` / `task_trampoline` 运行 3 个 16 KiB 栈任务（`desktop-ui`、`sys-monitor`、`idle-worker`），开机验证 2 次切换并输出 `[wmos] sched ok tasks=3`，主循环每轮调用 `task::yield_now()`。
  - **ACPI 关机 (`arch/{io,cpu}.rs`)**：新增 16/32 位端口读写与 `cpu::shutdown()`（`0x604`/`0xB004` `0x2000`、`0x4004` `0x3400`）。
  - **GUI 与 Shell 升级 (`gfx/{color,apps,desktop}.rs` & `shell.rs`)**：新增 `Theme::{Dark, Ocean, Amber}`、5 窗口桌面与 `WMOS Files` 文件管理器（支持浏览与只读打开 `/disk/` 文件）、8 项 Start 菜单（含主题切换与 `Shutdown`）；终端扩展至 22 条命令（新增 `lspci`、`ps`、`disks`、`theme`、`poweroff` 及 `ls /disk`、`cat /disk/<path>`、`open files`、`open notepad <file>`）。
  - **CI 校验与文档同步**：`scripts/verify-qemu.sh` 在 UEFI 模式挂载 `dist/wmos.img` 验证真实 AHCI + FAT32 `/disk/` 挂载，校验 17 项启动标记与 7 条交互命令（含 `cat /disk/boot/limine/limine.conf` 与 `open files`）；同步更新 `docs/DESIGN.md`、`docs/CHANGELOG.md` 与 `README.md`。
- **验证结果**：
  - 本地 `cargo +stable-x86_64-pc-windows-gnu clean && cargo +stable-x86_64-pc-windows-gnu build --release`（inside `kernel/`）：0 errors, 0 warnings；ELF64 (`355536` B, `e_entry=0xffffffff8000ccb0`, 3 `PT_LOAD`)、`.sh` 纯 LF 与零禁用标记检查全部通过。
  - 待提交推送后由 GitHub Actions `Build and Verify WMOS` 执行云端 BIOS/UEFI + SATA 磁盘交互验证。

---

## [v2.0.0] - 2026-11-27

### `4bd2fd7` — 新增内核堆、CMOS RTC、RAMFS、双缓冲桌面、Paint/Notepad 应用与文档同步
- **变更类型**：`feat` / `ci` / `docs`
- **改动文件**：
  - `kernel/src/main.rs`、`kernel/src/arch/{mod.rs,rtc.rs}`、`kernel/src/mem/{mod.rs,heap.rs}`、`kernel/src/fs/mod.rs`
  - `kernel/src/shell.rs`、`kernel/src/gfx/{mod.rs,fb.rs,ui.rs,apps.rs,desktop.rs}`
  - `scripts/verify-qemu.sh`、`docs/DESIGN.md`、`docs/CHANGELOG.md`、`README.md`
- **变更摘要**：
  - 实现 `HEAP_START = 0xFFFF_A000_0000_0000` 全局分配器、`0x70`/`0x71` CMOS RTC、`BTreeMap` RAMFS、`Vec<u32>` RAM 双缓冲帧缓存、`PaintApp` / `NotepadApp`、Start 菜单以及支持历史与 Tab 补全的 17 命令终端。
- **验证结果**：
  - GitHub Actions Run [`45925487872`](https://github.com/wmsir/wmos/actions/runs/45925487872) 全绿通过（BIOS + UEFI 双模式与 `ls`/`write`/`cat`/`date`/`open paint` 交互全部通过，产出 `wmos-images` `3372980` B 与 `wmos-ci-logs-and-preview` `12830` B）。

---

## [docs-baseline] - 2026-11-27

### `bec16e4` — 新增系统设计文档、变更日志与强制文档同步规范
- **变更类型**：`docs`
- **改动文件**：
  - `docs/DESIGN.md`、`docs/CHANGELOG.md`、`AGENTS.md`、`README.md`
- **变更摘要**：
  - 建立代码核验版 `docs/DESIGN.md`、`docs/CHANGELOG.md` 与 `AGENTS.md` 文档同步硬性规范，并在 `README.md` 顶部补充规范入口与目录树。
- **验证结果**：
  - GitHub Actions Run [`45922116722`](https://github.com/wmsir/wmos/actions/runs/45922116722) 全绿通过（`wmos-images` `3170777` B、`wmos-ci-logs-and-preview` `12454` B）。

---

## [v1.0.0-mvp] - 2026-11-27

### `c494b6d` — 修复 PS/2 鼠标初始化与中断状态同步竞争
- **变更类型**：`fix`
- **改动文件**：
  - `kernel/src/drivers/{ps2.rs,mouse.rs,keyboard.rs}`
- **变更摘要**：
  - `ps2::init()` 在发送鼠标 `0xF6` / `0xF4` 与 `flush()` 期间保持 IRQ1/IRQ12 屏蔽，并在 `mouse`/`keyboard` 中增加 `0x64` 状态校验与 `cycle == 0` 同步校验。
- **验证结果**：
  - GitHub Actions Run [`45913901340`](https://github.com/wmsir/wmos/actions/runs/45913901340) 全绿通过（`[wmos] mouse active (756, 385)` 与 `[wmos] cmd: help` 均通过，产出 `wmos-images` `3170785` B）。

### `481a0ac` — 升级 `wmos v1.0.0-mvp` 完整内核、内存管理、PS/2 驱动与交互桌面终端
- **变更类型**：`feat`
- **改动文件**：
  - `kernel/src/arch/{mod.rs,cpu.rs,gdt.rs,idt.rs,pic.rs,pit.rs}`、`kernel/src/mem/{mod.rs,pmm.rs,vmm.rs}`
  - `kernel/src/drivers/{mod.rs,ps2.rs,keyboard.rs,mouse.rs}`、`kernel/src/shell.rs`、`kernel/src/gfx/{mod.rs,fb.rs,ui.rs,desktop.rs}`、`kernel/src/main.rs`、`scripts/verify-qemu.sh`、`README.md`
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
