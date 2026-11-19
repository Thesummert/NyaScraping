//! Use cases run on a worker; UI callbacks submit commands and consume events.
use nya_domain::{
    firmware::{FirmwareBackend, FirmwareImage},
    DebugAction, DebugSession, DebugSnapshot, Document, Result, TargetCatalog, TargetProfile,
    TextBackend, ToolKind, ToolRegistry, ToolState, WatchRequest, WirelessConfig, WirelessDevice,
    WirelessRegistry,
};
use std::{path::PathBuf, sync::mpsc::SyncSender, time::Instant};

pub struct MemoryReadRequest {
    pub blocks: Vec<nya_domain::waveform::ReadBlock>,
    pub deadline: Instant,
    pub response: SyncSender<Result<Vec<Vec<u8>>>>,
}

pub enum Command {
    StartWirelessAcquisition {
        profile: TargetProfile,
        connection: WirelessConfig,
    },
    StopWirelessAcquisition(u64),
    ReadWirelessMemory {
        generation: u64,
        request: MemoryReadRequest,
    },
    RefreshWirelessDevices,
    PairWireless {
        endpoint: String,
        computer_name: String,
        comparison: SyncSender<String>,
    },
    ForgetWireless(String),
    RefreshTools,
    RefreshTargets,
    AddPack(PathBuf),
    RemovePack(PathBuf),
    InstallPack(String),
    Debug(DebugAction),
    UpdateWatches(Vec<WatchRequest>),
    InstallTool {
        tool: ToolKind,
        version: String,
    },
    SelectTool {
        tool: ToolKind,
        version: String,
    },
    UninstallTool {
        tool: ToolKind,
        version: String,
    },
    AddLocalTool {
        tool: ToolKind,
        name: String,
        directory: PathBuf,
    },
    RemoveLocalTool {
        tool: ToolKind,
        name: String,
    },
    OpenSource(PathBuf),
    OpenFirmware(PathBuf),
    ReloadFirmware(PathBuf),
    ReadLiveMemory(MemoryReadRequest),
}

pub enum Event {
    WirelessAcquisitionStarted(u64),
    /// Automatic cleanup does not complete an unrelated queued UI command.
    WirelessAcquisitionStopped {
        generation: u64,
        message: String,
    },
    WirelessDevicesUpdated {
        devices: Vec<WirelessDevice>,
        selected_id: Option<String>,
        message: String,
    },
    /// Internal acquisition response; never posted to the UI event queue.
    MemoryRead,
    TargetsUpdated(TargetCatalog),
    ToolsUpdated {
        state: ToolState,
        message: String,
    },
    DebugUpdated {
        snapshot: DebugSnapshot,
        completes_command: bool,
    },
    SourceOpened(Document),
    FirmwareOpened(Box<FirmwareImage>),
    FirmwareReloaded(Box<FirmwareImage>),
    Failed(String),
}

pub struct Application<T, E, F> {
    tools: T,
    text: E,
    firmware: F,
    watches: Vec<WatchRequest>,
    pending_breakpoints: Vec<nya_domain::Breakpoint>,
    next_pending_breakpoint: u64,
}

impl<T: ToolRegistry + DebugSession + WirelessRegistry, E: TextBackend, F: FirmwareBackend>
    Application<T, E, F>
{
    pub fn new(tools: T, text: E, firmware: F) -> Self {
        Self {
            tools,
            text,
            firmware,
            watches: Vec::new(),
            pending_breakpoints: Vec::new(),
            next_pending_breakpoint: 1,
        }
    }

    pub fn execute(&mut self, command: Command) -> Event {
        let result: Result<Event> = match command {
            Command::StartWirelessAcquisition {
                profile,
                connection,
            } => self
                .tools
                .start_wireless_acquisition(&profile, &connection)
                .map(Event::WirelessAcquisitionStarted),
            Command::StopWirelessAcquisition(generation) => Ok(Event::WirelessAcquisitionStopped {
                generation,
                message: match self.tools.stop_wireless_acquisition(generation) {
                    Ok(()) => "无线采集已停止，保留已有数据".into(),
                    Err(error) => format!("无线采集已停止，手机会话清理失败：{error}"),
                },
            }),
            Command::ReadWirelessMemory {
                generation,
                request,
            } => {
                let result = if Instant::now() >= request.deadline {
                    Err(nya_domain::Error("无线内存采集请求已过期".into()))
                } else {
                    self.tools.read_wireless_memory(generation, &request.blocks)
                };
                let _ = request.response.try_send(result);
                return Event::MemoryRead;
            }
            Command::RefreshWirelessDevices => {
                self.tools
                    .paired_wireless_devices()
                    .map(|devices| Event::WirelessDevicesUpdated {
                        devices,
                        selected_id: None,
                        message: "已配对手机列表已刷新".into(),
                    })
            }
            Command::PairWireless {
                endpoint,
                computer_name,
                comparison,
            } => self
                .tools
                .pair_wireless(&endpoint, &computer_name, &mut |code| {
                    let _ = comparison.try_send(code.into());
                })
                .and_then(|device| {
                    Ok(Event::WirelessDevicesUpdated {
                        devices: self.tools.paired_wireless_devices()?,
                        selected_id: Some(device.id),
                        message: format!("已配对 {}，可关闭设置后连接或下载", device.name),
                    })
                }),
            Command::ForgetWireless(id) => self.tools.forget_wireless(&id).and_then(|revoked| {
                Ok(Event::WirelessDevicesUpdated {
                    devices: self.tools.paired_wireless_devices()?,
                    selected_id: Some(String::new()),
                    message: if revoked {
                        "配对已删除，手机凭据已撤销"
                    } else {
                        "本机配对已删除；手机未能撤销，请在手机配对列表另行删除"
                    }
                    .into(),
                })
            }),
            Command::ReadLiveMemory(request) => {
                let result = if Instant::now() >= request.deadline {
                    Err(nya_domain::Error("内存采集请求已过期".into()))
                } else {
                    self.tools.read_live_memory(&request.blocks)
                };
                let _ = request.response.try_send(result);
                return Event::MemoryRead;
            }
            Command::Debug(mut action) => {
                if let DebugAction::Connect {
                    profile,
                    external_server: false,
                    ..
                }
                | DebugAction::Flash { profile } = &mut action
                {
                    match self.tools.active_backend() {
                        Ok(Some(tool)) => profile.tool = tool,
                        Ok(None) => return Event::Failed("请先在调试工具页选择后端".into()),
                        Err(error) => return Event::Failed(error.to_string()),
                    }
                }
                let state = self.tools.debug_snapshot(&[]).state;
                let mut pending_message = None;
                let flush_after = matches!(
                    &action,
                    DebugAction::Connect { .. }
                        | DebugAction::ConnectWireless { .. }
                        | DebugAction::Pause
                );
                let result = match action {
                    DebugAction::AddSourceBreakpoint { file, line }
                        if !matches!(
                            state,
                            nya_domain::SessionState::Halted | nya_domain::SessionState::Running
                        ) =>
                    {
                        if file.is_empty() || line == 0 {
                            Err(nya_domain::Error("无效的源码断点位置".into()))
                        } else {
                            let id = format!("pending:{}", self.next_pending_breakpoint);
                            self.next_pending_breakpoint += 1;
                            self.pending_breakpoints.push(nya_domain::Breakpoint {
                                id,
                                location: format!("{file}:{line}"),
                                enabled: true,
                                file,
                                line,
                            });
                            pending_message =
                                Some("断点已记录，连接目标并暂停后自动设置".to_owned());
                            Ok(())
                        }
                    }
                    DebugAction::RemoveBreakpoint(id) if id.starts_with("pending:") => {
                        let before = self.pending_breakpoints.len();
                        self.pending_breakpoints.retain(|bp| bp.id != id);
                        if before == self.pending_breakpoints.len() {
                            Err(nya_domain::Error("未找到待设置断点".into()))
                        } else {
                            pending_message = Some("已移除待设置断点".into());
                            Ok(())
                        }
                    }
                    DebugAction::SetBreakpointEnabled { id, enabled }
                        if id.starts_with("pending:") =>
                    {
                        if let Some(bp) = self.pending_breakpoints.iter_mut().find(|bp| bp.id == id)
                        {
                            bp.enabled = enabled;
                            pending_message = Some(
                                if enabled {
                                    "待设置断点已启用"
                                } else {
                                    "待设置断点已禁用"
                                }
                                .into(),
                            );
                            if enabled
                                && matches!(
                                    state,
                                    nya_domain::SessionState::Halted
                                        | nya_domain::SessionState::Running
                                )
                            {
                                pending_message =
                                    self.install_pending_breakpoints().or(pending_message);
                            }
                            Ok(())
                        } else {
                            Err(nya_domain::Error("未找到待设置断点".into()))
                        }
                    }
                    action => self.tools.execute_debug(action),
                };
                if result.is_ok()
                    && flush_after
                    && self.tools.debug_snapshot(&[]).state == nya_domain::SessionState::Halted
                {
                    pending_message = self.install_pending_breakpoints().or(pending_message);
                }
                let mut snapshot = self.snapshot();
                if let Err(error) = result {
                    snapshot.message = format!("操作失败：{error}");
                } else if let Some(message) = pending_message {
                    snapshot.message = message;
                }
                Ok(Event::DebugUpdated {
                    snapshot,
                    completes_command: true,
                })
            }
            Command::UpdateWatches(watches) => {
                self.watches = watches;
                Ok(Event::DebugUpdated {
                    snapshot: self.snapshot(),
                    completes_command: false,
                })
            }
            Command::RefreshTools => self.tool_event(Ok("工具列表已刷新".into())),
            Command::RefreshTargets => Ok(self.target_event(Ok("目标列表已刷新".into()))),
            Command::AddPack(path) => {
                let result = self.tools.add_pack(&path);
                Ok(self.target_event(result.map(|()| "芯片包已添加".into())))
            }
            Command::RemovePack(path) => {
                let result = self.tools.remove_pack(&path);
                Ok(self.target_event(result.map(|()| "已移除芯片包引用".into())))
            }
            Command::InstallPack(part) => {
                let result = self.tools.install_pack(part.trim());
                Ok(self.target_event(
                    result.map(|()| "芯片包安装命令已完成，请在目标列表中确认型号".into()),
                ))
            }
            Command::InstallTool { tool, version } => {
                let version = version.trim();
                let result = if version.is_empty() {
                    self.tools.install_latest(tool)
                } else {
                    self.tools.install(tool, version)
                };
                self.tool_event(
                    result.map(|()| format!("{} 安装完成，可在列表中选择使用", tool.name())),
                )
            }
            Command::SelectTool { tool, version } => {
                let result = self.tools.select(tool, &version);
                self.tool_event(
                    result.map(|()| format!("当前调试后端：{} · {version}", tool.name())),
                )
            }
            Command::UninstallTool { tool, version } => {
                let result = self.tools.uninstall(tool, &version);
                self.tool_event(result.map(|()| format!("已卸载 {} · {version}", tool.name())))
            }
            Command::AddLocalTool {
                tool,
                name,
                directory,
            } => {
                let result = self.tools.add_local(tool, name.trim(), &directory);
                self.tool_event(result.map(|()| {
                    format!(
                        "已添加本地 {} · {}，可在列表中选择使用",
                        tool.name(),
                        name.trim()
                    )
                }))
            }
            Command::RemoveLocalTool { tool, name } => {
                let result = self.tools.remove_local(tool, &name);
                self.tool_event(
                    result.map(|()| format!("已移除本地 {} · {name} 的引用", tool.name())),
                )
            }
            Command::OpenSource(path) => self.text.load(&path).map(Event::SourceOpened),
            Command::OpenFirmware(path) => self
                .firmware
                .load(&path)
                .map(|image| Event::FirmwareOpened(Box::new(image))),
            Command::ReloadFirmware(path) => self
                .firmware
                .load(&path)
                .map(|image| Event::FirmwareReloaded(Box::new(image)))
                .map_err(|error| nya_domain::Error(format!("自动刷新符号文件失败：{error}"))),
        };
        result.unwrap_or_else(|error| Event::Failed(error.to_string()))
    }

    pub fn poll(&mut self) -> Option<Event> {
        match self.tools.poll_debug() {
            Ok(false) => None,
            result => {
                let pending_message = if result.is_ok()
                    && self.tools.debug_snapshot(&[]).state == nya_domain::SessionState::Halted
                {
                    self.install_pending_breakpoints()
                } else {
                    None
                };
                let mut snapshot = self.snapshot();
                if let Err(error) = result {
                    snapshot.message = format!("调试连接异常：{error}");
                } else if let Some(message) = pending_message {
                    snapshot.message = message;
                }
                Some(Event::DebugUpdated {
                    snapshot,
                    completes_command: false,
                })
            }
        }
    }

    pub fn sample_watches(&mut self) -> Option<Event> {
        if self.watches.is_empty() {
            return None;
        }
        let mut snapshot = self.tools.sample_watches(&self.watches);
        snapshot
            .breakpoints
            .extend(self.pending_breakpoints.iter().cloned());
        if snapshot.state == nya_domain::SessionState::Running
            && !snapshot.watch_sample
            && snapshot.watch_sample_error.is_none()
        {
            return None;
        }
        if snapshot.watch_sample_error.is_some() {
            self.load_frame_source(&mut snapshot);
        }
        if !matches!(
            snapshot.state,
            nya_domain::SessionState::Halted | nya_domain::SessionState::Running
        ) && snapshot.watch_sample_error.is_none()
        {
            return None;
        }
        Some(Event::DebugUpdated {
            snapshot,
            completes_command: false,
        })
    }

    fn snapshot(&mut self) -> DebugSnapshot {
        let mut snapshot = self.tools.debug_snapshot(&self.watches);
        snapshot
            .breakpoints
            .extend(self.pending_breakpoints.iter().cloned());
        self.load_frame_source(&mut snapshot);
        snapshot
    }

    fn install_pending_breakpoints(&mut self) -> Option<String> {
        if self.tools.debug_snapshot(&[]).context_pending {
            return None;
        }
        let mut failures = Vec::new();
        let mut installed = 0;
        let mut index = 0;
        while index < self.pending_breakpoints.len() {
            let bp = &self.pending_breakpoints[index];
            if !bp.enabled {
                index += 1;
                continue;
            }
            match self.tools.execute_debug(DebugAction::AddSourceBreakpoint {
                file: bp.file.clone(),
                line: bp.line,
            }) {
                Ok(()) => {
                    self.pending_breakpoints.remove(index);
                    installed += 1;
                }
                Err(error) => {
                    failures.push(format!("{}：{}", bp.location, error));
                    index += 1;
                }
            }
        }
        if !failures.is_empty() {
            Some(format!(
                "{} 个断点设置失败，已保留：{}",
                failures.len(),
                failures.join("；")
            ))
        } else if installed > 0 {
            Some(format!("已设置 {installed} 个待处理断点"))
        } else {
            None
        }
    }

    fn load_frame_source(&self, snapshot: &mut DebugSnapshot) {
        if let Some(frame) = snapshot
            .frames
            .iter()
            .find(|f| f.level == snapshot.selected_frame)
        {
            if !frame.file.is_empty() {
                snapshot.source = self.text.load(std::path::Path::new(&frame.file)).ok();
            }
        }
    }

    fn tool_event(&self, result: Result<String>) -> Result<Event> {
        // Rescan even on failure: installers or removals may have partially succeeded.
        let installations = self.tools.discover()?;
        let backend = self.tools.active_backend()?;
        Ok(Event::ToolsUpdated {
            state: ToolState {
                installations,
                backend,
                targets: self.catalog(),
            },
            message: result.unwrap_or_else(|error| format!("操作失败：{error}")),
        })
    }

    fn catalog(&self) -> TargetCatalog {
        self.tools
            .target_catalog()
            .unwrap_or_else(|error| TargetCatalog {
                message: format!("目标加载失败：{error}"),
                ..Default::default()
            })
    }

    fn target_event(&self, result: Result<String>) -> Event {
        let mut catalog = self.catalog();
        let message = result.unwrap_or_else(|error| format!("操作失败：{error}"));
        catalog.message = format!("{message} · {}", catalog.message);
        Event::TargetsUpdated(catalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nya_domain::{Error, ToolInstallation};
    use std::path::Path;

    #[derive(Default)]
    struct Tools {
        phones: Vec<WirelessDevice>,
        collection: Option<u64>,
        state: ToolState,
        calls: Vec<String>,
        debug: DebugSnapshot,
        actions: Vec<DebugAction>,
        dirty: bool,
    }
    impl WirelessRegistry for Tools {
        fn start_wireless_acquisition(
            &mut self,
            _profile: &TargetProfile,
            _connection: &WirelessConfig,
        ) -> Result<u64> {
            self.collection = Some(42);
            Ok(42)
        }
        fn read_wireless_memory(
            &mut self,
            generation: u64,
            blocks: &[nya_domain::waveform::ReadBlock],
        ) -> Result<Vec<Vec<u8>>> {
            if self.collection != Some(generation) {
                return Err(Error("stale collection".into()));
            }
            self.calls.push("wireless:read".into());
            Ok(blocks
                .iter()
                .map(|block| vec![42; block.byte_size])
                .collect())
        }
        fn stop_wireless_acquisition(&mut self, generation: u64) -> Result<()> {
            if self.collection == Some(generation) {
                self.collection = None;
            }
            Ok(())
        }
        fn paired_wireless_devices(&self) -> Result<Vec<WirelessDevice>> {
            Ok(self.phones.clone())
        }
        fn pair_wireless(
            &mut self,
            endpoint: &str,
            name: &str,
            on_code: &mut dyn FnMut(&str),
        ) -> Result<WirelessDevice> {
            on_code("12345678");
            if endpoint == "unreachable:4242" {
                return Err(Error("phone unavailable".into()));
            }
            let device = WirelessDevice {
                id: "phone-id".into(),
                name: name.into(),
                endpoint: endpoint.into(),
            };
            self.phones = vec![device.clone()];
            Ok(device)
        }
        fn forget_wireless(&mut self, id: &str) -> Result<bool> {
            self.phones.retain(|device| device.id != id);
            Ok(false)
        }
    }
    impl ToolRegistry for Tools {
        fn discover(&self) -> Result<Vec<ToolInstallation>> {
            Ok(self.state.installations.clone())
        }
        fn active_backend(&self) -> Result<Option<ToolKind>> {
            Ok(self.state.backend)
        }
        fn install_latest(&mut self, tool: ToolKind) -> Result<()> {
            self.calls.push("latest".into());
            self.install(tool, "2.0")
        }
        fn install(&mut self, tool: ToolKind, version: &str) -> Result<()> {
            self.calls.push(version.into());
            self.state.installations.push(ToolInstallation {
                tool,
                version: version.into(),
                executable: "fixture".into(),
                selected: false,
            });
            if version == "broken" {
                Err(Error("installer failed".into()))
            } else {
                Ok(())
            }
        }
        fn select(&mut self, tool: ToolKind, version: &str) -> Result<()> {
            if !self
                .state
                .installations
                .iter()
                .any(|t| t.tool == tool && t.version == version)
            {
                return Err(Error("not installed".into()));
            }
            for installation in &mut self.state.installations {
                if installation.tool == tool {
                    installation.selected = installation.version == version;
                }
            }
            self.state.backend = Some(tool);
            Ok(())
        }
        fn uninstall(&mut self, tool: ToolKind, version: &str) -> Result<()> {
            self.state
                .installations
                .retain(|t| t.tool != tool || t.version != version);
            Ok(())
        }
    }
    impl DebugSession for Tools {
        fn read_live_memory(
            &mut self,
            blocks: &[nya_domain::waveform::ReadBlock],
        ) -> Result<Vec<Vec<u8>>> {
            self.calls.push("read-memory".into());
            Ok(blocks
                .iter()
                .map(|block| vec![42; block.byte_size])
                .collect())
        }
        fn execute_debug(&mut self, action: DebugAction) -> Result<()> {
            use nya_domain::SessionState;
            match &action {
                DebugAction::Connect { .. }
                | DebugAction::ConnectWireless { .. }
                | DebugAction::Pause => self.debug.state = SessionState::Halted,
                DebugAction::Continue => self.debug.state = SessionState::Running,
                DebugAction::Disconnect => self.debug.state = SessionState::Disconnected,
                DebugAction::AddBreakpoint(_) => return Err(Error("bad breakpoint".into())),
                DebugAction::RemoveBreakpoint(id) => {
                    self.debug.breakpoints.retain(|bp| bp.id != *id)
                }
                DebugAction::AddSourceBreakpoint { file, line } => {
                    if file == "missing.c" {
                        return Err(Error("source unavailable".into()));
                    }
                    self.debug.breakpoints.push(nya_domain::Breakpoint {
                        id: (self.debug.breakpoints.len() + 1).to_string(),
                        location: format!("{file}:{line}"),
                        enabled: true,
                        file: file.clone(),
                        line: *line,
                    });
                }
                _ => {}
            }
            self.actions.push(action);
            Ok(())
        }
        fn poll_debug(&mut self) -> Result<bool> {
            Ok(std::mem::take(&mut self.dirty))
        }
        fn sample_watches(&mut self, watches: &[WatchRequest]) -> DebugSnapshot {
            let mut snapshot = self.debug_snapshot(watches);
            snapshot.watch_sample = true;
            snapshot
        }
        fn debug_snapshot(&mut self, watches: &[WatchRequest]) -> DebugSnapshot {
            let mut snapshot = self.debug.clone();
            if snapshot.state == nya_domain::SessionState::Halted {
                snapshot.values = watches
                    .iter()
                    .map(|w| nya_domain::EvaluatedWatch {
                        id: w.id.clone(),
                        value: nya_domain::WatchValue {
                            expression: w.expression.clone(),
                            type_name: "int".into(),
                            display_value: "42".into(),
                            error: None,
                        },
                    })
                    .collect();
            }
            snapshot
        }
    }
    struct NoFiles;
    impl TextBackend for NoFiles {
        fn load(&self, _: &Path) -> Result<Document> {
            unreachable!()
        }
        fn save(&self, _: &Document) -> Result<()> {
            unreachable!()
        }
    }
    impl FirmwareBackend for NoFiles {
        fn load(&self, _: &Path) -> Result<FirmwareImage> {
            unreachable!()
        }
    }
    fn state(event: Event) -> (ToolState, String) {
        match event {
            Event::ToolsUpdated { state, message } => (state, message),
            _ => panic!("expected tools snapshot"),
        }
    }

    fn debug(event: Event) -> DebugSnapshot {
        match event {
            Event::DebugUpdated { snapshot, .. } => snapshot,
            _ => panic!("expected debug snapshot"),
        }
    }

    #[test]
    fn queued_breakpoints_do_not_delay_the_initial_stop_update() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        app.execute(Command::Debug(DebugAction::AddSourceBreakpoint {
            file: "main.c".into(),
            line: 12,
        }));
        app.tools.debug.state = nya_domain::SessionState::Halted;
        app.tools.debug.context_pending = true;
        app.tools.dirty = true;
        let stopped = debug(app.poll().unwrap());
        assert!(stopped.context_pending);
        assert!(stopped.breakpoints[0].id.starts_with("pending:"));
        assert!(app.tools.actions.is_empty());
        app.tools.debug.context_pending = false;
        app.tools.dirty = true;
        let refreshed = debug(app.poll().unwrap());
        assert!(!refreshed.context_pending);
        assert_eq!(refreshed.breakpoints[0].id, "1");
        assert_eq!(app.tools.actions.len(), 1);
    }

    #[test]
    fn source_breakpoints_can_be_queued_managed_and_installed_after_connect() {
        use nya_domain::{SessionState, TargetProfile};
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let add = |file: &str, line| {
            Command::Debug(DebugAction::AddSourceBreakpoint {
                file: file.into(),
                line,
            })
        };
        let snapshot = debug(app.execute(add("main.c", 12)));
        assert_eq!(snapshot.breakpoints.len(), 1);
        assert!(snapshot.breakpoints[0].id.starts_with("pending:"));
        assert!(app.tools.actions.is_empty());
        let id = snapshot.breakpoints[0].id.clone();
        let snapshot = debug(
            app.execute(Command::Debug(DebugAction::SetBreakpointEnabled {
                id: id.clone(),
                enabled: false,
            })),
        );
        assert!(!snapshot.breakpoints[0].enabled);
        let profile = TargetProfile {
            name: "fixture".into(),
            tool: ToolKind::OpenOcd,
            target: "chip".into(),
            interface: None,
            transport: None,
            gdb_port: 3333,
            gdb_executable: "gdb".into(),
            firmware: "fixture.elf".into(),
        };
        let snapshot = debug(app.execute(Command::Debug(DebugAction::Connect {
            profile,
            external_server: true,
            reset: false,
        })));
        assert_eq!(snapshot.state, SessionState::Halted);
        assert_eq!(snapshot.breakpoints[0].id, id);
        let snapshot = debug(
            app.execute(Command::Debug(DebugAction::SetBreakpointEnabled {
                id,
                enabled: true,
            })),
        );
        assert_eq!(snapshot.breakpoints.len(), 1);
        assert_eq!(snapshot.breakpoints[0].id, "1");
        assert_eq!(snapshot.breakpoints[0].line, 12);

        app.tools.debug.state = SessionState::Running;
        let installed = debug(app.execute(add("other.c", 7)))
            .breakpoints
            .pop()
            .unwrap();
        assert!(!installed.id.starts_with("pending:"));
        assert_eq!(app.tools.debug.state, SessionState::Running);
        assert!(matches!(
            app.tools.actions.last(),
            Some(DebugAction::AddSourceBreakpoint { line: 7, .. })
        ));
        let snapshot =
            debug(app.execute(Command::Debug(DebugAction::RemoveBreakpoint(installed.id))));
        assert_eq!(snapshot.breakpoints.len(), 1);
        app.tools.debug.state = SessionState::Disconnected;
        let pending = debug(app.execute(add("missing.c", 8)))
            .breakpoints
            .pop()
            .unwrap();
        app.tools.debug.state = SessionState::Halted;
        app.tools.dirty = true;
        let snapshot = debug(app.poll().unwrap());
        assert!(snapshot.message.contains("source unavailable"));
        assert!(snapshot.breakpoints.iter().any(|bp| bp.id == pending.id));
    }

    #[test]
    fn live_memory_requests_reply_directly_and_expired_requests_never_touch_target() {
        use nya_domain::waveform::ReadBlock;
        use std::{sync::mpsc, time::Duration};
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let block = ReadBlock {
            address: 0x20000000,
            byte_size: 4,
            little_endian: true,
        };
        let (response, receiver) = mpsc::sync_channel(1);
        assert!(matches!(
            app.execute(Command::ReadLiveMemory(MemoryReadRequest {
                blocks: vec![block.clone()],
                deadline: Instant::now() - Duration::from_secs(1),
                response,
            })),
            Event::MemoryRead
        ));
        assert!(receiver.recv().unwrap().is_err());
        assert!(app.tools.calls.is_empty());
        let (response, receiver) = mpsc::sync_channel(1);
        assert!(matches!(
            app.execute(Command::ReadLiveMemory(MemoryReadRequest {
                blocks: vec![block],
                deadline: Instant::now() + Duration::from_secs(1),
                response,
            })),
            Event::MemoryRead
        ));
        assert_eq!(receiver.recv().unwrap().unwrap(), [vec![42; 4]]);
        assert_eq!(app.tools.calls, ["read-memory"]);
        assert!(app.tools.actions.is_empty());
    }

    #[test]
    fn periodic_samples_do_not_complete_commands_or_reload_source() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        assert!(app.sample_watches().is_none());
        app.execute(Command::UpdateWatches(vec![WatchRequest {
            id: "watch-1".into(),
            expression: "counter".into(),
            function: None,
            address: None,
        }]));
        assert!(app.sample_watches().is_none());
        app.tools.debug.state = nya_domain::SessionState::Halted;
        match app.sample_watches().unwrap() {
            Event::DebugUpdated {
                snapshot,
                completes_command,
            } => {
                assert!(!completes_command);
                assert!(snapshot.watch_sample);
                assert_eq!(snapshot.values[0].id, "watch-1");
                assert!(snapshot.source.is_none());
            }
            _ => panic!("expected live snapshot"),
        }
        app.execute(Command::UpdateWatches(vec![]));
        assert!(app.sample_watches().is_none());
    }

    #[test]
    fn install_select_refresh_and_uninstall_return_current_state() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let (installed, _) = state(app.execute(Command::InstallTool {
            tool: ToolKind::PyOcd,
            version: " 1.0 ".into(),
        }));
        assert_eq!(installed.installations[0].version, "1.0");
        assert_eq!(installed.backend, None);
        let (selected, _) = state(app.execute(Command::SelectTool {
            tool: ToolKind::PyOcd,
            version: "1.0".into(),
        }));
        assert_eq!(selected.backend, Some(ToolKind::PyOcd));
        assert!(selected.installations[0].selected);
        let (refreshed, _) = state(app.execute(Command::RefreshTools));
        assert_eq!(refreshed.backend, selected.backend);
        let (removed, _) = state(app.execute(Command::UninstallTool {
            tool: ToolKind::PyOcd,
            version: "1.0".into(),
        }));
        assert!(removed.installations.is_empty());
    }

    #[test]
    fn blank_version_installs_latest_and_failures_rescan_partial_changes() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let (installed, _) = state(app.execute(Command::InstallTool {
            tool: ToolKind::OpenOcd,
            version: " ".into(),
        }));
        assert_eq!(installed.installations[0].version, "2.0");
        assert_eq!(app.tools.calls, ["latest", "2.0"]);
        let (partial, message) = state(app.execute(Command::InstallTool {
            tool: ToolKind::ProbeRs,
            version: "broken".into(),
        }));
        assert_eq!(partial.installations.len(), 2);
        assert!(message.contains("installer failed"));
        let (unchanged, message) = state(app.execute(Command::SelectTool {
            tool: ToolKind::PyOcd,
            version: "9.0".into(),
        }));
        assert_eq!(unchanged.backend, None);
        assert!(message.contains("not installed"));
    }
    #[test]
    fn standalone_flash_requires_and_uses_selected_backend_without_gdb() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let action = DebugAction::Flash {
            profile: nya_domain::TargetProfile {
                name: "fixture".into(),
                tool: ToolKind::OpenOcd,
                target: "chip".into(),
                interface: None,
                transport: None,
                gdb_port: 3333,
                gdb_executable: PathBuf::new(),
                firmware: "fixture.elf".into(),
            },
        };
        assert!(matches!(
            app.execute(Command::Debug(action.clone())),
            Event::Failed(_)
        ));
        assert!(app.tools.actions.is_empty());
        app.tools.state.backend = Some(ToolKind::PyOcd);
        assert!(matches!(
            app.execute(Command::Debug(action)),
            Event::DebugUpdated {
                completes_command: true,
                ..
            }
        ));
        assert!(matches!(
            &app.tools.actions[..],
            [DebugAction::Flash { profile }] if profile.tool == ToolKind::PyOcd
        ));
    }

    #[test]
    fn native_collection_uses_its_own_generation_and_expired_requests_skip_hardware() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let profile = TargetProfile {
            name: "phone".into(),
            tool: ToolKind::ProbeRs,
            target: "chip".into(),
            interface: None,
            transport: None,
            gdb_port: 3333,
            gdb_executable: "".into(),
            firmware: "firmware.axf".into(),
        };
        assert!(matches!(
            app.execute(Command::StartWirelessAcquisition {
                profile,
                connection: WirelessConfig::new("phone")
            }),
            Event::WirelessAcquisitionStarted(42)
        ));
        for (generation, expired, expected) in
            [(42, false, true), (41, false, false), (42, true, false)]
        {
            let (response, receiver) = std::sync::mpsc::sync_channel(1);
            let request = MemoryReadRequest {
                blocks: vec![nya_domain::waveform::ReadBlock {
                    address: 0x20000000,
                    byte_size: 4,
                    little_endian: true,
                }],
                deadline: if expired {
                    Instant::now()
                } else {
                    Instant::now() + std::time::Duration::from_secs(1)
                },
                response,
            };
            assert!(matches!(
                app.execute(Command::ReadWirelessMemory {
                    generation,
                    request
                }),
                Event::MemoryRead
            ));
            assert_eq!(receiver.recv().unwrap().is_ok(), expected);
        }
        assert_eq!(app.tools.calls, ["wireless:read"]);
        assert!(matches!(
            app.execute(Command::StopWirelessAcquisition(41)),
            Event::WirelessAcquisitionStopped { generation: 41, .. }
        ));
        assert_eq!(app.tools.collection, Some(42));
        app.execute(Command::StopWirelessAcquisition(42));
        assert!(app.tools.collection.is_none());
    }

    #[test]
    fn pairing_delivers_comparison_code_and_returns_selected_phone_and_offline_forget() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let event = app.execute(Command::PairWireless {
            endpoint: "phone:4242".into(),
            computer_name: "computer".into(),
            comparison: sender,
        });
        assert_eq!(receiver.recv().unwrap(), "12345678");
        match event {
            Event::WirelessDevicesUpdated {
                devices,
                selected_id,
                ..
            } => {
                assert_eq!(devices.len(), 1);
                assert_eq!(selected_id.as_deref(), Some("phone-id"));
            }
            _ => panic!("pairing must refresh devices and select the new phone"),
        }
        match app.execute(Command::ForgetWireless("phone-id".into())) {
            Event::WirelessDevicesUpdated {
                devices,
                selected_id,
                message,
            } => {
                assert!(devices.is_empty());
                assert_eq!(selected_id.as_deref(), Some(""));
                assert!(message.contains("另行删除"));
            }
            _ => panic!("offline forget must still update the desktop"),
        }
    }

    #[test]
    fn pairing_failure_is_reported_and_closed_comparison_receiver_never_blocks() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        drop(receiver);
        assert!(matches!(
            app.execute(Command::PairWireless {
                endpoint: "unreachable:4242".into(),
                computer_name: "computer".into(),
                comparison: sender
            }),
            Event::Failed(_)
        ));
        assert!(app.tools.phones.is_empty());
    }

    #[test]
    fn wireless_actions_bypass_local_tool_selection_and_install_pending_breakpoints() {
        use nya_domain::{TargetProfile, WirelessConfig};
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        app.execute(Command::Debug(DebugAction::AddSourceBreakpoint {
            file: "main.c".into(),
            line: 8,
        }));
        let profile = TargetProfile {
            name: "wireless".into(),
            tool: ToolKind::ProbeRs,
            target: "chip".into(),
            interface: None,
            transport: None,
            gdb_port: 3333,
            gdb_executable: "gdb".into(),
            firmware: "firmware.axf".into(),
        };
        let connection = WirelessConfig::new("phone");
        let event = app.execute(Command::Debug(DebugAction::ConnectWireless {
            profile: profile.clone(),
            connection: connection.clone(),
            reset: false,
        }));
        match event {
            Event::DebugUpdated { snapshot, .. } => {
                assert_eq!(snapshot.state, nya_domain::SessionState::Halted);
                assert_eq!(snapshot.breakpoints.len(), 1);
                assert_eq!(snapshot.breakpoints[0].line, 8);
            }
            _ => panic!("wireless connection must not require a local tool"),
        }
        assert!(
            matches!(&app.tools.actions[0], DebugAction::ConnectWireless { profile, .. } if profile.tool == ToolKind::ProbeRs)
        );
        app.execute(Command::Debug(DebugAction::Disconnect));
        app.execute(Command::Debug(DebugAction::FlashWireless {
            profile,
            connection,
        }));
        assert!(matches!(
            app.tools.actions.last(),
            Some(DebugAction::FlashWireless { .. })
        ));
        assert!(app.tools.state.backend.is_none());
    }

    #[test]
    fn session_uses_selected_backend_and_async_events_do_not_complete_commands() {
        use nya_domain::{SessionState, TargetProfile};
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let profile = TargetProfile {
            name: "fixture".into(),
            tool: ToolKind::OpenOcd,
            target: "chip".into(),
            interface: None,
            transport: None,
            gdb_port: 3333,
            gdb_executable: "gdb".into(),
            firmware: "fixture.elf".into(),
        };
        assert!(matches!(
            app.execute(Command::Debug(DebugAction::Connect {
                profile: profile.clone(),
                external_server: false,
                reset: false,
            })),
            Event::Failed(_)
        ));
        assert!(app.tools.actions.is_empty());
        app.tools.state.backend = Some(ToolKind::ProbeRs);
        match app.execute(Command::Debug(DebugAction::Connect {
            profile,
            external_server: false,
            reset: false,
        })) {
            Event::DebugUpdated {
                snapshot,
                completes_command,
            } => {
                assert_eq!(snapshot.state, SessionState::Halted);
                assert!(completes_command);
            }
            _ => panic!("expected debug state"),
        }
        assert!(
            matches!(&app.tools.actions[0], DebugAction::Connect { profile, .. } if profile.tool == ToolKind::ProbeRs)
        );
        let request = WatchRequest {
            id: "revision:0:0".into(),
            expression: "counter".into(),
            function: None,
            address: None,
        };
        match app.execute(Command::UpdateWatches(vec![request])) {
            Event::DebugUpdated {
                snapshot,
                completes_command,
            } => {
                assert!(!completes_command);
                assert_eq!(snapshot.values[0].id, "revision:0:0");
            }
            _ => panic!("expected watches"),
        }
        match app.execute(Command::Debug(DebugAction::Continue)) {
            Event::DebugUpdated { snapshot, .. } => {
                assert_eq!(snapshot.state, SessionState::Running);
                assert!(snapshot.values.is_empty());
            }
            _ => panic!("expected running state"),
        }
        assert!(app.poll().is_none());
        app.tools.debug.state = SessionState::Halted;
        app.tools.dirty = true;
        match app.poll().unwrap() {
            Event::DebugUpdated {
                snapshot,
                completes_command,
            } => {
                assert!(!completes_command);
                assert_eq!(snapshot.values[0].value.display_value, "42");
            }
            _ => panic!("expected async stop"),
        }
        match app.execute(Command::Debug(DebugAction::AddBreakpoint("invalid".into()))) {
            Event::DebugUpdated { snapshot, .. } => {
                assert_eq!(snapshot.state, SessionState::Halted);
                assert!(snapshot.message.contains("bad breakpoint"));
            }
            _ => panic!("expected recoverable error"),
        }
    }
}
