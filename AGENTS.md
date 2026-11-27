# WMOS 工程协作与文档同步规范 (`AGENTS.md`)

所有在本仓库工作的开发者与 AI Agent 均须严格遵守以下规范。

---

## 1. 文档同步硬性要求（强制执行）

任何涉及内核源码、链接脚本、引导配置、终端命令、驱动逻辑、图形界面、打包脚本或 CI 工作流的修改，**必须**在**同一次提交**中同步更新以下文档，禁止仅改代码不更新文档：

1. **[docs/DESIGN.md](docs/DESIGN.md)**
   - 同步更新对应子系统的设计描述、数据结构/函数契约、内存地址与常量、IRQ 向量与掩码、串口状态标记表、镜像打包步骤或已知边界与演进路线。
2. **[docs/CHANGELOG.md](docs/CHANGELOG.md)**
   - 在顶部追加一条符合格式规范的变更记录，标明日期/版本、变更类型、改动文件列表、变更摘要与验证结果（本地编译检查与 CI Run 链接）。
3. **[README.md](README.md)**
   - 当修改影响用户可见特性、交互终端命令列表、仓库目录树、产物说明或本地构建/运行命令时，必须同步更新 `README.md`。

> **验收红线**：文档中的每一个文件路径、符号名称、数值常量、串口标记、命令名和构建指令都必须能在当前仓库源码中直接核验，严禁臆测或保留过时描述。

---

## 2. 内核工程与编译约束

- **100% Rust Stable `#![no_std]`**：
  - 严格遵守 [rust-toolchain.toml](rust-toolchain.toml)（`channel = "stable"`，`target = "x86_64-unknown-none"`、`edition = "2021"`）。
  - **禁止使用 `f32` / `f64`**（内核目标未启用浮点状态保存/SSE/x87）。
  - **禁止使用 nightly 特性**（如 `extern "x86-interrupt"`）；中断入口须通过 `core::arch::global_asm!` 实现。
  - **禁止待办注释、占位存根与编译器警告**；所有构建必须保证 **0 errors, 0 warnings**。
- **编译目录要求**：
  - 必须在 `kernel/` 目录内执行编译（例如 `(cd kernel && cargo build --release)` 或在 CI 中设置 `working-directory: kernel`），确保 Cargo 正确加载 [kernel/.cargo/config.toml](kernel/.cargo/config.toml) 中的 `-C relocation-model=static` 与 `-C code-model=kernel`。切勿在仓库根目录仅通过 `--manifest-path kernel/Cargo.toml` 编译。

---

## 3. 脚本、串口契约与行尾约束

- **Shell 脚本行尾强制 LF**：
  - `scripts/*.sh`（包括 [scripts/build-images.sh](scripts/build-images.sh) 与 [scripts/verify-qemu.sh](scripts/verify-qemu.sh)）**必须**保持纯 LF (`\n`)，严禁出现 CRLF (`\r\n` / `0` 个 `\r` 字节)，且须符合 [.gitattributes](.gitattributes) 中的 `*.sh text eol=lf` 规则。
- **串口校验契约联动**：
  - 修改或增删 [kernel/src/main.rs](kernel/src/main.rs)、[kernel/src/arch/idt.rs](kernel/src/arch/idt.rs)、[kernel/src/shell.rs](kernel/src/shell.rs) 或 [kernel/src/gfx/desktop.rs](kernel/src/gfx/desktop.rs) 中的 `[wmos] <marker>` 串口输出标记时，必须同步检查并更新 [scripts/verify-qemu.sh](scripts/verify-qemu.sh) 与 [docs/DESIGN.md](docs/DESIGN.md)。
- **镜像与 CI 一致性**：
  - 修改 [boot/limine.conf](boot/limine.conf) 内核路径或输出产物名时，必须同步更新 [scripts/build-images.sh](scripts/build-images.sh) 与 [.github/workflows/build.yml](.github/workflows/build.yml)。
