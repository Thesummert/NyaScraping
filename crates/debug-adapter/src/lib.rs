//! All NyaScrapingCore-specific types stay inside this adapter.
use nya_domain::{
    DebugBackend, Error, Result, TargetProfile, ToolInstallation, ToolKind, ToolRegistry,
};
use nya_scraping_core::{
    backend::{operation_command, GdbServerOptions, Operation},
    gdbserver::{GdbServer, RunningGdbServer},
    manager::{Manager, Tool},
};
use std::time::Duration;

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
}

impl DebugAdapter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            manager: Manager::new().map_err(error)?,
            server: None,
        })
    }

    fn operation(&mut self, profile: &TargetProfile, operation: Operation) -> Result<()> {
        if self.server.is_some() {
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

    fn install(&mut self, tool: ToolKind, version: &str) -> Result<()> {
        self.manager
            .install(core_tool(tool), version)
            .map_err(error)
    }

    fn select(&mut self, tool: ToolKind, version: &str) -> Result<()> {
        self.manager.select(core_tool(tool), version).map_err(error)
    }

    fn uninstall(&mut self, tool: ToolKind, version: &str) -> Result<()> {
        if self.server.is_some() {
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
        if let Some(server) = self.server.as_mut() {
            server.stop().map_err(error)?;
        }
        self.server = None;
        Ok(())
    }

    fn flash(&mut self, profile: &TargetProfile) -> Result<()> {
        self.operation(profile, Operation::Flash(profile.firmware.clone()))
    }

    fn reset(&mut self, profile: &TargetProfile) -> Result<()> {
        self.operation(profile, Operation::Reset)
    }
}
