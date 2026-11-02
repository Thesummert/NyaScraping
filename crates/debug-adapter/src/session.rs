use crate::{
    mi::{self, Client, Value},
    DebugAdapter,
};
use nya_domain::*;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Session {
    pub client: Option<Client>,
    state: SessionState,
    last_stop: u64,
    frames: Vec<StackFrame>,
    selected_frame: u32,
    breakpoints: Vec<Breakpoint>,
    message: String,
    log: String,
}
impl Session {
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
            return Ok(());
        }
        let stack = self.client()?.command("-stack-list-frames 0 63")?;
        self.frames = stack
            .get("stack")
            .map(Value::list)
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.get("frame"))
            .map(|frame| StackFrame {
                level: frame.field("level").parse().unwrap_or(0),
                function: frame.field("func").into(),
                file: frame
                    .get("fullname")
                    .unwrap_or_else(|| frame.get("file").unwrap_or(frame))
                    .text()
                    .into(),
                line: frame.field("line").parse().unwrap_or(0),
                address: frame.field("addr").into(),
            })
            .collect();
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
            })
            .collect();
        Ok(())
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

    fn interrupt_for_sample(&mut self) -> Result<bool> {
        self.client()?.poll()?;
        if self.client()?.state != SessionState::Running {
            return Ok(false);
        }
        self.client()?.command("-exec-interrupt --all")?;
        let start = Instant::now();
        while self.client()?.state == SessionState::Running {
            self.client()?.poll()?;
            if start.elapsed() > Duration::from_secs(1) {
                return Err(Error("实时采样等待暂停超时，已停止自动刷新".into()));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Never automatically continue a breakpoint/exception that raced with sampling.
        let interrupted = self.client()?.stop.as_ref().is_some_and(|s| {
            s.field("reason") == "signal-received" && s.field("signal-name") == "SIGINT"
        });
        self.selected_frame = 0;
        self.context()?;
        self.last_stop = self.client()?.stop_generation;
        if !interrupted {
            return Err(Error("目标因断点或其他事件暂停，已停止自动刷新".into()));
        }
        self.halted()?.command("-stack-select-frame 0")?;
        Ok(true)
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
                client.command(&format!(
                    "-file-exec-and-symbols {}",
                    mi::quote(&profile.firmware.to_string_lossy())
                ))?;
                if !external_server {
                    self.start_server(&profile)?;
                }
                let result = client.command(&format!(
                    "-target-select remote 127.0.0.1:{}",
                    profile.gdb_port
                ));
                if let Err(error) = result {
                    let _ = self.stop_server();
                    return Err(error);
                }
                self.session.client = Some(client);
                if let Err(error) = self
                    .session
                    .thread_state()
                    .and_then(|()| self.session.context())
                {
                    self.session.client = None;
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
                self.session.halted()?.command(&format!(
                    "-interpreter-exec console {}",
                    mi::quote("monitor reset halt")
                ))?;
                self.session.thread_state()?;
                self.session.selected_frame = 0;
                self.session.context()?;
                self.session.message = "已复位目标".into();
            }
            DebugAction::Download => {
                self.session
                    .halted()?
                    .command_timeout("-target-download", Duration::from_secs(60))?;
                self.session.message = "固件下载完成".into();
            }
            DebugAction::AddBreakpoint(location) => {
                if location.trim().is_empty() {
                    return Err(Error("请输入函数名或 文件:行号".into()));
                }
                self.session
                    .halted()?
                    .command(&format!("-break-insert {}", mi::quote(location.trim())))?;
                self.session.breakpoint_list()?;
                self.session.message = "断点已添加".into();
            }
            DebugAction::RemoveBreakpoint(id) => {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
                    return Err(Error("无效断点编号".into()));
                }
                self.session
                    .halted()?
                    .command(&format!("-break-delete {id}"))?;
                self.session.breakpoint_list()?;
                self.session.message = "断点已移除".into();
            }
            DebugAction::SelectFrame(level) => {
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
            if !changed {
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
                if self.session.last_stop != client.stop_generation {
                    self.session.last_stop = client.stop_generation;
                    self.session.selected_frame = 0;
                    self.session.context()?;
                    self.session.message = format!(
                        "目标已暂停：{}",
                        self.session
                            .client()?
                            .stop
                            .as_ref()
                            .map(|stop| stop.field("reason"))
                            .unwrap_or("stopped")
                    );
                }
            } else {
                self.session.frames.clear();
            }
            Ok(true)
        })();
        if let Err(error) = &result {
            if let Some(client) = &self.session.client {
                self.session.log = client.logs.iter().cloned().collect::<Vec<_>>().join("\n");
            }
            self.session.client = None;
            let _ = self.stop_server();
            self.session.state = SessionState::Faulted;
            self.session.frames.clear();
            self.session.message = error.to_string();
        }
        result
    }
    fn sample_watches(&mut self, watches: &[WatchRequest]) -> DebugSnapshot {
        if watches.is_empty() || self.session.client.is_none() {
            return self.debug_snapshot(&[]);
        }
        let result: Result<Vec<EvaluatedWatch>> = (|| {
            let resume = self.session.interrupt_for_sample()?;
            // Use a smaller budget while running to limit the time spent stopped.
            let values = self.session.values(
                watches,
                if resume { 32 } else { 128 },
                if resume {
                    Duration::from_millis(100)
                } else {
                    Duration::from_secs(2)
                },
            );
            if resume {
                self.session.halted()?.command("-exec-continue")?;
                self.session.frames.clear();
            }
            Ok(values)
        })();
        let mut snapshot = self.debug_snapshot(&[]);
        snapshot.watch_sample = true;
        match result {
            Ok(values)
                if matches!(snapshot.state, SessionState::Halted | SessionState::Running) =>
            {
                snapshot.values = values
            }
            Ok(_) => {
                snapshot.watch_sample_error = Some("采样期间连接状态改变，已停止自动刷新".into())
            }
            Err(error) => snapshot.watch_sample_error = Some(error.to_string()),
        }
        if snapshot.watch_sample_error.is_some() {
            snapshot.values.clear();
        }
        snapshot
    }

    fn debug_snapshot(&mut self, watches: &[WatchRequest]) -> DebugSnapshot {
        let state = self
            .session
            .client
            .as_ref()
            .map(|c| c.state)
            .unwrap_or(self.session.state);
        let mut values = Vec::new();
        if state == SessionState::Halted {
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
            server: None,
            session: Session::default(),
        };
        let profile = TargetProfile {
            name: "fake".into(),
            tool: ToolKind::OpenOcd,
            target: "test".into(),
            interface: None,
            gdb_port: 3333,
            gdb_executable: gdb,
            firmware,
        };
        (root, adapter, profile)
    }

    #[test]
    fn live_samples_change_while_running_and_preserve_stop_events() {
        let (_root, mut adapter, profile) = fixture();
        let watches = vec![
            WatchRequest {
                id: "live".into(),
                expression: "live_counter".into(),
                function: None,
            },
            WatchRequest {
                id: "missing".into(),
                expression: "missing".into(),
                function: None,
            },
            WatchRequest {
                id: "scope".into(),
                expression: "local".into(),
                function: Some("other_function".into()),
            },
        ];
        assert_eq!(
            adapter.sample_watches(&watches).state,
            SessionState::Disconnected
        );
        adapter
            .execute_debug(DebugAction::Connect {
                profile,
                external_server: true,
            })
            .unwrap();
        let paused = adapter.sample_watches(&watches);
        assert!(paused.watch_sample);
        assert_eq!(paused.state, SessionState::Halted);
        adapter.execute_debug(DebugAction::Continue).unwrap();
        let first = adapter.sample_watches(&watches);
        assert_eq!(first.state, SessionState::Running);
        assert!(first.watch_sample_error.is_none());
        assert_eq!(first.values[0].id, "live");
        assert_eq!(first.values[0].value.display_value, "1");
        assert!(first.values[1].value.error.is_some());
        assert!(first.values[2].value.error.is_some());
        assert!(first.frames.is_empty());
        assert!(adapter.debug_snapshot(&watches).values.is_empty());
        assert!(adapter.sample_watches(&[]).values.is_empty());
        let second = adapter.sample_watches(&watches);
        assert_eq!(second.state, SessionState::Running);
        assert_eq!(second.values[0].value.display_value, "2");
        adapter
            .session
            .client()
            .unwrap()
            .command("-test-break-on-interrupt")
            .unwrap();
        let breakpoint = adapter.sample_watches(&watches);
        assert_eq!(breakpoint.state, SessionState::Halted);
        assert!(breakpoint.watch_sample_error.is_some());
        assert!(breakpoint.values.is_empty());
        assert!(!breakpoint.frames.is_empty());
        adapter.execute_debug(DebugAction::Disconnect).unwrap();
        assert_eq!(
            adapter.sample_watches(&watches).state,
            SessionState::Disconnected
        );
    }

    #[test]
    fn live_sampling_resume_failure_keeps_target_halted() {
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
            .command("-test-fail-continue")
            .unwrap();
        let snapshot = adapter.sample_watches(&[WatchRequest {
            id: "live".into(),
            expression: "counter".into(),
            function: None,
        }]);
        assert_eq!(snapshot.state, SessionState::Halted);
        assert!(snapshot
            .watch_sample_error
            .unwrap()
            .contains("Continue failed"));
        assert!(snapshot.values.is_empty());
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
