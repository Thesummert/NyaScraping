# EmberMobile 无线调试后端

`nya-debug-adapter` 现在可以通过 EmberMobile 手机网关配对、连接探针、上传烧录固件，并将手机 GDB 服务接入已有调试会话。协议依据 `ember-mobile/native/ember-v2`、`docs/protocol-v2.md`、`docs/pairing.md` 和 `docs/protocol.md` 实现；构建不依赖旁边的 ember-mobile 仓库或 emberctl 可执行文件。

桌面入口为「设置 → 无线调试…」，也可在「设置 → 调试设置」勾选「使用 EmberMobile 无线调试」。后端 API 与命令行示例仍可独立使用。

## 桌面使用

1. 手机启动 Ember 服务，电脑打开「设置 → 无线调试…」。输入手机地址（例如 `192.168.1.100:4242`）和本机名称，点击「配对此地址」。
2. 核对桌面显示的八位校验码与手机一致，在手机上同意配对。成功后自动选择该手机；以后可在下拉列表中选择、刷新或删除配对。
3. 填写具体芯片型号、SWD/JTAG、速率和核心编号，选择本地 GDB 和 ELF/AXF 文件。手机地址变化时可直接修改地址，仍会核对设备身份。
4. 关闭设置，使用工具栏「连接」或「下载」。未连接时「下载」通过手机上传烧录并复位；连接后沿用 GDB 下载、断点、单步及变量操作。

无线参数自动保存到桌面配置目录的 `wireless.json`，并随 `.nyaproj` 项目保存；项目只记录设备 ID 和参数，配对密钥仍保存在本机后端凭据库。旧项目缺少无线配置时默认使用本地后端。无线调试不需要安装本地 OpenOCD/pyOCD/probe-rs，也不会使用它们的芯片配置文件。

## 准备和验证

手机启动 Ember 服务，与电脑处于可互通的局域网。在手机上授予 USB 权限并连接调试探针；后端不能代替 Android 授权。首次配对无需连接 MCU。

```sh
# 首次配对：两端核对八位码，然后在手机上点击同意
cargo run -p nya-debug-adapter --example wireless -- pair 192.168.1.100:4242 我的电脑
cargo run -p nya-debug-adapter --example wireless -- paired

# 使用上一步返回的完整设备 ID；芯片必须使用 probe-rs 的具体型号
cargo run -p nya-debug-adapter --example wireless -- flash <设备ID> STM32H723ZGTx /path/firmware.axf
cargo run -p nya-debug-adapter --example wireless -- debug <设备ID> STM32H723ZGTx /path/firmware.axf /path/arm-none-eabi-gdb

# 手机地址改变时，最后两个可选位置参数为 GDB 路径和新的 TCP 入口
cargo run -p nya-debug-adapter --example wireless -- debug <设备ID> STM32H723ZGTx /path/firmware.axf /path/arm-none-eabi-gdb 192.168.1.101:4242
cargo run -p nya-debug-adapter --example wireless -- forget <设备ID>
```

调试示例提供 `continue`、`pause`、`step`、`next`、`out`、`reset`、`download`、`break <位置>`、`watch <表达式>`、`status`、`quit`。连接只加载本地符号；显式执行 `download` 才通过 GDB vFlash 下载，完成后沿用已有自动复位流程。独立 `flash` 上传并校验文件，再等待手机烧录、回读校验和复位运行全部成功。

网络需要放行手机的 TCP 4242（公开发现）、UDP 4243（QUIC）和所选 GDB TCP 端口（默认 3333）。TCP 发现请求不携带长期凭据，配对和控制均通过固定证书的 QUIC 连接；握手失败不会降级到 TCP v1。手机 GDB 端口遵循原实现，是没有认证和 TLS 的标准 RSP 服务，应在可信局域网使用。

## 后端接口

- `nya_domain::WirelessRegistry`：`pair_wireless(endpoint, computer_name, on_code)`、`paired_wireless_devices()`、`forget_wireless(device_id)`。这些方法通过 `DebugAdapter` 实现；配对校验码由回调立即传给界面，最终授权只由手机确认。重复配对会优先验证并复用已有有效凭据。
- `WirelessConfig::new(device_id)`：默认使用已保存地址、核心 0、SWD、1000 kHz；可覆盖 `endpoint`、`core`、`speed_khz`、`protocol`、`under_reset`。地址覆盖仍校验完整设备 ID 和原证书指纹。目前使用明确的 TCP 地址入口，不自动浏览 mDNS。
- `DebugAction::ConnectWireless { profile, connection, reset }`：`profile.target` 是手机 probe-rs 芯片名，`firmware` 和 `gdb_executable` 是电脑本地路径，`gdb_port` 是手机服务端口。`profile.tool/interface/transport` 不参与无线工具选择，SWD/JTAG 取自 `WirelessConfig`。本地 GDB 和符号加载通过后才操作远端探针。
- `DebugAction::FlashWireless { profile, connection }`：支持 ELF/AXF/HEX，文件限 1–64 MiB，独立烧录接口按手机协议复位核心 0。其他核心可通过 `ConnectWireless` 选择后使用 GDB 下载。
- 连接后继续使用 `DebugSession` 的暂停、继续、源码单步、断点、栈帧、变量读取/修改、复位、下载和断开。`Application::execute(Command::Debug(...))` 可直接传入无线操作，不需要选择或安装本地 OpenOCD/pyOCD/probe-rs。运行中独立内存采样接口目前不支持无线会话。

上述方法会阻塞，应在后端工作线程调用。`DebugAdapter::set_install_progress` 的共享进度用于报告上传和手机烧录进度。成功完成以前不会报告 100%。

## 凭据和生命周期

配对凭据保存在 NyaScrapingCore 配置目录下的 `wireless/paired-devices.dat`，与 emberctl 的独立存储互不共享。Windows 使用当前用户的 DPAPI 加密，Unix 的目录/文件权限分别为 0700/0600；写入采用进程间锁与原子替换。公开设备列表不包含密钥。手机更换证书、撤销配对或重装后，需要重新配对。

手机已经连接同型号目标时复用该目标；已有其他型号目标、正在运行的 GDB 或烧录任务时拒绝接管。本次建立的 GDB 服务在断开、连接失败和适配器销毁时停止；只释放本次附着的目标，保留手机原有目标。所有目标修改请求携带探针 `session_id`，烧录状态还核对 `job_id`，避免误操作后续会话。

提交烧录后断网或超时不表示烧录取消。后端不会自动重试提交、断开目标或宣称成功，应先在手机检查任务和目标状态。离线时仍可删除本机配对，`forget_wireless` 返回 `false` 表示未能同时在手机撤销，请在手机配对列表另行删除。

## 自动验证

```sh
cargo test -p nya-debug-adapter -p nya-application -p nya-domain
cargo test -p nya-scraping --bin nya-scraping
cargo check -p nya-debug-adapter --examples
cargo run -p nya-scraping --example wireless_ui_check
```

无线测试使用本机真实 TCP/QUIC/TLS 模拟手机，检查 TLS exporter 校验码、认证、证书固定、协议字段、固件分块和 SHA-256、任务结果与资源清理；调试动作使用现有模拟 GDB/MI 进程验证。这些测试需要允许本地 TCP/UDP 套接字。尚未以真实 Android 手机、探针和 MCU 完成硬件联调。

桌面测试覆盖无线配置校验、项目保存及旧项目兼容、无需安装本地调试后端的无线配置，以及配对校验码和设备列表事件。`wireless_ui_check` 离屏渲染普通配置和配对等待状态，检查地址刷新与设备选择，覆盖 1480×900 和 1180×720 窗口；传入截图路径前缀可导出 PNG，无需显示服务器或连接真实手机。
