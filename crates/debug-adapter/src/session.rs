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
    fn evaluate(&mut self, watch: &WatchRequest) -> Result<WatchValue> {
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
    fn debug_snapshot(&mut self, watches: &[WatchRequest]) -> DebugSnapshot {
        let state = self
            .session
            .client
            .as_ref()
            .map(|c| c.state)
            .unwrap_or(self.session.state);
        let mut values = Vec::new();
        if state == SessionState::Halted {
            let start = Instant::now();
            for (index, watch) in watches.iter().enumerate() {
                let result = if index >= 128 || start.elapsed() > Duration::from_secs(2) {
                    Err(Error("本轮刷新达到上限，请减少展开项后刷新".into()))
                } else {
                    self.session.evaluate(watch)
                };
                values.push(EvaluatedWatch {
                    id: watch.id.clone(),
                    value: result.unwrap_or_else(|error| WatchValue {
                        expression: watch.expression.clone(),
                        type_name: String::new(),
                        display_value: "—".into(),
                        error: Some(error.to_string()),
                    }),
                });
            }
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
