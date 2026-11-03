//! Static compiled-file metadata, independent of a live debug session.
use crate::Result;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct FirmwareImage {
    pub path: PathBuf,
    pub file_size: u64,
    pub format: String,
    pub architecture: String,
    pub kind: String,
    pub address_bits: u8,
    pub little_endian: bool,
    pub entry: u64,
    /// Whether an embedded DWARF information section is present.
    pub has_debug_info: bool,
    pub debug: FirmwareDebugInfo,
    pub segments: Vec<FirmwareSegment>,
    pub sections: Vec<FirmwareSection>,
    pub symbols: Vec<FirmwareSymbol>,
}

#[derive(Clone, Debug)]
pub struct FirmwareSegment {
    pub address: u64,
    pub memory_size: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub alignment: u64,
    pub flags: String,
}

#[derive(Clone, Debug)]
pub struct FirmwareSection {
    pub index: usize,
    pub name: String,
    pub address: u64,
    pub size: u64,
    /// BSS/NOBITS sections occupy memory but have no file contents.
    pub file_range: Option<(u64, u64)>,
    pub kind: String,
    pub flags: String,
}

#[derive(Clone, Debug)]
pub struct FirmwareSymbol {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub kind: String,
    pub scope: String,
    pub section_index: Option<usize>,
    pub undefined: bool,
    pub weak: bool,
    pub dynamic: bool,
}

pub trait FirmwareBackend: Send {
    fn load(&self, path: &Path) -> Result<FirmwareImage>;
}

#[derive(Clone, Debug, Default)]
pub struct FirmwareDebugInfo {
    pub files: Vec<SourceFile>,
    pub functions: Vec<DebugFunction>,
    pub variables: Vec<DebugVariable>,
    /// Unsupported or unavailable debug information, shown in the inspector.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Original compiler path, normalized independently of the host OS.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: usize,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressRange {
    pub begin: u64,
    pub end: u64,
}

#[derive(Clone, Debug)]
pub struct DebugFunction {
    pub name: String,
    pub linkage_name: Option<String>,
    pub return_type: Option<String>,
    pub declaration: Option<SourceLocation>,
    pub ranges: Vec<AddressRange>,
    pub declaration_only: bool,
    pub inlined: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariableKind {
    Global,
    Local,
    Parameter,
}

#[derive(Clone, Debug)]
pub struct DebugVariable {
    pub name: String,
    pub type_name: Option<String>,
    pub byte_size: Option<u64>,
    /// Numeric representation from DWARF, including typedef/qualifier resolution.
    pub scalar_type: Option<crate::waveform::ScalarType>,
    pub declaration: Option<SourceLocation>,
    /// Index in FirmwareDebugInfo::functions; lexical blocks retain their owner.
    pub function: Option<usize>,
    pub kind: VariableKind,
    pub location: VariableLocation,
    pub constant: Option<String>,
}

#[derive(Clone, Debug)]
pub enum VariableLocation {
    Address(u64),
    /// Frame/register/compound expressions require a target execution context.
    Expression(String),
    List(Vec<VariableLocationRange>),
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct VariableLocationRange {
    pub range: AddressRange,
    pub expression: String,
}
