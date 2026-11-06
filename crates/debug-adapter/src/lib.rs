pub use nya_scraping_core::manager::{InstallProgress, SharedInstallProgress};
mod diagnostics;
pub mod live_memory;
mod mi;
mod non_stop;
mod session;
mod targets;
// All NyaScrapingCore-specific types stay inside this adapter.
use nya_domain::{
    DebugBackend, Error, Result, TargetProfile, ToolInstallation, ToolKind, ToolRegistry,
};
use nya_scraping_core::{
    backend::{operation_command, GdbServerOptions, Operation},
    gdbserver::{GdbServer, RunningGdbServer},
    manager::{Manager, Tool},
};
use std::{
    fs,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

fn error(error: impl std::fmt::Display) -> Error {
    Error(error.to_string())
}

fn core_tool(tool: ToolKind) -> Tool {
    match tool {
        ToolKind::OpenOcd => Tool::OpenOcd,
        ToolKind::PyOcd => Tool::PyOcd,
        ToolKind::ProbeRs => Tool::ProbeRs,
    }
}

fn options(profile: &TargetProfile) -> GdbServerOptions {
    GdbServerOptions {
        target: profile.target.clone(),
        interface: profile.interface.clone(),
        transport: profile.transport.clone(),
        port: profile.gdb_port,
    }
}

pub struct DebugAdapter {
    manager: Manager,
    install_cancel: Arc<AtomicBool>,
    install_progress: SharedInstallProgress,
    server: Option<RunningGdbServer>,
    session: session::Session,
}

impl DebugAdapter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            manager: Manager::new().map_err(error)?,
            install_cancel: Arc::new(AtomicBool::new(false)),
            install_progress: Default::default(),
            server: None,
            session: session::Session::default(),
        })
    }

    pub fn set_install_cancel(&mut self, cancel: Arc<AtomicBool>) {
        self.install_cancel = cancel;
    }

    pub fn set_install_progress(&mut self, progress: SharedInstallProgress) {
        self.install_progress = progress;
    }

    fn operation(&mut self, profile: &TargetProfile, operation: Operation) -> Result<()> {
        if self.server.is_some() || self.session.client.is_some() {
            return Err(Error(
                "Disconnect the GDB server before a maintenance operation".into(),
            ));
        }
        let tool = core_tool(profile.tool);
        let executable = self.manager.resolve(tool).map_err(error)?;
        let mut command =
            operation_command(tool, &executable, &options(profile), &operation).map_err(error)?;
        self.configure_packs(profile.tool, &mut command)?;
        let mut diagnostics = diagnostics::Diagnostics::capture(&mut command)?;
        let status = command.status().map_err(error)?;
        if status.success() {
            Ok(())
        } else {
            Err(diagnostics.failure(format!("{} exited with {status}", profile.tool.name())))
        }
    }
}

impl ToolRegistry for DebugAdapter {
    fn discover(&self) -> Result<Vec<ToolInstallation>> {
        let mut installations = Vec::new();
        for kind in [ToolKind::OpenOcd, ToolKind::PyOcd, ToolKind::ProbeRs] {
            let tool = core_tool(kind);
            let selected = self.manager.selected(tool).map_err(error)?;
            for (version, executable) in self.manager.installed(tool).map_err(error)? {
                let active = selected.as_deref() == Some(version.as_str());
                installations.push(ToolInstallation {
                    tool: kind,
                    version,
                    executable,
                    selected: active,
                });
            }
            for (name, executable) in self.manager.local(tool).map_err(error)? {
                let version = format!("local:{name}");
                installations.push(ToolInstallation {
                    tool: kind,
                    selected: selected.as_deref() == Some(version.as_str()),
                    version,
                    executable,
                });
            }
            for (index, executable) in self.manager.system(tool).into_iter().enumerate() {
                installations.push(ToolInstallation {
                    tool: kind,
                    version: "system".into(),
                    executable,
                    selected: index == 0 && matches!(selected.as_deref(), None | Some("system")),
                });
            }
        }
        Ok(installations)
    }

    fn active_backend(&self) -> Result<Option<ToolKind>> {
        let path = self.manager.config_path.with_file_name("desktop-backend");
        let value = match fs::read_to_string(path) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(error(e)),
        };
        match value.trim() {
            "openocd" => Ok(Some(ToolKind::OpenOcd)),
            "pyocd" => Ok(Some(ToolKind::PyOcd)),
            "probe-rs" => Ok(Some(ToolKind::ProbeRs)),
            other => Err(Error(format!("无效的调试后端配置：{other}"))),
        }
    }

    fn install_latest(&mut self, tool: ToolKind) -> Result<()> {
        self.manager
            .install_latest_with_progress(
                core_tool(tool),
                &self.install_cancel,
                &self.install_progress,
            )
            .map_err(error)
    }

    fn install(&mut self, tool: ToolKind, version: &str) -> Result<()> {
        self.manager
            .install_with_progress(
                core_tool(tool),
                version,
                &self.install_cancel,
                &self.install_progress,
            )
            .map_err(error)
    }

    fn select(&mut self, tool: ToolKind, version: &str) -> Result<()> {
        if self.server.is_some() || self.session.client.is_some() {
            return Err(Error(
                "Disconnect the GDB server before switching a backend".into(),
            ));
        }
        self.manager
            .select(core_tool(tool), version)
            .map_err(error)?;
        fs::write(
            self.manager.config_path.with_file_name("desktop-backend"),
            core_tool(tool).to_string(),
        )
        .map_err(error)
    }

    fn uninstall(&mut self, tool: ToolKind, version: &str) -> Result<()> {
        if self.server.is_some() || self.session.client.is_some() {
            return Err(Error(
                "Disconnect the GDB server before uninstalling a tool".into(),
            ));
        }
        self.manager
            .uninstall(core_tool(tool), version)
            .map_err(error)
    }

    fn add_local(&mut self, tool: ToolKind, name: &str, directory: &std::path::Path) -> Result<()> {
        self.manager
            .add_local(core_tool(tool), name, directory)
            .map_err(error)
    }

    fn remove_local(&mut self, tool: ToolKind, name: &str) -> Result<()> {
        if self.server.is_some() || self.session.client.is_some() {
            return Err(Error(
                "Disconnect the GDB server before removing a local tool".into(),
            ));
        }
        self.manager
            .remove_local(core_tool(tool), name)
            .map_err(error)
    }

    fn target_catalog(&self) -> Result<nya_domain::TargetCatalog> {
        self.load_targets()
    }

    fn add_pack(&mut self, path: &std::path::Path) -> Result<()> {
        self.add_local_pack(path)
    }

    fn remove_pack(&mut self, path: &std::path::Path) -> Result<()> {
        self.remove_local_pack(path)
    }

    fn install_pack(&mut self, part: &str) -> Result<()> {
        self.install_target_pack(part)
    }
}

impl DebugBackend for DebugAdapter {
    fn start_server(&mut self, profile: &TargetProfile) -> Result<()> {
        if self.server.is_some() {
            return Err(Error(
                "A GDB server is already owned by this adapter".into(),
            ));
        }
        let server = GdbServer::resolve(&self.manager, core_tool(profile.tool), options(profile))
            .map_err(error)?;
        let mut command = server.command().map_err(error)?;
        if profile.tool == ToolKind::PyOcd {
            command.args(["-O", "cache.enable_memory=false"]);
        }
        self.configure_packs(profile.tool, &mut command)?;
        let mut diagnostics = diagnostics::Diagnostics::capture(&mut command)?;
        self.server = Some(
            RunningGdbServer::start(command, profile.gdb_port, Duration::from_secs(10))
                .map_err(|error| diagnostics.failure(error))?,
        );
        Ok(())
    }

    fn stop_server(&mut self) -> Result<()> {
        if let Some(mut server) = self.server.take() {
            server.stop().map_err(error)?;
        }
        Ok(())
    }

    fn flash(&mut self, profile: &TargetProfile) -> Result<()> {
        self.operation(profile, Operation::Flash(profile.firmware.clone()))
    }

    fn reset(&mut self, profile: &TargetProfile) -> Result<()> {
        self.operation(profile, Operation::Reset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("nya-adapter-{}-{stamp}", std::process::id())))
        }
        fn adapter(&self) -> DebugAdapter {
            DebugAdapter {
                manager: Manager {
                    config_path: self.0.join("config/config"),
                    data_dir: self.0.join("data"),
                },
                install_cancel: Default::default(),
                install_progress: Default::default(),
                server: None,
                session: session::Session::default(),
            }
        }
        fn add_pyocd(&self, version: &str) -> PathBuf {
            let executable = self
                .0
                .join("data/pyocd")
                .join(version)
                .join(if cfg!(windows) {
                    "Scripts/pyocd.exe"
                } else {
                    "bin/pyocd"
                });
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(&executable, "fixture").unwrap();
            executable
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[cfg(unix)]
    fn standalone_flash_resets_without_gdb_and_reports_failures() {
        use nya_domain::{DebugAction, DebugSession, SessionState};
        use std::os::unix::fs::PermissionsExt;

        for tool in [ToolKind::OpenOcd, ToolKind::PyOcd, ToolKind::ProbeRs] {
            for failure in ["", "flash", "reset"] {
                // OpenOCD resets as part of its single program command.
                if tool == ToolKind::OpenOcd && failure == "reset" {
                    continue;
                }
                let fixture = Fixture::new();
                let mut adapter = fixture.adapter();
                let directory = fixture.0.join("probe");
                fs::create_dir_all(&directory).unwrap();
                let executable = directory.join(core_tool(tool).to_string());
                fs::write(
                    &executable,
                    r##"#!/bin/sh
printf '%s\n' "$*" >> "$0.calls"
if [ -f "$0.fail-flash" ] && [ "$1" != reset ]; then
    echo 'fixture flash failure' >&2
    exit 1
fi
if [ -f "$0.fail-reset" ] && [ "$1" = reset ]; then
    echo 'fixture reset failure' >&2
    exit 1
fi
"##,
                )
                .unwrap();
                fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
                adapter.add_local(tool, "fixture", &directory).unwrap();
                adapter.select(tool, "local:fixture").unwrap();
                if !failure.is_empty() {
                    fs::write(executable.with_extension(format!("fail-{failure}")), "").unwrap();
                }
                let firmware = fixture.0.join("firmware with spaces.elf");
                fs::write(&firmware, "fixture").unwrap();
                let result = adapter.execute_debug(DebugAction::Flash {
                    profile: TargetProfile {
                        name: "fixture".into(),
                        tool,
                        target: "chip".into(),
                        interface: (tool == ToolKind::OpenOcd).then(|| "probe.cfg".into()),
                        transport: None,
                        gdb_port: 3333,
                        gdb_executable: fixture.0.join("gdb-does-not-exist"),
                        firmware,
                    },
                });
                let calls = fs::read_to_string(executable.with_extension("calls")).unwrap();
                let calls: Vec<_> = calls.lines().collect();
                let progress = adapter.install_progress.lock().unwrap().clone();
                if failure.is_empty() {
                    result.unwrap();
                    assert_eq!(progress.percent, Some(100.0));
                    assert!(adapter.debug_snapshot(&[]).message.contains("自动复位"));
                } else {
                    let error = result.unwrap_err();
                    assert!(error.0.contains(&format!("fixture {failure} failure")));
                    assert_ne!(progress.percent, Some(100.0));
                    if failure == "reset" {
                        assert!(error.0.contains("固件已写入，但复位失败"));
                    }
                }
                if tool == ToolKind::OpenOcd {
                    assert_eq!(calls.len(), 1);
                    assert!(calls[0].contains("verify reset exit"));
                } else if failure == "flash" {
                    assert_eq!(calls.len(), 1);
                } else {
                    assert_eq!(calls.len(), 2);
                    assert!(calls[1].starts_with("reset "));
                }
                assert!(adapter.server.is_none());
                assert!(adapter.session.client.is_none());
                assert_eq!(
                    adapter.debug_snapshot(&[]).state,
                    SessionState::Disconnected
                );
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn failed_openocd_connection_and_flash_include_probe_diagnostics() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let mut adapter = fixture.adapter();
        let executable = adapter
            .manager
            .data_dir
            .join("openocd/1.0/xpack-openocd-1.0/bin/openocd");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(
            &executable,
            "#!/bin/sh\nprintf 'Error: Selected transport is not supported by the device\\n' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        adapter.manager.select(Tool::OpenOcd, "1.0").unwrap();
        let firmware = fixture.0.join("firmware.elf");
        fs::write(&firmware, "fixture").unwrap();
        let profile = TargetProfile {
            name: "J-Link".into(),
            tool: ToolKind::OpenOcd,
            target: "target/stm32h7x.cfg".into(),
            interface: Some("interface/jlink.cfg".into()),
            transport: None,
            gdb_port: std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port(),
            gdb_executable: "gdb".into(),
            firmware,
        };
        let connection_error = adapter.start_server(&profile).unwrap_err();
        assert!(connection_error
            .0
            .contains("Selected transport is not supported"));
        assert!(adapter.server.is_none());
        let flash_error = adapter.flash(&profile).unwrap_err();
        assert!(flash_error
            .0
            .contains("Selected transport is not supported"));
    }

    #[test]
    fn local_tools_can_be_discovered_selected_and_unregistered() {
        let fixture = Fixture::new();
        let mut adapter = fixture.adapter();
        let directory = fixture.0.join("custom/bin");
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(if cfg!(windows) { "pyocd.exe" } else { "pyocd" });
        fs::write(&executable, b"fixture").unwrap();
        let executable = fs::canonicalize(executable).unwrap();
        adapter
            .add_local(ToolKind::PyOcd, "custom", &fixture.0.join("custom"))
            .unwrap();
        assert!(adapter
            .discover()
            .unwrap()
            .iter()
            .any(|item| item.version == "local:custom"
                && item.executable == executable
                && !item.selected));
        adapter.select(ToolKind::PyOcd, "local:custom").unwrap();
        let mut reopened = fixture.adapter();
        assert!(reopened
            .discover()
            .unwrap()
            .iter()
            .any(|item| item.version == "local:custom" && item.selected));
        assert_eq!(reopened.manager.resolve(Tool::PyOcd).unwrap(), executable);
        reopened.remove_local(ToolKind::PyOcd, "custom").unwrap();
        assert!(!reopened
            .discover()
            .unwrap()
            .iter()
            .any(|item| item.version == "local:custom"));
        assert!(executable.exists());
    }

    #[test]
    fn backend_and_version_survive_reopening_and_invalid_selection() {
        let fixture = Fixture::new();
        let executable = fixture.add_pyocd("1.0");
        fixture.add_pyocd("2.0");
        let mut adapter = fixture.adapter();
        assert_eq!(adapter.active_backend().unwrap(), None);
        adapter.select(ToolKind::PyOcd, "1.0").unwrap();
        let mut reopened = fixture.adapter();
        assert_eq!(reopened.active_backend().unwrap(), Some(ToolKind::PyOcd));
        assert_eq!(reopened.manager.resolve(Tool::PyOcd).unwrap(), executable);
        assert!(reopened.select(ToolKind::PyOcd, "9.0").is_err());
        assert!(reopened.select(ToolKind::OpenOcd, "../config").is_err());
        assert_eq!(reopened.manager.resolve(Tool::PyOcd).unwrap(), executable);
        assert_eq!(reopened.active_backend().unwrap(), Some(ToolKind::PyOcd));
        reopened.select(ToolKind::PyOcd, "2.0").unwrap();
        let discovered = reopened.discover().unwrap();
        assert!(discovered.iter().any(|t| t.version == "2.0" && t.selected));
        assert!(!discovered.iter().any(|t| t.version == "1.0" && t.selected));
    }

    #[test]
    fn uninstall_clears_version_and_never_removes_system_tools() {
        let fixture = Fixture::new();
        let executable = fixture.add_pyocd("1.0");
        let mut adapter = fixture.adapter();
        adapter.select(ToolKind::PyOcd, "1.0").unwrap();
        assert!(adapter.uninstall(ToolKind::PyOcd, "system").is_err());
        assert!(executable.exists());
        adapter.uninstall(ToolKind::PyOcd, "1.0").unwrap();
        assert!(!executable.exists());
        assert_eq!(adapter.manager.selected(Tool::PyOcd).unwrap(), None);
        assert_eq!(adapter.active_backend().unwrap(), Some(ToolKind::PyOcd));
        assert!(!adapter
            .discover()
            .unwrap()
            .iter()
            .any(|t| t.version == "1.0"));
    }
}
