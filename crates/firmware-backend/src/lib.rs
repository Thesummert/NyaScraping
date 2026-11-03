//! Content-based object-file inspection; never executes the input or external tools.
#![forbid(unsafe_code)]

mod dwarf;

use nya_domain::{
    firmware::{FirmwareBackend, FirmwareImage, FirmwareSection, FirmwareSegment, FirmwareSymbol},
    Error, Result,
};
use object::{Object, ObjectSection, ObjectSegment, ObjectSymbol};
use std::{fs::File, io::Read, path::Path};

pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_RECORDS: usize = 200_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeFirmwareBackend;

fn error(error: impl std::fmt::Display) -> Error {
    Error(format!("Compiled file: {error}"))
}

fn check_range(offset: u64, size: u64, file_size: u64) -> Result<()> {
    if offset.checked_add(size).is_none_or(|end| end > file_size) {
        return Err(error("section or segment extends beyond the file"));
    }
    Ok(())
}

fn check_count(count: usize) -> Result<()> {
    if count >= MAX_RECORDS {
        return Err(error(
            "too many sections, segments or symbols (limit 200000)",
        ));
    }
    Ok(())
}

impl NativeFirmwareBackend {
    /// Extensions are deliberately ignored: AXF/OUT commonly contain ELF.
    /// Raw BIN/HEX have no object header and are not accepted by this inspector.
    pub fn parse(&self, path: &Path, bytes: &[u8]) -> Result<FirmwareImage> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(error("file exceeds 128 MiB"));
        }
        let file = object::File::parse(bytes).map_err(error)?;
        let file_size = bytes.len() as u64;
        let mut segments = Vec::new();
        for segment in file.segments() {
            check_count(segments.len())?;
            let (file_offset, size) = segment.file_range();
            check_range(file_offset, size, file_size)?;
            segments.push(FirmwareSegment {
                address: segment.address(),
                memory_size: segment.size(),
                file_offset,
                file_size: size,
                alignment: segment.align(),
                flags: format!("{:?}", segment.flags()),
            });
        }
        let mut sections = Vec::new();
        let mut has_debug_info = false;
        for section in file.sections() {
            check_count(segments.len() + sections.len())?;
            let name = String::from_utf8_lossy(section.name_bytes().map_err(error)?).into_owned();
            has_debug_info |= matches!(
                name.as_str(),
                ".debug_info" | ".zdebug_info" | "__debug_info" | "__zdebug_info"
            ) && section.size() > 0;
            let file_range = section.file_range();
            if let Some((offset, size)) = file_range {
                check_range(offset, size, file_size)?;
            }
            sections.push(FirmwareSection {
                index: section.index().0,
                name,
                address: section.address(),
                size: section.size(),
                file_range,
                kind: format!("{:?}", section.kind()),
                flags: format!("{:?}", section.flags()),
            });
        }
        let mut symbols = Vec::new();
        for (symbol, dynamic) in file
            .symbols()
            .map(|s| (s, false))
            .chain(file.dynamic_symbols().map(|s| (s, true)))
        {
            check_count(segments.len() + sections.len() + symbols.len())?;
            symbols.push(FirmwareSymbol {
                name: String::from_utf8_lossy(symbol.name_bytes().map_err(error)?).into_owned(),
                address: symbol.address(),
                size: symbol.size(),
                kind: format!("{:?}", symbol.kind()),
                scope: format!("{:?}", symbol.scope()),
                section_index: symbol.section_index().map(|index| index.0),
                undefined: symbol.is_undefined(),
                weak: symbol.is_weak(),
                dynamic,
            });
        }
        symbols.sort_by(|a, b| a.address.cmp(&b.address).then(a.name.cmp(&b.name)));
        let mut debug = if has_debug_info {
            dwarf::parse(&file).unwrap_or_else(|error| nya_domain::firmware::FirmwareDebugInfo {
                warnings: vec![format!("DWARF 解析失败：{error}")],
                ..Default::default()
            })
        } else {
            nya_domain::firmware::FirmwareDebugInfo {
                warnings: vec!["未找到嵌入式 DWARF 信息，无法恢复源码文件、函数参数或局部变量；PE/PDB 等外部调试信息尚未加载。".into()],
                ..Default::default()
            }
        };
        // Stripped or unsupported debug formats can still expose named static symbols.
        // Keep their source/type information explicitly unknown.
        use nya_domain::firmware::{
            AddressRange, DebugFunction, DebugVariable, VariableKind, VariableLocation,
        };
        let fallback_functions = debug.functions.is_empty();
        let fallback_variables = debug.variables.is_empty();
        let mut fallback_count = 0;
        for symbol in &symbols {
            if symbol.undefined || symbol.name.is_empty() || symbol.dynamic {
                continue;
            }
            if fallback_functions && symbol.kind == "Text" {
                debug.functions.push(DebugFunction {
                    name: symbol.name.clone(),
                    linkage_name: None,
                    return_type: None,
                    declaration: None,
                    ranges: symbol
                        .address
                        .checked_add(symbol.size)
                        .filter(|end| *end > symbol.address)
                        .map(|end| {
                            vec![AddressRange {
                                begin: symbol.address,
                                end,
                            }]
                        })
                        .unwrap_or_default(),
                    declaration_only: false,
                    inlined: false,
                });
                fallback_count += 1;
            } else if fallback_variables && matches!(symbol.kind.as_str(), "Data" | "Tls") {
                debug.variables.push(DebugVariable {
                    scalar_type: None,
                    name: symbol.name.clone(),
                    type_name: None,
                    byte_size: Some(symbol.size),
                    members: Vec::new(),
                    declaration: None,
                    function: None,
                    kind: VariableKind::Global,
                    location: if symbol.kind == "Tls" {
                        VariableLocation::Unavailable
                    } else {
                        VariableLocation::Address(symbol.address)
                    },
                    constant: None,
                });
                fallback_count += 1;
            }
        }
        if fallback_count > 0 {
            debug.warnings.push(
                "部分条目来自符号表回退，仅包含符号地址和大小，源码位置、类型及参数不可恢复。"
                    .into(),
            );
        }
        Ok(FirmwareImage {
            path: path.to_owned(),
            file_size,
            format: format!("{:?}", file.format()),
            architecture: format!("{:?}", file.architecture()),
            kind: format!("{:?}", file.kind()),
            address_bits: if file.is_64() { 64 } else { 32 },
            little_endian: file.is_little_endian(),
            entry: file.entry(),
            has_debug_info,
            debug,
            segments,
            sections,
            symbols,
        })
    }
}

impl FirmwareBackend for NativeFirmwareBackend {
    fn load(&self, path: &Path) -> Result<FirmwareImage> {
        let file = File::open(path).map_err(error)?;
        let metadata = file.metadata().map_err(error)?;
        if !metadata.is_file() {
            return Err(error("input must be a regular file"));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(error("file exceeds 128 MiB"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        self.parse(path, &bytes)
            .map_err(|e| Error(format!("{}: {e}", path.display())))
    }
}
