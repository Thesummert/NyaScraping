mod mi;
mod session;
// All NyaScrapingCore-specific types stay inside this adapter.
use nya_domain::{
    DebugBackend, Error, Result, TargetProfile, ToolInstallation, ToolKind, ToolRegistry,
};
use nya_scraping_core::{
    backend::{operation_command, GdbServerOptions, Operation},
    gdbserver::{GdbServer, RunningGdbServer},
    manager::{Manager, Tool},
};
use std::{fs, time::Duration};

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
        port: profile.gdb_port,
    }
}

pub struct DebugAdapter {
    manager: Manager,
    server: Option<RunningGdbServer>,
    session: session::Session,
}

impl DebugAdapter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            manager: Manager::new().map_err(error)?,
            server: None,
            session: session::Session::default(),
        })
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
        let status = command.status().map_err(error)?;
        if status.success() {
            Ok(())
        } else {
            Err(Error(format!(
                "{} exited with {status}",
                profile.tool.name()
            )))
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
        self.manager.install_latest(core_tool(tool)).map_err(error)
    }

    fn install(&mut self, tool: ToolKind, version: &str) -> Result<()> {
        self.manager
            .install(core_tool(tool), version)
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
        self.server = Some(server.start(Duration::from_secs(10)).map_err(error)?);
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
