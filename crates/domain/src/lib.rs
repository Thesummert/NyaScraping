//! UI-independent contracts. This crate has no Slint, process, or native dependencies.
pub mod firmware;
pub mod waveform;

use std::{fmt, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    OpenOcd,
    PyOcd,
    ProbeRs,
}

impl ToolKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenOcd => "OpenOCD",
            Self::PyOcd => "pyOCD",
            Self::ProbeRs => "probe-rs",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TargetProfile {
    pub name: String,
    pub tool: ToolKind,
    /// OpenOCD configuration file, or pyOCD / probe-rs chip name.
    pub target: String,
    pub interface: Option<String>,
    /// Optional OpenOCD transport override; None uses the probe's default.
    pub transport: Option<String>,
    pub gdb_port: u16,
    pub gdb_executable: PathBuf,
    pub firmware: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ToolInstallation {
    pub tool: ToolKind,
    /// "system" denotes a PATH executable; "local:<name>" denotes a registration.
    pub version: String,
    pub executable: PathBuf,
    pub selected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ToolState {
    pub installations: Vec<ToolInstallation>,
    /// Persisted backend family; its selected version is recorded on the installation.
    pub backend: Option<ToolKind>,
    pub targets: TargetCatalog,
}

#[derive(Clone, Debug, Default)]
pub struct DebugTarget {
    pub name: String,
    pub vendor: String,
    pub source: String,
}

#[derive(Clone, Debug, Default)]
pub struct TargetCatalog {
    pub targets: Vec<DebugTarget>,
    /// OpenOCD interface configuration files for debug probes.
    pub probes: Vec<DebugTarget>,
    pub probe_message: String,
    pub packs: Vec<PathBuf>,
    pub message: String,
}

pub trait ToolRegistry: Send {
    fn discover(&self) -> Result<Vec<ToolInstallation>>;
    fn install(&mut self, tool: ToolKind, version: &str) -> Result<()>;
    fn install_latest(&mut self, tool: ToolKind) -> Result<()>;
    fn active_backend(&self) -> Result<Option<ToolKind>>;
    /// Select a version and persist this tool as the active desktop backend.
    fn select(&mut self, tool: ToolKind, version: &str) -> Result<()>;
    fn uninstall(&mut self, tool: ToolKind, version: &str) -> Result<()>;
    fn add_local(
        &mut self,
        _tool: ToolKind,
        _name: &str,
        _directory: &std::path::Path,
    ) -> Result<()> {
        Err(Error("当前后端不支持添加本地工具".into()))
    }
    fn remove_local(&mut self, _tool: ToolKind, _name: &str) -> Result<()> {
        Err(Error("当前后端不支持移除本地工具".into()))
    }
    fn target_catalog(&self) -> Result<TargetCatalog> {
        Ok(TargetCatalog::default())
    }
    fn add_pack(&mut self, _path: &std::path::Path) -> Result<()> {
        Err(Error("当前后端不支持添加芯片包".into()))
    }
    fn remove_pack(&mut self, _path: &std::path::Path) -> Result<()> {
        Err(Error("当前后端不支持移除芯片包".into()))
    }
    fn install_pack(&mut self, _part: &str) -> Result<()> {
        Err(Error("当前后端不支持在线安装芯片包".into()))
    }
}

/// Server readiness is distinct from an attached and halted GDB session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionState {
    #[default]
    Disconnected,
    Starting,
    ServerReady,
    Attaching,
    Halted,
    Running,
    Faulted,
}

pub trait DebugBackend: Send {
    fn start_server(&mut self, profile: &TargetProfile) -> Result<()>;
    fn stop_server(&mut self) -> Result<()>;
    fn flash(&mut self, profile: &TargetProfile) -> Result<()>;
    fn reset(&mut self, profile: &TargetProfile) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct WatchValue {
    pub expression: String,
    pub type_name: String,
    pub display_value: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum DebugAction {
    Connect {
        profile: TargetProfile,
        external_server: bool,
    },
    Disconnect,
    Continue,
    Pause,
    StepInto,
    StepOver,
    StepOut,
    Reset,
    Download,
    /// Flash through the selected probe backend, then reset without attaching GDB.
    Flash {
        profile: TargetProfile,
    },
    AddBreakpoint(String),
    AddSourceBreakpoint {
        file: String,
        line: u32,
    },
    RemoveBreakpoint(String),
    SetBreakpointEnabled {
        id: String,
        enabled: bool,
    },
    SelectFrame(u32),
    Refresh,
    WriteWatch {
        watch: WatchRequest,
        value: String,
    },
}

#[derive(Clone, Debug)]
pub struct WatchRequest {
    pub id: String,
    pub expression: String,
    pub function: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EvaluatedWatch {
    pub id: String,
    pub value: WatchValue,
}

#[derive(Clone, Debug, Default)]
pub struct StackFrame {
    pub level: u32,
    pub function: String,
    pub file: String,
    pub line: u32,
    pub address: String,
}

#[derive(Clone, Debug)]
pub struct Breakpoint {
    pub id: String,
    pub location: String,
    pub enabled: bool,
    pub file: String,
    pub line: u32,
}

#[derive(Clone, Debug, Default)]
pub struct DebugSnapshot {
    /// The stop is confirmed; stack unwinding and watch evaluation follow separately.
    pub context_pending: bool,
    pub watch_sample: bool,
    pub watch_sample_error: Option<String>,
    pub source: Option<Document>,
    pub state: SessionState,
    pub message: String,
    pub frames: Vec<StackFrame>,
    pub selected_frame: u32,
    pub breakpoints: Vec<Breakpoint>,
    pub values: Vec<EvaluatedWatch>,
    pub log: String,
}

pub trait DebugSession: Send {
    fn execute_debug(&mut self, action: DebugAction) -> Result<()>;
    fn poll_debug(&mut self) -> Result<bool>;
    fn debug_snapshot(&mut self, watches: &[WatchRequest]) -> DebugSnapshot;
    /// Refresh watches without interrupting or resuming a running target.
    fn sample_watches(&mut self, watches: &[WatchRequest]) -> DebugSnapshot;
    /// Read bytes through the existing session without interrupt/resume or a second attachment.
    fn read_live_memory(&mut self, _blocks: &[waveform::ReadBlock]) -> Result<Vec<Vec<u8>>> {
        Err(Error("当前调试会话不支持无暂停内存读取".into()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub path: PathBuf,
    pub text: String,
    pub encoding: String,
    pub bom: bool,
    pub line_ending: LineEnding,
    pub mixed_endings: bool,
}

pub trait TextBackend: Send {
    fn load(&self, path: &std::path::Path) -> Result<Document>;
    fn save(&self, document: &Document) -> Result<()>;
}
