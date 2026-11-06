use crate::non_stop::NonStopBridge;
use crate::{
    mi::{self, Client, Value},
    DebugAdapter,
};
use nya_domain::waveform::ReadBlock;
use nya_domain::*;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Session {
    pub client: Option<Client>,
    bridge: Option<NonStopBridge>,
    memory_backend: Option<ToolKind>,
    state: SessionState,
    last_stop: u64,
    context_pending: bool,
    frames: Vec<StackFrame>,
    selected_frame: u32,
    breakpoints: Vec<Breakpoint>,
    message: String,
    log: String,
}
impl Session {
    fn accept_stop(&mut self) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        if client.state != SessionState::Halted {
            self.context_pending = false;
            return false;
        }
        if self.last_stop == client.stop_generation {
            return false;
        }
        self.last_stop = client.stop_generation;
        self.selected_frame = 0;
        self.frames = client
            .stop
            .as_ref()
            .and_then(|stop| stop.get("frame"))
            .map(stack_frame)
            .into_iter()
            .collect();
        self.context_pending = true;
        self.message = format!(
            "目标已暂停：{}",
            client
                .stop
                .as_ref()
                .map(|s| s.field("reason"))
                .unwrap_or("stopped")
        );
        true
    }
    fn client(&mut self) -> Result<&mut Client> {
        self.client
            .as_mut()
            .ok_or_else(|| Error("请先连接调试目标".into()))
    }
    fn halted(&mut self) -> Result<&mut Client> {
        let client = self.client()?;
        if client.state != SessionState::Halted {
            return Err(Error("目标暂停后才能执行此操作".into()));
        }
        Ok(client)
    }
    fn context(&mut self) -> Result<()> {
        if self.client()?.state != SessionState::Halted {
            self.frames.clear();
            self.context_pending = false;
            return Ok(());
        }
        let stack = self.client()?.command("-stack-list-frames 0 63")?;
        self.frames = stack
            .get("stack")
            .map(Value::list)
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.get("frame"))
            .map(stack_frame)
            .collect();
        self.context_pending = false;
        self.last_stop = self.client()?.stop_generation;
        Ok(())
    }
    fn breakpoint_list(&mut self) -> Result<()> {
        let list = self.client()?.command("-break-list")?;
        self.breakpoints = list
            .get("BreakpointTable")
            .and_then(|table| table.get("body"))
            .map(Value::list)
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.get("bkpt"))
            .map(|bp| Breakpoint {
                id: bp.field("number").into(),
                location: if !bp.field("original-location").is_empty() {
                    bp.field("original-location").into()
                } else {
                    format!(
                        "{}:{} {}",
                        bp.field("fullname"),
                        bp.field("line"),
                        bp.field("func")
                    )
                },
                enabled: bp.field("enabled") == "y",
                file: if bp.field("fullname").is_empty() {
                    bp.field("file")
                } else {
                    bp.field("fullname")
                }
                .into(),
                line: bp.field("line").parse().unwrap_or(0),
            })
            .collect();
        Ok(())
    }
    /// All-stop remote targets require a brief interrupt to change breakpoints.
    fn breakpoint_command(&mut self, command: &str) -> Result<()> {
        self.client()?.poll()?;
        let was_running = self.client()?.state == SessionState::Running;
        let mut resume = false;
        if was_running {
            self.client()?.command("-exec-interrupt --all")?;
            let start = Instant::now();
            while self.client()?.state == SessionState::Running {
                if start.elapsed() > Duration::from_secs(3) {
                    return Err(Error("等待目标暂停超时，未修改断点".into()));
                }
                std::thread::sleep(Duration::from_millis(10));
                self.client()?.poll()?;
            }
            resume = self.client()?.stop.as_ref().is_some_and(|stop| {
                stop.field("reason") == "signal-received" && stop.field("signal-name") == "SIGINT"
            });
        }
        let result = (|| {
            self.halted()?.command(command)?;
            self.breakpoint_list()
        })();
        // Restore execution even if setting a breakpoint failed. A real breakpoint
        // racing with the interrupt must stay stopped and retain its source context.
        if resume {
            self.last_stop = self.client()?.stop_generation;
            self.halted()?.command("-exec-continue")?;
            self.frames.clear();
        } else if was_running {
            self.selected_frame = 0;
            self.context()?;
        }
        result
    }

    fn thread_state(&mut self) -> Result<()> {
        let info = self.client()?.command("-thread-info")?;
        let threads = info.get("threads").map(Value::list).unwrap_or_default();
        if threads.iter().any(|t| t.field("state") == "stopped") {
            self.client()?.state = SessionState::Halted;
        } else if threads.iter().any(|t| t.field("state") == "running") {
            self.client()?.state = SessionState::Running;
        } else {
            return Err(Error("GDB 已连接，但没有可调试的线程".into()));
        }
        Ok(())
    }

    fn reset_and_halt(&mut self) -> Result<()> {
        self.halted()?.command(&format!(
            "-interpreter-exec console {}",
            mi::quote("monitor reset halt")
        ))?;
        // A monitor reset changes registers behind GDB's back. Invalidate both
        // its cached registers and frames before querying the reset context.
        self.client()?.command(&format!(
            "-interpreter-exec console {}",
            mi::quote("maintenance flush register-cache")
        ))?;
        self.thread_state()?;
        self.selected_frame = 0;
        self.context()
    }
    fn validate_scope(&self, watch: &WatchRequest) -> Result<()> {
        if let Some(function) = &watch.function {
            let current = self
                .frames
                .iter()
                .find(|f| f.level == self.selected_frame)
                .map(|f| f.function.as_str())
                .unwrap_or_default();
            if current != function
                && !current
                    .strip_prefix(function.as_str())
                    .is_some_and(|s| s.starts_with('('))
            {
                return Err(Error(format!("变量属于 {function}，当前栈帧为 {current}")));
            }
        }
        Ok(())
    }
    fn evaluate(&mut self, watch: &WatchRequest) -> Result<WatchValue> {
        self.validate_scope(watch)?;
        let result = self.halted()?.command_timeout(
            &format!("-var-create - * {}", mi::quote(&watch.expression)),
            Duration::from_millis(700),
        )?;
        let name = result.field("name");
        let value = WatchValue {
            expression: watch.expression.clone(),
            type_name: result.field("type").into(),
            display_value: result.field("value").into(),
            error: None,
        };
        if !name.is_empty() {
            self.client()?.command_timeout(
                &format!("-var-delete {}", mi::quote(name)),
                Duration::from_millis(700),
            )?;
        }
        Ok(value)
    }
    fn values(
        &mut self,
        watches: &[WatchRequest],
        limit: usize,
        budget: Duration,
    ) -> Vec<EvaluatedWatch> {
        let start = Instant::now();
        watches
            .iter()
            .enumerate()
            .map(|(index, watch)| {
                let result = if index >= limit || start.elapsed() >= budget {
                    Err(Error("本轮刷新达到上限，请减少展开项或增大刷新间隔".into()))
                } else {
                    self.evaluate(watch)
                };
                EvaluatedWatch {
                    id: watch.id.clone(),
                    value: result.unwrap_or_else(|error| WatchValue {
                        expression: watch.expression.clone(),
                        type_name: String::new(),
                        display_value: "—".into(),
                        error: Some(error.to_string()),
                    }),
                }
            })
            .collect()
    }

    fn assign(&mut self, watch: &WatchRequest, value: &str) -> Result<String> {
        self.validate_scope(watch)?;
        let variable = self
            .halted()?
            .command(&format!("-var-create - * {}", mi::quote(&watch.expression)))?;
        let name = variable.field("name");
        if name.is_empty() {
            return Err(Error("GDB 未返回变量标识".into()));
        }
        let result = (|| {
            let attributes = self
                .halted()?
                .command(&format!("-var-show-attributes {}", mi::quote(name)))?;
            if attributes.field("attr") != "editable" {
                return Err(Error("该表达式不可写，请选择可修改的变量或成员".into()));
            }
            self.halted()?.command(&format!(
                "-var-assign {} {}",
                mi::quote(name),
                mi::quote(value)
            ))?;
            let readback = self.halted()?.command(&format!(
                "-data-evaluate-expression {}",
                mi::quote(&watch.expression)
            ))?;
            Ok(readback.field("value").to_owned())
        })();
        let cleanup = self
            .client()?
            .command(&format!("-var-delete {}", mi::quote(name)));
        match result {
            Ok(value) => {
                cleanup?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }

    fn write_watch(&mut self, watch: &WatchRequest, value: &str) -> Result<()> {
        let value = value.trim();
        if value.is_empty() || value.len() > 4096 {
            return Err(Error("请输入有效参数值（最多 4096 字节）".into()));
        }
        self.client()?.poll()?;
        let resume = self.client()?.state == SessionState::Running;
        if !resume
            && self.client()?.state == SessionState::Halted
            && self.last_stop != self.client()?.stop_generation
        {
            self.selected_frame = 0;
            self.context()?;
            self.last_stop = self.client()?.stop_generation;
        }
        if resume {
            self.client()?.command("-exec-interrupt --all")?;
            let start = Instant::now();
            while self.client()?.state == SessionState::Running {
                self.client()?.poll()?;
                if start.elapsed() > Duration::from_secs(3) {
                    return Err(Error("等待目标暂停超时，未写入参数".into()));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // A breakpoint racing with our interrupt must remain stopped.
            self.last_stop = self.client()?.stop_generation;
            let stop = self.client()?.stop.as_ref();
            if !stop.is_some_and(|s| {
                s.field("reason") == "signal-received" && s.field("signal-name") == "SIGINT"
            }) {
                self.context()?;
                return Err(Error(
                    "目标因断点或其他事件暂停，未写入参数；请检查后重试".into(),
                ));
            }
            self.selected_frame = 0;
            self.halted()?.command("-stack-select-frame 0")?;
            self.context()?;
        }
        let readback = self
            .assign(watch, value)
            .map_err(|error| Error(format!("参数写入或回读失败，目标保持暂停：{error}")))?;
        if resume {
            self.halted()?.command("-exec-continue").map_err(|error| {
                Error(format!(
                    "参数已写入并回读为 {readback}，恢复运行失败：{error}"
                ))
            })?;
            self.frames.clear();
        }
        self.message = format!(
            "{} 已写入，回读：{}{}",
            watch.expression,
            readback,
            if resume { "；已恢复运行" } else { "" }
        );
        Ok(())
    }
}

impl DebugSession for DebugAdapter {
    fn execute_debug(&mut self, action: DebugAction) -> Result<()> {
        match action {
            DebugAction::Connect {
                profile,
                external_server,
            } => {
                if self.session.client.is_some() {
                    return Err(Error("调试会话已存在，请先断开".into()));
                }
                if !profile.firmware.is_file() {
                    return Err(Error("请选择存在的 ELF/AXF 调试文件".into()));
                }
                if profile.gdb_port == 0 || profile.gdb_executable.as_os_str().is_empty() {
                    return Err(Error("请填写 GDB 路径与有效端口".into()));
                }
                // Spawn/validate GDB before touching the probe. Never download on attach.
                self.session = Session::default();
                let mut client = Client::spawn(&profile.gdb_executable)?;
                client.command("-gdb-set pagination off")?;
                client.command("-gdb-set confirm off")?;
                client.command("-gdb-set mi-async on")?;
                client.command("-gdb-set may-call-functions off")?;
                // Frame argument values are available in watches. Fetching them
                // while formatting *stopped delays the initial stop notification.
                client.command("-gdb-set print frame-arguments none")?;
                if matches!(profile.tool, ToolKind::PyOcd | ToolKind::ProbeRs) {
                    client.command(&format!(
                        "-interpreter-exec console {}",
                        mi::quote("maintenance set target-non-stop on")
                    ))?;
                    client.command("-gdb-set trust-readonly-sections off")?;
                    // GDB's stack/code caches batch unwinder reads and invalidate
                    // on resume. Ordinary data reads still fetch live target bytes.
                    client.command("-gdb-set code-cache on")?;
                    client.command("-gdb-set stack-cache on")?;
                    client.command("-gdb-set mem inaccessible-by-default off")?;
                }
                client.command(&format!(
                    "-file-exec-and-symbols {}",
                    mi::quote(&profile.firmware.to_string_lossy())
                ))?;
                if !external_server {
                    self.start_server(&profile)?;
                }
                let mut endpoint = std::net::SocketAddr::from(([127, 0, 0, 1], profile.gdb_port));
                if matches!(profile.tool, ToolKind::PyOcd | ToolKind::ProbeRs) {
                    let bridge = match if profile.tool == ToolKind::PyOcd {
                        NonStopBridge::start_native(endpoint)
                    } else {
                        NonStopBridge::start(endpoint)
                    } {
                        Ok(bridge) => bridge,
                        Err(error) => {
                            let _ = self.stop_server();
                            return Err(error);
                        }
                    };
                    endpoint = bridge.address();
                    self.session.bridge = Some(bridge);
                }
                let result = client.command(&format!("-target-select remote {endpoint}"));
                if let Err(error) = result {
                    self.session.bridge = None;
                    let _ = self.stop_server();
                    return Err(error);
                }
                self.session.client = Some(client);
                self.session.memory_backend = Some(profile.tool);
                if let Err(error) = self
                    .session
                    .thread_state()
                    .and_then(|()| self.session.context())
                {
                    self.session.client = None;
                    self.session.bridge = None;
                    self.session.memory_backend = None;
                    let _ = self.stop_server();
                    return Err(error);
                }
                self.session.breakpoint_list()?;
                self.session.last_stop = self.session.client()?.stop_generation;
                self.session.client()?.changed = false;
                self.session.message = "调试目标已连接".into();
            }
            DebugAction::Disconnect => {
                if let Some(client) = self.session.client.as_mut() {
                    if client.state == SessionState::Running {
                        let _ =
                            client.command_timeout("-exec-interrupt --all", Duration::from_secs(1));
                    }
                    let _ = client.command_timeout("-target-disconnect", Duration::from_secs(1));
                }
                self.session.client = None;
                self.session.bridge = None;
                self.session.memory_backend = None;
                self.session.state = SessionState::Disconnected;
                self.session.frames.clear();
                self.session.breakpoints.clear();
                self.stop_server()?;
                self.session.message = "已断开调试会话".into();
            }
            DebugAction::Continue
            | DebugAction::StepInto
            | DebugAction::StepOver
            | DebugAction::StepOut => {
                let command = match action {
                    DebugAction::Continue => "-exec-continue",
                    DebugAction::StepInto => "-exec-step",
                    DebugAction::StepOver => "-exec-next",
                    _ => "-exec-finish",
                };
                self.session.halted()?.command(command)?;
                self.session.frames.clear();
                self.session.selected_frame = 0;
                self.session.message = "目标执行中".into();
            }
            DebugAction::Pause => {
                if self.session.client()?.state != SessionState::Running {
                    return Err(Error("目标当前未运行".into()));
                }
                self.session.client()?.command("-exec-interrupt --all")?;
                self.session.message = "已请求暂停，等待目标停止".into();
            }
            DebugAction::Reset => {
                self.session.reset_and_halt()?;
                self.session.message = "已复位目标".into();
            }
            DebugAction::Flash { profile } => {
                if self.server.is_some() || self.session.client.is_some() {
                    return Err(Error("请先断开调试会话再直接烧录".into()));
                }
                if !profile.firmware.is_file() {
                    return Err(Error("请选择存在的 ELF/AXF 固件文件".into()));
                }
                let progress = self.install_progress.clone();
                if let Ok(mut value) = progress.lock() {
                    *value = crate::InstallProgress {
                        stage: "正在烧录固件，请稍候…".into(),
                        ..Default::default()
                    };
                }
                self.flash(&profile)?;
                // OpenOCD's program command already verifies and resets before exit.
                if profile.tool != ToolKind::OpenOcd {
                    if let Ok(mut value) = progress.lock() {
                        value.stage = "固件已写入，正在复位目标…".into();
                    }
                    self.reset(&profile).map_err(|error| {
                        Error(format!("固件已写入，但复位失败，请检查目标状态：{error}"))
                    })?;
                }
                if let Ok(mut value) = progress.lock() {
                    value.stage = "固件已烧录并自动复位".into();
                    value.percent = Some(100.0);
                    value.remaining_seconds = Some(0);
                }
                self.session = Session::default();
                self.session.message = "固件已烧录并自动复位".into();
            }
            DebugAction::Download => {
                self.session.client()?.poll()?;
                let progress = self.install_progress.clone();
                if let Ok(mut value) = progress.lock() {
                    *value = crate::InstallProgress {
                        stage: "正在准备下载固件…".into(),
                        ..Default::default()
                    };
                }
                if self.session.client()?.state == SessionState::Running {
                    if let Ok(mut value) = progress.lock() {
                        value.stage = "正在自动暂停目标，准备烧录…".into();
                    }
                    self.session.client()?.command("-exec-interrupt --all")?;
                    let started = Instant::now();
                    while self.session.client()?.state == SessionState::Running {
                        if started.elapsed() >= Duration::from_secs(3) {
                            return Err(Error("等待目标暂停超时，未下载固件".into()));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        self.session.client()?.poll()?;
                    }
                }
                self.session.halted()?;
                let started = Instant::now();
                let result = self.session.halted()?.command_timeout_observe(
                    "-target-download",
                    Duration::from_secs(300),
                    |record| {
                        if record.kind != b'+' || record.class != "download" {
                            return;
                        }
                        let done = record.data.field("total-sent").parse::<u64>().ok();
                        let total = record
                            .data
                            .field("total-size")
                            .parse::<u64>()
                            .ok()
                            .filter(|n| *n > 0);
                        if let Ok(mut value) = progress.lock() {
                            // Some section-start records omit total-sent; retain the last measurement.
                            let done = done.or(value.downloaded);
                            let speed = done.filter(|n| *n > 0).and_then(|n| {
                                let seconds = started.elapsed().as_secs_f64();
                                (seconds >= 0.1)
                                    .then(|| (n as f64 / seconds) as u64)
                                    .filter(|n| *n > 0)
                            });
                            let total = total.or(value.total);
                            *value = crate::InstallProgress {
                                stage: "正在传输固件并等待目标完成烧录…".into(),
                                downloaded: done,
                                total,
                                percent: done
                                    .zip(total)
                                    .map(|(d, t)| (d as f64 / t as f64 * 100.0).min(99.0) as f32),
                                bytes_per_second: speed,
                                remaining_seconds: done
                                    .zip(total)
                                    .zip(speed)
                                    .map(|((d, t), s)| t.saturating_sub(d).div_ceil(s)),
                            };
                        }
                    },
                )?;
                // Flash algorithms use target RAM and registers. Continuing
                // directly from their final state can fault or run old code.
                self.session.reset_and_halt().map_err(|error| {
                    Error(format!("固件已写入，但复位失败，请检查目标状态：{error}"))
                })?;
                if let Ok(mut value) = progress.lock() {
                    // GDB's progress total can include non-loadable debug
                    // sections; the final load-size is the actual byte count.
                    let size = result.field("load-size").parse::<u64>().ok();
                    value.stage = "固件已下载并复位，等待继续运行".into();
                    value.downloaded = size.or(value.downloaded);
                    value.total = size.or(value.total);
                    value.percent = Some(100.0);
                    value.remaining_seconds = Some(0);
                }
                self.session.message = "固件已下载并复位，点击继续运行".into();
            }
            DebugAction::AddBreakpoint(location) => {
                if location.trim().is_empty() {
                    return Err(Error("请输入函数名或 文件:行号".into()));
                }
                self.session
                    .breakpoint_command(&format!("-break-insert {}", mi::quote(location.trim())))?;
                self.session.message = "断点已添加".into();
            }
            DebugAction::RemoveBreakpoint(id) => {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
                    return Err(Error("无效断点编号".into()));
                }
                self.session
                    .breakpoint_command(&format!("-break-delete {id}"))?;
                self.session.message = "断点已移除".into();
            }
            DebugAction::AddSourceBreakpoint { file, line } => {
                if file.is_empty() || line == 0 {
                    return Err(Error("无效的源码断点位置".into()));
                }
                self.session.breakpoint_command(&format!(
                    "-break-insert --source {} --line {}",
                    mi::quote(&file),
                    line
                ))?;
                self.session.message = "断点已添加".into();
            }
            DebugAction::SetBreakpointEnabled { id, enabled } => {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
                    return Err(Error("无效断点编号".into()));
                }
                self.session.breakpoint_command(&format!(
                    "-break-{} {id}",
                    if enabled { "enable" } else { "disable" }
                ))?;
                self.session.message = if enabled {
                    "断点已启用"
                } else {
                    "断点已禁用"
                }
                .into();
            }
            DebugAction::SelectFrame(level) => {
                if self.session.context_pending {
                    self.session.context()?;
                }
                self.session
                    .halted()?
                    .command(&format!("-stack-select-frame {level}"))?;
                self.session.selected_frame = level;
                self.session.message = format!("已选择栈帧 #{level}");
            }
            DebugAction::WriteWatch { watch, value } => {
                if let Err(error) = self.session.write_watch(&watch, &value) {
                    self.session.message = format!("操作失败：{error}");
                    return Err(error);
                }
            }
            DebugAction::Refresh => {
                self.session.thread_state()?;
                self.session.context()?;
                self.session.breakpoint_list()?;
            }
        }
        Ok(())
    }
    fn poll_debug(&mut self) -> Result<bool> {
        let result = (|| {
            if let Some(server) = self.server.as_mut() {
                if let Some(status) = server.try_wait().map_err(crate::error)? {
                    return Err(Error(format!("GDB 服务已退出：{status}")));
                }
            }
            let Some(client) = &mut self.session.client else {
                return Ok(false);
            };
            let changed = client.poll()?;
            if !changed && !self.session.context_pending {
                return Ok(false);
            }
            self.session.state = client.state;
            if client.state == SessionState::Disconnected {
                self.session.client = None;
                self.session.frames.clear();
                self.session.breakpoints.clear();
                self.stop_server()?;
                self.session.message = "目标会话已结束".into();
            } else if client.state == SessionState::Halted {
                // Publish the MI stop frame first; unwind on a later worker turn
                // so a deep stack cannot hold back the confirmed halt state.
                if !self.session.accept_stop() && self.session.context_pending {
                    self.session.context()?;
                }
            } else {
                self.session.frames.clear();
                self.session.context_pending = false;
            }
            Ok(true)
        })();
        if let Err(error) = &result {
            if let Some(client) = &self.session.client {
                self.session.log = client.logs.iter().cloned().collect::<Vec<_>>().join("\n");
            }
            self.session.client = None;
            self.session.bridge = None;
            self.session.memory_backend = None;
            let _ = self.stop_server();
            self.session.state = SessionState::Faulted;
            self.session.frames.clear();
            self.session.breakpoints.clear();
            self.session.message = error.to_string();
        }
        result
    }
    fn sample_watches(&mut self, watches: &[WatchRequest]) -> DebugSnapshot {
        // GDB evaluates only an already-halted target. Running-target reads use
        // the independent memory sampler, which never interrupts or resumes it.
        let mut snapshot = self.debug_snapshot(watches);
        snapshot.watch_sample = snapshot.state == SessionState::Halted
            && !snapshot.context_pending
            && !watches.is_empty();
        snapshot
    }

    fn read_live_memory(&mut self, blocks: &[ReadBlock]) -> Result<Vec<Vec<u8>>> {
        if !matches!(
            self.session.memory_backend,
            Some(ToolKind::PyOcd | ToolKind::ProbeRs)
        ) {
            return Err(Error("请先连接 pyOCD 或 probe-rs 调试会话".into()));
        }
        if blocks.len() > 64
            || blocks.iter().any(|b| {
                b.byte_size == 0
                    || b.byte_size > 1024
                    || b.address.checked_add(b.byte_size as u64).is_none()
            })
        {
            return Err(Error("内存采集读取块超出限制".into()));
        }
        let client = self.session.client()?;
        if !matches!(client.state, SessionState::Running | SessionState::Halted) {
            return Err(Error("当前调试连接无法采集内存".into()));
        }
        // Stop scheduling new blocks after 500 ms, but give an issued command
        // its full timeout so an exhausted batch budget cannot poison GDB.
        let deadline = Instant::now() + Duration::from_millis(500);
        blocks
            .iter()
            .map(|block| {
                if Instant::now() >= deadline {
                    return Err(Error("本轮内存读取超时，请减少不连续地址".into()));
                }
                let result = client.command_timeout(
                    &format!(
                        "-data-read-memory-bytes 0x{:x} {}",
                        block.address, block.byte_size
                    ),
                    Duration::from_millis(500),
                )?;
                decode_memory(&result, block)
            })
            .collect()
    }

    fn debug_snapshot(&mut self, watches: &[WatchRequest]) -> DebugSnapshot {
        self.session.accept_stop();
        let state = self
            .session
            .client
            .as_ref()
            .map(|c| c.state)
            .unwrap_or(self.session.state);
        let mut values = Vec::new();
        if state == SessionState::Halted && !self.session.context_pending {
            values = self.session.values(watches, 128, Duration::from_secs(2));
        }
        if let Some(client) = &mut self.session.client {
            self.session.log = client.logs.iter().cloned().collect::<Vec<_>>().join("\n");
        }
        let state = self
            .session
            .client
            .as_ref()
            .map(|c| c.state)
            .unwrap_or(state);
        if state != SessionState::Halted {
            values.clear();
            self.session.frames.clear();
        }
        if state == SessionState::Faulted {
            self.session.message = "GDB 通信超时或会话失效，请重新连接".into();
            self.session.client = None;
            let _ = self.stop_server();
            self.session.state = SessionState::Faulted;
        }
        DebugSnapshot {
            context_pending: state == SessionState::Halted && self.session.context_pending,
            watch_sample: false,
            watch_sample_error: None,
            source: None,
            state,
            message: self.session.message.clone(),
            frames: self.session.frames.clone(),
            selected_frame: self.session.selected_frame,
            breakpoints: self.session.breakpoints.clone(),
            values,
            log: self.session.log.clone(),
        }
    }
}

fn stack_frame(frame: &Value) -> StackFrame {
    StackFrame {
        level: frame.field("level").parse().unwrap_or(0),
        function: frame.field("func").into(),
        file: frame
            .get("fullname")
            .or_else(|| frame.get("file"))
            .map(Value::text)
            .unwrap_or_default()
            .into(),
        line: frame.field("line").parse().unwrap_or(0),
        address: frame.field("addr").into(),
    }
}

fn decode_memory(result: &Value, block: &ReadBlock) -> Result<Vec<u8>> {
    let chunks = result.get("memory").map(Value::list).unwrap_or_default();
    let invalid = || Error("调试后端返回了不完整或无效的内存数据".into());
    let mut bytes = Vec::with_capacity(block.byte_size);
    for chunk in chunks {
        let begin = chunk
            .field("begin")
            .strip_prefix("0x")
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .ok_or_else(invalid)?;
        let end = chunk
            .field("end")
            .strip_prefix("0x")
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .ok_or_else(invalid)?;
        let contents = chunk.field("contents");
        if begin != block.address + bytes.len() as u64
            || end < begin
            || end > block.address + block.byte_size as u64
            || contents.len() != (end - begin) as usize * 2
            || !contents.is_ascii()
        {
            return Err(invalid());
        }
        for offset in (0..contents.len()).step_by(2) {
            bytes.push(
                u8::from_str_radix(&contents[offset..offset + 2], 16).map_err(|_| invalid())?,
            );
        }
    }
    if bytes.len() != block.byte_size {
        return Err(invalid());
    }
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use nya_scraping_core::manager::Manager;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn fixture() -> (tempfile::TempDir, DebugAdapter, TargetProfile) {
        let root = tempfile::tempdir().unwrap();
        let gdb = root.path().join("fake gdb");
        fs::write(&gdb, include_bytes!("../tests/fixtures/fake_gdb.py")).unwrap();
        fs::set_permissions(&gdb, fs::Permissions::from_mode(0o755)).unwrap();
        let firmware = root.path().join("firmware with spaces.axf");
        fs::write(&firmware, b"fixture").unwrap();
        let adapter = DebugAdapter {
            manager: Manager {
                config_path: root.path().join("config/config"),
                data_dir: root.path().join("data"),
            },
            install_cancel: Default::default(),
            install_progress: Default::default(),
            server: None,
            session: Session::default(),
        };
        let profile = TargetProfile {
            name: "fake".into(),
            tool: ToolKind::OpenOcd,
            target: "test".into(),
            interface: None,
            transport: None,
            gdb_port: 3333,
            gdb_executable: gdb,
            firmware,
        };
        (root, adapter, profile)
    }

    #[test]
    fn stops_are_published_before_stack_and_watch_reads_and_stale_refreshes_are_dropped() {
        for tool in [ToolKind::PyOcd, ToolKind::ProbeRs] {
            let (_root, mut adapter, mut profile) = fixture();
            profile.tool = tool;
            adapter
                .execute_debug(DebugAction::Connect {
                    profile,
                    external_server: true,
                })
                .unwrap();
            let watch = WatchRequest {
                id: "counter".into(),
                expression: "counter".into(),
                function: Some("compute".into()),
            };
            adapter.execute_debug(DebugAction::Continue).unwrap();
            adapter
                .session
                .client()
                .unwrap()
                .command("-test-hold-context")
                .unwrap();
            adapter
                .session
                .client()
                .unwrap()
                .command("-test-break-on-interrupt")
                .unwrap();
            adapter.execute_debug(DebugAction::Pause).unwrap();
            for _ in 0..50 {
                adapter.poll_debug().unwrap();
                if adapter.debug_snapshot(&[]).state == SessionState::Halted {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let fast = adapter.sample_watches(std::slice::from_ref(&watch));
            assert_eq!(fast.state, SessionState::Halted);
            assert!(fast.context_pending);
            assert_eq!(fast.frames[0].function, "compute");
            assert!(fast.values.is_empty());
            assert!(!fast.watch_sample);
            adapter
                .session
                .client()
                .unwrap()
                .command("-test-release-context")
                .unwrap();
            assert!(adapter.poll_debug().unwrap());
            let complete = adapter.debug_snapshot(std::slice::from_ref(&watch));
            assert!(!complete.context_pending);
            assert_eq!(complete.frames.len(), 2);
            assert_eq!(complete.values[0].value.display_value, "42");

            // Continuing before the slow refresh must discard the pending work.
            adapter.execute_debug(DebugAction::Continue).unwrap();
            adapter
                .session
                .client()
                .unwrap()
                .command("-test-hold-context")
                .unwrap();
            adapter.execute_debug(DebugAction::Pause).unwrap();
            for _ in 0..50 {
                adapter.poll_debug().unwrap();
                if adapter.debug_snapshot(&[]).state == SessionState::Halted {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            adapter.execute_debug(DebugAction::Continue).unwrap();
            adapter.poll_debug().unwrap();
            let running = adapter.debug_snapshot(&[watch]);
            assert_eq!(running.state, SessionState::Running);
            assert!(!running.context_pending);
            assert!(running.frames.is_empty());
            assert!(running.values.is_empty());
        }
    }

    #[test]
    fn monitor_refresh_never_interrupts_or_resumes_a_running_target() {
        let (_root, mut adapter, profile) = fixture();
        let watches = [WatchRequest {
            id: "live".into(),
            expression: "counter".into(),
            function: None,
        }];
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        let paused = adapter.sample_watches(&watches);
        assert!(paused.watch_sample);
        assert_eq!(paused.values[0].value.display_value, "42");
        adapter.execute_debug(DebugAction::Continue).unwrap();
        // This fake target stops on any interrupt. Repeated refreshes must leave it running.
        adapter
            .session
            .client()
            .unwrap()
            .command("-test-break-on-interrupt")
            .unwrap();
        for _ in 0..3 {
            let snapshot = adapter.sample_watches(&watches);
            assert_eq!(snapshot.state, SessionState::Running);
            assert!(!snapshot.watch_sample);
            assert!(snapshot.watch_sample_error.is_none());
            assert!(snapshot.values.is_empty());
        }
        adapter.execute_debug(DebugAction::Pause).unwrap();
        adapter.poll_debug().unwrap();
        assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Halted);
    }

    #[test]
    fn additional_backends_read_live_bytes_without_interrupt_or_reconnect() {
        for tool in [ToolKind::PyOcd, ToolKind::ProbeRs] {
            let (_root, mut adapter, mut profile) = fixture();
            profile.tool = tool;
            adapter
                .execute_debug(DebugAction::Connect {
                    profile,
                    external_server: true,
                })
                .unwrap();
            adapter.execute_debug(DebugAction::Continue).unwrap();
            adapter
                .session
                .client()
                .unwrap()
                .command("-test-break-on-interrupt")
                .unwrap();
            let blocks = [ReadBlock {
                address: 0x20000000,
                byte_size: 4,
                little_endian: true,
            }];
            assert_eq!(
                adapter.read_live_memory(&blocks).unwrap(),
                [vec![1, 2, 3, 4]]
            );
            assert_eq!(
                adapter.read_live_memory(&blocks).unwrap(),
                [vec![2, 3, 4, 5]]
            );
            assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Running);
            let invalid = [ReadBlock {
                address: 0xdeadbeef,
                ..blocks[0].clone()
            }];
            assert!(adapter
                .read_live_memory(&invalid)
                .unwrap_err()
                .0
                .contains("Memory access failed"));
            assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Running);
            adapter
                .session
                .client()
                .unwrap()
                .command("-test-break-on-memory")
                .unwrap();
            assert!(adapter.read_live_memory(&blocks).is_ok());
            adapter.poll_debug().unwrap();
            let snapshot = adapter.debug_snapshot(&[]);
            assert_eq!(snapshot.state, SessionState::Halted);
            assert!(!snapshot.frames.is_empty());
            adapter.execute_debug(DebugAction::Disconnect).unwrap();
            assert!(adapter.read_live_memory(&blocks).is_err());
            assert!(adapter.session.bridge.is_none());
        }
    }

    #[test]
    fn memory_results_reject_gaps_overlaps_wrong_addresses_and_invalid_hex() {
        let block = ReadBlock {
            address: 0x20000000,
            byte_size: 4,
            little_endian: false,
        };
        let response = |begin: &str, end: &str, contents: &str| {
            Value::Tuple(vec![(
                "memory".into(),
                Value::List(vec![Value::Tuple(vec![
                    ("begin".into(), Value::Text(begin.into())),
                    ("end".into(), Value::Text(end.into())),
                    ("contents".into(), Value::Text(contents.into())),
                ])]),
            )])
        };
        assert_eq!(
            decode_memory(&response("0x20000000", "0x20000004", "12345678"), &block).unwrap(),
            [0x12, 0x34, 0x56, 0x78]
        );
        for invalid in [
            response("0x20000001", "0x20000005", "12345678"),
            response("0x20000000", "0x20000002", "1234"),
            response("0x20000000", "0x20000004", "gggggggg"),
            Value::Tuple(vec![]),
        ] {
            assert!(decode_memory(&invalid, &block).is_err());
        }
    }

    #[test]
    fn parameter_writes_read_back_and_resume_only_on_success() {
        let (_root, mut adapter, profile) = fixture();
        let watch = WatchRequest {
            id: "parameter".into(),
            expression: "counter".into(),
            function: None,
        };
        assert!(adapter
            .execute_debug(DebugAction::WriteWatch {
                watch: watch.clone(),
                value: "1".into()
            })
            .is_err());
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        let write = |adapter: &mut DebugAdapter, expression: &str, value: &str| {
            adapter.execute_debug(DebugAction::WriteWatch {
                watch: WatchRequest {
                    expression: expression.into(),
                    ..watch.clone()
                },
                value: value.into(),
            })
        };
        write(&mut adapter, "counter", "17").unwrap();
        let snapshot = adapter.debug_snapshot(std::slice::from_ref(&watch));
        assert_eq!(snapshot.state, SessionState::Halted);
        assert_eq!(snapshot.values[0].value.display_value, "17");
        assert!(write(&mut adapter, "readonly", "5").is_err());
        assert!(write(&mut adapter, "2 + 3", "5").is_err());
        assert!(write(&mut adapter, "counter", "").is_err());
        adapter.execute_debug(DebugAction::Continue).unwrap();
        write(&mut adapter, "counter", "23").unwrap();
        let snapshot = adapter.debug_snapshot(&[]);
        assert_eq!(snapshot.state, SessionState::Running);
        assert!(snapshot.message.contains("23"));
        assert!(snapshot.message.contains("已恢复运行"));
        assert!(write(&mut adapter, "counter", "invalid").is_err());
        let snapshot = adapter.debug_snapshot(std::slice::from_ref(&watch));
        assert_eq!(snapshot.state, SessionState::Halted);
        assert_eq!(snapshot.values[0].value.display_value, "23");
        adapter.execute_debug(DebugAction::Continue).unwrap();
        adapter
            .session
            .client()
            .unwrap()
            .command("-test-break-on-interrupt")
            .unwrap();
        assert!(write(&mut adapter, "counter", "99").is_err());
        let snapshot = adapter.debug_snapshot(std::slice::from_ref(&watch));
        assert_eq!(snapshot.state, SessionState::Halted);
        assert_eq!(snapshot.values[0].value.display_value, "23");
    }

    #[test]
    fn running_breakpoints_are_installed_and_real_stops_are_preserved() {
        let (_root, mut adapter, profile) = fixture();
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        adapter.execute_debug(DebugAction::Continue).unwrap();
        adapter
            .execute_debug(DebugAction::AddSourceBreakpoint {
                file: "/fixture/main.c".into(),
                line: 8,
            })
            .unwrap();
        let snapshot = adapter.debug_snapshot(&[]);
        assert_eq!(snapshot.state, SessionState::Running);
        assert_eq!(snapshot.breakpoints[0].line, 8);
        adapter
            .execute_debug(DebugAction::SetBreakpointEnabled {
                id: "1".into(),
                enabled: false,
            })
            .unwrap();
        let snapshot = adapter.debug_snapshot(&[]);
        assert_eq!(snapshot.state, SessionState::Running);
        assert!(!snapshot.breakpoints[0].enabled);
        let error = adapter
            .execute_debug(DebugAction::AddSourceBreakpoint {
                file: "missing.c".into(),
                line: 8,
            })
            .unwrap_err();
        assert!(error.to_string().contains("No source file"));
        assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Running);
        adapter
            .session
            .client()
            .unwrap()
            .command("-test-break-on-interrupt")
            .unwrap();
        adapter
            .execute_debug(DebugAction::RemoveBreakpoint("1".into()))
            .unwrap();
        let snapshot = adapter.debug_snapshot(&[]);
        assert_eq!(snapshot.state, SessionState::Halted);
        assert_eq!(snapshot.frames[0].line, 8);
        assert!(snapshot.breakpoints.is_empty());
    }

    #[test]
    fn source_breakpoint_and_enable_disable_round_trip() {
        let (_root, mut adapter, profile) = fixture();
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        adapter
            .execute_debug(DebugAction::AddSourceBreakpoint {
                file: "/fixture/main.c".into(),
                line: 8,
            })
            .unwrap();
        let bp = adapter.debug_snapshot(&[]).breakpoints.remove(0);
        assert_eq!(
            (bp.file.as_str(), bp.line, bp.enabled),
            ("/fixture/main.c", 8, true)
        );
        adapter
            .execute_debug(DebugAction::SetBreakpointEnabled {
                id: bp.id.clone(),
                enabled: false,
            })
            .unwrap();
        assert!(!adapter.debug_snapshot(&[]).breakpoints[0].enabled);
        adapter
            .execute_debug(DebugAction::SetBreakpointEnabled {
                id: bp.id,
                enabled: true,
            })
            .unwrap();
        assert!(adapter.debug_snapshot(&[]).breakpoints[0].enabled);
        assert!(adapter
            .execute_debug(DebugAction::SetBreakpointEnabled {
                id: "1\n-exec-continue".into(),
                enabled: false
            })
            .is_err());
    }

    #[test]
    fn connect_controls_breakpoints_frames_values_and_cleanup() {
        let (_root, mut adapter, profile) = fixture();
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        let watch = WatchRequest {
            id: "stable-id".into(),
            expression: "counter".into(),
            function: Some("compute".into()),
        };
        let snapshot = adapter.debug_snapshot(std::slice::from_ref(&watch));
        assert_eq!(snapshot.state, SessionState::Halted);
        assert_eq!(snapshot.frames.len(), 2);
        assert_eq!(snapshot.values[0].value.display_value, "42");
        assert_eq!(snapshot.values[0].value.type_name, "int");
        adapter
            .execute_debug(DebugAction::AddBreakpoint("compute".into()))
            .unwrap();
        assert_eq!(adapter.debug_snapshot(&[]).breakpoints[0].id, "1");
        adapter.execute_debug(DebugAction::Continue).unwrap();
        assert_eq!(
            adapter.debug_snapshot(std::slice::from_ref(&watch)).state,
            SessionState::Running
        );
        assert!(adapter
            .debug_snapshot(std::slice::from_ref(&watch))
            .values
            .is_empty());
        assert!(adapter.execute_debug(DebugAction::StepOver).is_err());
        assert!(adapter.select(ToolKind::PyOcd, "1.0").is_err());
        assert!(adapter.uninstall(ToolKind::PyOcd, "1.0").is_err());
        adapter.execute_debug(DebugAction::Pause).unwrap();
        for _ in 0..30 {
            adapter.poll_debug().unwrap();
            if adapter.debug_snapshot(&[]).state == SessionState::Halted {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        adapter.execute_debug(DebugAction::SelectFrame(1)).unwrap();
        let snapshot = adapter.debug_snapshot(std::slice::from_ref(&watch));
        assert_eq!(snapshot.selected_frame, 1);
        assert!(snapshot.values[0]
            .value
            .error
            .as_deref()
            .unwrap()
            .contains("compute"));
        adapter.poll_debug().unwrap();
        assert_eq!(adapter.debug_snapshot(&[]).selected_frame, 1);
        for action in [
            DebugAction::StepInto,
            DebugAction::StepOver,
            DebugAction::StepOut,
            DebugAction::Reset,
            DebugAction::Download,
        ] {
            adapter.execute_debug(action).unwrap();
            adapter.poll_debug().unwrap();
        }
        let progress = adapter.install_progress.lock().unwrap().clone();
        assert_eq!(progress.downloaded, Some(1024));
        assert_eq!(progress.total, Some(1024));
        assert_eq!(progress.percent, Some(100.0));
        let missing = WatchRequest {
            id: "missing".into(),
            expression: "missing".into(),
            function: None,
        };
        assert!(adapter.debug_snapshot(&[missing]).values[0]
            .value
            .error
            .as_deref()
            .unwrap()
            .contains("No symbol"));
        adapter
            .execute_debug(DebugAction::RemoveBreakpoint("1".into()))
            .unwrap();
        assert!(adapter.debug_snapshot(&[]).breakpoints.is_empty());
        adapter.execute_debug(DebugAction::Disconnect).unwrap();
        assert!(adapter.session.client.is_none());
        assert_eq!(
            adapter.debug_snapshot(&[watch]).state,
            SessionState::Disconnected
        );
    }

    #[test]
    fn download_resets_flash_context_and_flushes_cached_frames_before_continue() {
        for tool in [ToolKind::OpenOcd, ToolKind::PyOcd, ToolKind::ProbeRs] {
            let (_root, mut adapter, mut profile) = fixture();
            profile.tool = tool;
            adapter
                .execute_debug(DebugAction::Connect {
                    profile,
                    external_server: true,
                })
                .unwrap();
            adapter.execute_debug(DebugAction::SelectFrame(1)).unwrap();
            adapter.execute_debug(DebugAction::Download).unwrap();
            let snapshot = adapter.debug_snapshot(&[]);
            assert_eq!(snapshot.state, SessionState::Halted);
            assert_eq!(snapshot.selected_frame, 0);
            assert!(snapshot.message.contains("已下载并复位"));
            adapter.execute_debug(DebugAction::Continue).unwrap();
            assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Running);
            adapter.execute_debug(DebugAction::Disconnect).unwrap();
        }
    }

    #[test]
    fn running_target_download_automatically_stops_then_flashes_and_resets() {
        for tool in [ToolKind::OpenOcd, ToolKind::PyOcd, ToolKind::ProbeRs] {
            let (_root, mut adapter, mut profile) = fixture();
            profile.tool = tool;
            adapter
                .execute_debug(DebugAction::Connect {
                    profile,
                    external_server: true,
                })
                .unwrap();
            adapter.execute_debug(DebugAction::Continue).unwrap();
            assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Running);
            adapter.execute_debug(DebugAction::Download).unwrap();
            let snapshot = adapter.debug_snapshot(&[]);
            assert_eq!(snapshot.state, SessionState::Halted);
            assert!(snapshot.message.contains("已下载并复位"));
            assert_eq!(
                adapter.install_progress.lock().unwrap().percent,
                Some(100.0)
            );
            adapter.execute_debug(DebugAction::Continue).unwrap();
            assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Running);
            adapter.execute_debug(DebugAction::Disconnect).unwrap();
        }
    }

    #[test]
    fn running_target_download_aborts_if_interrupt_fails() {
        let (_root, mut adapter, profile) = fixture();
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        adapter.execute_debug(DebugAction::Continue).unwrap();
        adapter
            .session
            .client()
            .unwrap()
            .command("-test-fail-interrupt")
            .unwrap();
        adapter.install_progress.lock().unwrap().percent = Some(100.0);
        let error = adapter.execute_debug(DebugAction::Download).unwrap_err();
        assert!(error.0.contains("Target interrupt failed"));
        assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Running);
        assert_ne!(
            adapter.install_progress.lock().unwrap().percent,
            Some(100.0)
        );
    }

    #[test]
    fn download_and_reset_failures_never_report_success_or_resume() {
        for (failure, expected) in [
            ("-test-fail-download", "Flash programming failed"),
            ("-test-fail-reset", "固件已写入，但复位失败"),
        ] {
            let (_root, mut adapter, profile) = fixture();
            adapter
                .execute_debug(DebugAction::Connect {
                    profile,
                    external_server: true,
                })
                .unwrap();
            adapter.install_progress.lock().unwrap().percent = Some(100.0);
            adapter.session.client().unwrap().command(failure).unwrap();
            let error = adapter.execute_debug(DebugAction::Download).unwrap_err();
            assert!(error.0.contains(expected));
            assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Halted);
            assert_ne!(
                adapter.install_progress.lock().unwrap().percent,
                Some(100.0)
            );
        }
    }

    #[test]
    fn failed_attach_and_transport_failure_release_owned_session() {
        let (_root, mut adapter, mut profile) = fixture();
        let mut missing = profile.clone();
        missing.gdb_executable = "/no/such/gdb".into();
        assert!(adapter
            .execute_debug(DebugAction::Connect {
                profile: missing,
                external_server: true
            })
            .is_err());
        assert!(adapter.session.client.is_none());
        profile.gdb_port = 0;
        assert!(adapter
            .execute_debug(DebugAction::Connect {
                profile: profile.clone(),
                external_server: true
            })
            .is_err());
        profile.gdb_port = 3333;
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        assert!(adapter
            .session
            .client()
            .unwrap()
            .command("-test-exit")
            .is_err());
        assert!(adapter.poll_debug().is_err());
        assert!(adapter.session.client.is_none());
        assert_eq!(adapter.debug_snapshot(&[]).state, SessionState::Faulted);
    }
}
