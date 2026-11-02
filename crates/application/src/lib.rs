//! Use cases run on a worker; UI callbacks submit commands and consume events.
use nya_domain::{
    firmware::{FirmwareBackend, FirmwareImage},
    DebugAction, DebugSession, DebugSnapshot, Document, Result, TextBackend, ToolKind,
    ToolRegistry, ToolState, WatchRequest,
};
use std::path::PathBuf;

pub enum Command {
    RefreshTools,
    Debug(DebugAction),
    UpdateWatches(Vec<WatchRequest>),
    InstallTool { tool: ToolKind, version: String },
    SelectTool { tool: ToolKind, version: String },
    UninstallTool { tool: ToolKind, version: String },
    OpenSource(PathBuf),
    OpenFirmware(PathBuf),
}

pub enum Event {
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
    Failed(String),
}

pub struct Application<T, E, F> {
    tools: T,
    text: E,
    firmware: F,
    watches: Vec<WatchRequest>,
}

impl<T: ToolRegistry + DebugSession, E: TextBackend, F: FirmwareBackend> Application<T, E, F> {
    pub fn new(tools: T, text: E, firmware: F) -> Self {
        Self {
            tools,
            text,
            firmware,
            watches: Vec::new(),
        }
    }

    pub fn execute(&mut self, command: Command) -> Event {
        let result: Result<Event> = match command {
            Command::Debug(mut action) => {
                if let DebugAction::Connect {
                    profile,
                    external_server: false,
                } = &mut action
                {
                    match self.tools.active_backend() {
                        Ok(Some(tool)) => profile.tool = tool,
                        Ok(None) => return Event::Failed("请先在调试工具页选择后端".into()),
                        Err(error) => return Event::Failed(error.to_string()),
                    }
                }
                let result = self.tools.execute_debug(action);
                let mut snapshot = self.snapshot();
                if let Err(error) = result {
                    snapshot.message = format!("操作失败：{error}");
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
            Command::OpenSource(path) => self.text.load(&path).map(Event::SourceOpened),
            Command::OpenFirmware(path) => self
                .firmware
                .load(&path)
                .map(|image| Event::FirmwareOpened(Box::new(image))),
        };
        result.unwrap_or_else(|error| Event::Failed(error.to_string()))
    }

    pub fn poll(&mut self) -> Option<Event> {
        match self.tools.poll_debug() {
            Ok(false) => None,
            result => {
                let mut snapshot = self.snapshot();
                if let Err(error) = result {
                    snapshot.message = format!("调试连接异常：{error}");
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
        self.load_frame_source(&mut snapshot);
        snapshot
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
            },
            message: result.unwrap_or_else(|error| format!("操作失败：{error}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nya_domain::{Error, ToolInstallation};
    use std::path::Path;

    #[derive(Default)]
    struct Tools {
        state: ToolState,
        calls: Vec<String>,
        debug: DebugSnapshot,
        actions: Vec<DebugAction>,
        dirty: bool,
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
        fn execute_debug(&mut self, action: DebugAction) -> Result<()> {
            use nya_domain::SessionState;
            match &action {
                DebugAction::Connect { .. } | DebugAction::Pause => {
                    self.debug.state = SessionState::Halted
                }
                DebugAction::Continue => self.debug.state = SessionState::Running,
                DebugAction::Disconnect => self.debug.state = SessionState::Disconnected,
                DebugAction::AddBreakpoint(_) => return Err(Error("bad breakpoint".into())),
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

    #[test]
    fn periodic_samples_do_not_complete_commands_or_reload_source() {
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        assert!(app.sample_watches().is_none());
        app.execute(Command::UpdateWatches(vec![WatchRequest {
            id: "watch-1".into(),
            expression: "counter".into(),
            function: None,
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
    fn session_uses_selected_backend_and_async_events_do_not_complete_commands() {
        use nya_domain::{SessionState, TargetProfile};
        let mut app = Application::new(Tools::default(), NoFiles, NoFiles);
        let profile = TargetProfile {
            name: "fixture".into(),
            tool: ToolKind::OpenOcd,
            target: "chip".into(),
            interface: None,
            gdb_port: 3333,
            gdb_executable: "gdb".into(),
            firmware: "fixture.elf".into(),
        };
        assert!(matches!(
            app.execute(Command::Debug(DebugAction::Connect {
                profile: profile.clone(),
                external_server: false
            })),
            Event::Failed(_)
        ));
        assert!(app.tools.actions.is_empty());
        app.tools.state.backend = Some(ToolKind::ProbeRs);
        match app.execute(Command::Debug(DebugAction::Connect {
            profile,
            external_server: false,
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
