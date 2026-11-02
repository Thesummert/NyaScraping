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
    pub gdb_port: u16,
    pub gdb_executable: PathBuf,
    pub firmware: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ToolInstallation {
    pub tool: ToolKind,
    /// "system" denotes a PATH executable; otherwise an installed version.
    pub version: String,
    pub executable: PathBuf,
    pub selected: bool,
}

pub trait ToolRegistry: Send {
    fn discover(&self) -> Result<Vec<ToolInstallation>>;
    fn install(&mut self, tool: ToolKind, version: &str) -> Result<()>;
    fn select(&mut self, tool: ToolKind, version: &str) -> Result<()>;
    fn uninstall(&mut self, tool: ToolKind, version: &str) -> Result<()>;
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

/// Future GDB/MI implementation; unsupported operations must return errors.
/// No adapter currently implements this interface.
pub trait DebugSession: Send {
    fn state(&self) -> SessionState;
    fn attach(&mut self, profile: &TargetProfile) -> Result<()>;
    fn resume(&mut self) -> Result<()>;
    fn halt(&mut self) -> Result<()>;
    fn step(&mut self) -> Result<()>;
    fn evaluate(&mut self, expression: &str) -> Result<WatchValue>;
    fn set_breakpoint(&mut self, file: &str, line: u32) -> Result<()>;
    fn detach(&mut self) -> Result<()>;
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
