# NyaScraping

面向 MCU 的桌面调试工作台，使用 **Slint + Rust** 构建界面，以 **NyaScrapingCore** 作为调试后端，文本处理由项目内的 **Rust 原生文本后端**提供。

界面采用三栏布局：左侧为文件树与变量解析，中间为源码/波形展示，右侧为已添加的监视变量，顶部中央为调试工具栏。目前接通工具发现、源码读取、编译产物解析和 OpenOCD 运行中内存波形采集。顶部的 GDB 连接、下载及调试控制按钮暂不可用；波形页有独立的采集连接入口。

## 项目结构

```text
NyaScraping/
├── apps/desktop/                 # 桌面程序：组装依赖、工作线程、UI 模型映射
│   ├── src/                     # main.rs、presenter.rs
│   └── ui/                      # Slint 入口、主题、组件、功能页面
├── crates/
│   ├── domain/                  # 调试/工具/文本接口、会话状态、波形样本缓冲
│   ├── application/             # 命令、事件与用例；不依赖具体后端或 Slint
│   ├── debug-adapter/           # NyaScrapingCore 的 Rust 适配
│   ├── text-backend/            # 纯 Rust：文件读写、编码检测/转换、换行与安全保存
│   └── firmware-backend/        # 纯 Rust：编译产物、DWARF 函数/变量/类型与源码位置
├── backends/
│   └── NyaScrapingCore/         # Git 子模块，保持独立开发
├── docs/                        # 架构、界面与实施路线
├── examples/                    # 目标配置设计样例
└── .github/workflows/           # Rust/Slint/桥接与原生后端检查
```

详细设计见 [架构说明](docs/architecture.md) 和 [界面与实施路线](docs/roadmap.md)。

## 获取代码

```sh
git clone --recurse-submodules git@github.com:Thesummert/NyaScraping.git
cd NyaScraping
# 已克隆的仓库：
git submodule update --init --recursive
```

调试后端子模块使用 SSH 地址，提交由父仓库固定，不在构建过程中自动更新。

## 构建运行

需要 Rust stable（Slint 1.18.1 要求 Rust ≥ 1.92）及平台链接工具。Slint 与 slint-build 固定为相同版本。版本要求参考 [Slint 包清单](https://docs.rs/crate/slint/1.18.1/source/Cargo.toml)。文本后端使用纯 Rust 依赖，不需要 ICU、CXX 或 CMake 配置。

macOS：

```sh
brew install rustup
rustup-init
# 需已安装 Xcode Command Line Tools；已有 Rust 工具链可直接运行：
cargo run -p nya-scraping
```

Ubuntu / Debian：

```sh
sudo apt-get install build-essential pkg-config \
  libfontconfig1-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb-shape0-dev libxcb-xfixes0-dev
# 安装 Rust stable 后：
cargo run -p nya-scraping
```

Windows 使用 Rust MSVC 工具链和 Visual Studio C++ Build Tools；本次尚未在 Windows 验证桌面程序构建。

开发构建已开启优化并保留调试信息，软件渲染器使用更高优化级别，以减少高分屏上的重绘卡顿。首次重新构建依赖会花更长时间。源码与符号报告使用横向滚动，不随窗口宽度重新折行；文件树只在展开或折叠时更新结构，选择变量保留列表模型和滚动位置。固件文件树构建与报告格式化在后台线程完成。

源码工作台可输入文件绝对路径，通过 `nya-text-backend` 读取文本、编码与换行信息。文本后端支持 UTF-8、UTF-16LE/BE、BOM、常见旧编码、换行归一化与保留编码的保存；也提供 `load_with_encoding` 供调用方显式选择文件编码。工具页扫描 PATH 与 NyaScrapingCore 管理的安装目录；启动时也会扫描一次。硬件操作需要另行安装对应工具和 GDB。

左侧文件树区域输入编译产物路径后点击“解析文件”，通过 `nya-firmware-backend` 查看格式、架构、32/64 位、字节序、入口地址、段、节和普通/动态符号表。支持 ELF（包括常见的 ELF 格式 AXF、OUT、O、SO）、PE/COFF 和单架构 Mach-O；以文件内容识别，与扩展名无关，误写为 `.efl` 也可读取。段地址为虚拟地址，可重定位文件中的地址通常是节内偏移；BSS 单独标识为无文件数据。

包含嵌入式 DWARF 2–5 的文件会显示“目录 → 源文件 → 函数 → 参数/局部变量”文件树，全局/文件静态变量归到声明文件下。点击节点展开或折叠，在左侧下方的解析区域查看原始编译路径、声明行列号、函数链接名和地址范围、变量类型、大小、编译期常量及存储位置。类型支持基础类型、typedef、结构/联合/枚举名称、指针、引用、const/volatile 和数组维度。文件树来自调试信息，不要求源码仍在本机；保留 Windows 盘符并统一路径分隔符。支持跨编译单元引用、内联实例、GNU 压缩调试节，以及 DWARF 5 索引地址/字符串和位置列表；ELF 可重定位对象的调试节会应用重定位。

没有可用 DWARF 时从符号表回退提取函数和静态数据符号，放在“无源码位置”分组，类型、参数和源码位置标为未知。缺失、损坏或无法解码的调试信息会显示提示，保留段/节/符号表结果。建议以 `-g` 构建并保留调试信息；优化可能使局部变量的位置不可用。位置表达式和位置列表需要目标栈帧/寄存器才能求值，当前不求值。选中变量后点击「+ 监视」可添加到右侧，也可手动输入表达式并移除条目；输入名称与固件中唯一变量一致时自动解析为该变量。重复变量按条目身份去重，同名但不同作用域的变量可分别添加。成功重新解析编译产物时会停止采集、清除旧产物的变量监视与波形，保留未解析的手动表达式。

## 实时波形

1. 解析与目标上正在运行的固件一致的可执行 ELF/AXF，保留 DWARF 类型和固定内存地址。
2. 右键左侧变量选择「加入实时波形」，或在右侧监视条目上打开「波形」小开关。两处共用通道状态，重复添加不会增加通道；最多 16 路。移除监视条目也会移除其波形通道。
3. 使用板卡对应的配置启动 OpenOCD，例如 `openocd -f interface/stlink.cfg -f target/stm32f4x.cfg`（按实际探针和芯片替换配置）。目标应已运行；桌面采集器不会自动下载、复位或继续目标。
4. 在波形页填写 OpenOCD Tcl 地址（默认 `127.0.0.1:6666`）并点击「开始采集」。这里是 Tcl RPC 端口，与 GDB 的 3333 端口不同。目标/探针需要支持运行中读取该内存；不支持时显示错误并停止，不会自动暂停 CPU。Tcl 协议与 `read_memory` 参见 [OpenOCD Tcl RPC](https://openocd.org/doc-release/html/Tcl-Scripting-API.html) 和 [内存读取命令](https://openocd.org/doc/html/General-Commands.html)。

采集使用独立线程、持久 TCP 连接与 TCP_NODELAY。相邻/重叠变量合并成块（不跨越未选中的地址间隙），优先使用对齐的 32 位传输；一轮所有块合并成一次 RPC，不逐变量启动进程或通过 GDB 求值。收到响应后立即采集下一轮，没有人为轮询间隔。UI 每 33 ms 获取一次快照，波形页不可见时跳过绘图；每通道预分配 32768 点环形缓冲，按像素桶保留最小/最大值绘图。页面显示实测平均采集轮数/秒、保留点数、缓冲淘汰数和非有限值数。停止后保留现有波形，重新开始会清空缓冲。

支持 DWARF 标明的有/无符号 8/16/32/64 位整数、32/64 位浮点和布尔值，递归解析 typedef 与 const/volatile 修饰并遵循目标字节序。需要栈帧的局部变量、结构体/数组/指针、未知类型、可重定位目标文件和未解析表达式不能打开采集开关。

采集不执行目标端采样代码，也不发送 halt/resume/reset 或内存写入命令；SWD/JTAG 内存读取仍可能与 CPU/DMA 争用总线，不能保证零性能影响。实际采样率取决于探针、时钟、芯片和通道分布，尚未做真实探针的吞吐/性能测量。时间戳为主机读请求起止的中点，同轮通道并非硬件同步采样，64 位值可能跨两次 32 位读取；无法保证捕捉采样间隔之间的所有变化。带数据缓存的芯片可能读到尚未回写的内存视图，采集器不会通过缓存维护干扰目标。数值绘图使用 f64，超过 53 位有效位的整数存在舍入。当前时窗固定为最近 5 秒（受缓冲容量限制）；触发、游标、缩放、导出及 RTT/SWO 采集尚未接入。

解析不依赖 GDB 或探针，不执行输入文件。输入及解压后的 DWARF 各限制 128 MiB；段/节/符号合计、DWARF DIE 数量和源码文件表分别限制 200000 条。段/节/符号报告每类最多预览 1000 条，文件树使用虚拟列表展示。尚不加载外部 PDB、dSYM、DWO/DWP 或补充类型文件，不进行反汇编；裸 BIN、Intel HEX、旧式非 ELF AXF、归档库和通用多架构 Mach-O 不在本次支持范围。

## 验证

```sh
cargo fmt --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

# 离屏性能检查及模拟 OpenOCD 的波形采集/开关/关闭验证（需要本地 TCP 监听权限）
cargo run -p nya-scraping --example ui_perf --locked

# 单独验证 Rust 文本后端：
cargo test -p nya-text-backend --locked
# 单独验证编译产物解析：
cargo test -p nya-firmware-backend --locked
```

直接运行上述验证命令即可。格式检查仅覆盖父 workspace，避免改动独立子模块。根目录 `Cargo.lock` 用于锁定桌面程序依赖，应随代码一起提交。

文本后端测试覆盖 Unicode 路径与正文、UTF-8/UTF-16 往返、旧编码、BOM、混合换行、编码失败保留原文件、文件大小限制，以及 Unix 符号链接与权限。已在 macOS / Apple Silicon 上通过 workspace 测试、桌面构建、格式和 Clippy 检查；验证时保留失效的 ICU `PKG_CONFIG_PATH`，构建成功且程序未链接 Homebrew ICU。尚未进行窗口交互、真实探针连接或固件下载验收。

## 子模块开发

```sh
# 查看父仓库固定的版本
git submodule status
# 在子模块内修改、测试并提交后，父仓库再记录新的提交指针
git add backends/NyaScrapingCore
```

调试核心的修复放在子仓库，面向本应用的调试适配放在 `crates/debug-adapter`，文本处理在 `crates/text-backend` 内独立开发。不要直接在构建脚本里拉取调试后端最新分支。
