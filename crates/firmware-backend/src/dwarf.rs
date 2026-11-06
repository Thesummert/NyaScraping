//! Embedded DWARF 2–5. No source-file access or target execution is required.
use crate::{error, MAX_FILE_BYTES, MAX_RECORDS};
use gimli::{AttributeValue as Value, Reader};
use nya_domain::{firmware::*, Result};
use object::{Object, ObjectSection};
use std::{collections::HashMap, rc::Rc};

#[derive(Clone, Debug)]
struct Relocations(Rc<object::read::RelocationMap>);

impl gimli::Relocate for Relocations {
    fn relocate_address(&self, offset: usize, value: u64) -> gimli::Result<u64> {
        Ok(self.0.relocate(offset as u64, value))
    }
    fn relocate_offset(&self, offset: usize, value: usize) -> gimli::Result<usize> {
        usize::try_from(self.0.relocate(offset as u64, value as u64))
            .map_err(|_| gimli::Error::UnsupportedOffset)
    }
}

type R = gimli::RelocateReader<gimli::EndianRcSlice<gimli::RunTimeEndian>, Relocations>;
type Key = (usize, gimli::UnitOffset);

struct Node {
    key: Key,
    tag: gimli::DwTag,
    parent: Option<Key>,
}

struct Context {
    dwarf: gimli::Dwarf<R>,
    units: Vec<gimli::Unit<R>>,
    nodes: Vec<Node>,
    offsets: HashMap<usize, Key>,
    files: Vec<HashMap<u64, usize>>,
}

pub fn parse(file: &object::File<'_>) -> Result<FirmwareDebugInfo> {
    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let mut total_bytes = 0u64;
    let dwarf = gimli::Dwarf::load(|id| -> Result<R> {
        // object also resolves GNU .zdebug_* names for ELF.
        let macho = format!("__{}", &id.name()[1..]);
        let section = file
            .section_by_name(id.name())
            .or_else(|| file.section_by_name(&macho));
        let (bytes, relocations) = if let Some(section) = section {
            let compressed = section.compressed_data().map_err(error)?;
            total_bytes = total_bytes
                .checked_add(compressed.uncompressed_size)
                .ok_or_else(|| error("DWARF size overflow"))?;
            if total_bytes > MAX_FILE_BYTES {
                return Err(error("decompressed DWARF exceeds 128 MiB"));
            }
            (
                section.uncompressed_data().map_err(error)?.into_owned(),
                section.relocation_map().map_err(error)?,
            )
        } else {
            (Vec::new(), object::read::RelocationMap::default())
        };
        Ok(gimli::RelocateReader::new(
            gimli::EndianRcSlice::new(Rc::from(bytes), endian),
            Relocations(Rc::new(relocations)),
        ))
    })?;
    let mut ctx = Context {
        dwarf,
        units: Vec::new(),
        nodes: Vec::new(),
        offsets: HashMap::new(),
        files: Vec::new(),
    };
    let mut headers = ctx.dwarf.units();
    while let Some(header) = headers.next().map_err(error)? {
        if ctx.units.len() >= MAX_RECORDS {
            return Err(error("too many DWARF units"));
        }
        ctx.units.push(ctx.dwarf.unit(header).map_err(error)?);
    }
    for (index, unit) in ctx.units.iter().enumerate() {
        let mut entries = unit.entries();
        let mut depth = 0isize;
        let mut parents = Vec::new();
        while let Some((delta, entry)) = entries.next_dfs().map_err(error)? {
            depth += delta;
            if !(0..=256).contains(&depth) {
                return Err(error("DWARF nesting limit exceeded"));
            }
            if ctx.nodes.len() >= MAX_RECORDS {
                return Err(error("too many DWARF entries"));
            }
            parents.truncate(depth as usize);
            let key = (index, entry.offset());
            let parent = parents.last().copied();
            parents.push(key);
            if let Some(offset) = entry.offset().to_debug_info_offset(&unit.header) {
                ctx.offsets.insert(offset.0, key);
            }
            ctx.nodes.push(Node {
                key,
                tag: entry.tag(),
                parent,
            });
        }
    }
    let mut info = FirmwareDebugInfo::default();
    ctx.read_files(&mut info)?;
    if let Err(error) = ctx.read_executable_lines(&mut info) {
        info.executable_lines.clear();
        info.warnings
            .push(format!("DWARF 可执行行信息不可用：{error}"));
    }
    if ctx.units.is_empty() {
        info.warnings.push("DWARF 信息节中没有编译单元。".into());
    }
    if ctx.units.iter().any(|u| u.dwo_id.is_some()) {
        info.warnings
            .push("包含 split DWARF；外部 .dwo/.dwp 文件尚未加载，信息可能不完整。".into());
    }
    let mut owners = HashMap::new();
    for node in &ctx.nodes {
        let owner = node.parent.and_then(|p| owners.get(&p).copied().flatten());
        if matches!(
            node.tag,
            gimli::DW_TAG_subprogram | gimli::DW_TAG_inlined_subroutine
        ) {
            let id = info.functions.len();
            owners.insert(node.key, Some(id));
            let entry = ctx.units[node.key.0].entry(node.key.1).map_err(error)?;
            let mut ranges = ctx
                .dwarf
                .die_ranges(&ctx.units[node.key.0], &entry)
                .map_err(error)?;
            let mut addresses = Vec::new();
            while let Some(range) = ranges.next().map_err(error)? {
                if addresses.len() >= MAX_RECORDS {
                    return Err(error("too many function ranges"));
                }
                addresses.push(AddressRange {
                    begin: range.begin,
                    end: range.end,
                });
            }
            let ty = ctx.referenced_type(node.key)?;
            info.functions.push(DebugFunction {
                name: ctx
                    .string(node.key, gimli::DW_AT_name)?
                    .unwrap_or_else(|| "<匿名函数>".into()),
                linkage_name: ctx
                    .string(node.key, gimli::DW_AT_linkage_name)?
                    .or(ctx.string(node.key, gimli::DW_AT_MIPS_linkage_name)?),
                return_type: ty.map(|t| t.0),
                declaration: ctx.source_location(node.key)?,
                ranges: addresses,
                declaration_only: matches!(
                    entry.attr_value(gimli::DW_AT_declaration).map_err(error)?,
                    Some(Value::Flag(true))
                ),
                inlined: node.tag == gimli::DW_TAG_inlined_subroutine,
            });
        } else {
            owners.insert(node.key, owner);
        }
    }
    let mut unavailable_locations = 0usize;
    for node in &ctx.nodes {
        if !matches!(
            node.tag,
            gimli::DW_TAG_variable | gimli::DW_TAG_formal_parameter | gimli::DW_TAG_constant
        ) {
            continue;
        }
        // A declaration describes a variable but does not identify its storage.
        // Definitions may refer back to it with DW_AT_specification, so check
        // the DIE itself rather than following inherited attributes here.
        if node.tag == gimli::DW_TAG_variable
            && matches!(
                ctx.units[node.key.0]
                    .entry(node.key.1)
                    .map_err(error)?
                    .attr_value(gimli::DW_AT_declaration)
                    .map_err(error)?,
                Some(Value::Flag(true))
            )
        {
            continue;
        }
        // Type declarations also contain parameter DIEs; only expose actual function parameters.
        let owner = owners.get(&node.key).copied().flatten();
        if node.tag == gimli::DW_TAG_formal_parameter && owner.is_none() {
            continue;
        }
        let ty = ctx.referenced_type(node.key)?;
        let constant =
            ctx.attr(node.key, gimli::DW_AT_const_value)?
                .map(|(_, value)| match value {
                    Value::Sdata(value) => value.to_string(),
                    Value::Udata(value) => value.to_string(),
                    Value::Data1(value) => value.to_string(),
                    Value::Data2(value) => value.to_string(),
                    Value::Data4(value) => value.to_string(),
                    Value::Data8(value) => value.to_string(),
                    Value::String(value) => value
                        .to_string_lossy()
                        .map(|s| s.into_owned())
                        .unwrap_or_default(),
                    _ => "<常量块>".into(),
                });
        let name = ctx
            .string(node.key, gimli::DW_AT_name)?
            .unwrap_or_else(|| "<匿名变量>".into());
        // A location expression is optional metadata. Unsupported opcodes or
        // malformed location lists must not discard source files and other DIEs.
        let location = ctx.location(node.key).unwrap_or_else(|error| {
            unavailable_locations += 1;
            if unavailable_locations <= 10 {
                let (unit, offset) = node.key;
                info.warnings.push(format!(
                    "DWARF 变量“{name}”位置不可用（编译单元 {unit}，DIE {:#x}）：{error}",
                    offset.0
                ));
            }
            VariableLocation::Unavailable
        });
        info.variables.push(DebugVariable {
            name,
            type_name: ty.as_ref().map(|t| t.0.clone()),
            byte_size: ty.and_then(|t| t.1),
            scalar_type: ctx.scalar_type(node.key, &mut Vec::new())?,
            members: ctx.variable_members(node.key, &mut Vec::new(), &mut 4096)?,
            declaration: ctx.source_location(node.key)?,
            function: owner,
            kind: if node.tag == gimli::DW_TAG_formal_parameter {
                VariableKind::Parameter
            } else if owner.is_some() {
                VariableKind::Local
            } else {
                VariableKind::Global
            },
            location,
            constant,
        });
    }
    if unavailable_locations > 10 {
        info.warnings.push(format!(
            "另有 {} 个 DWARF 变量位置不可用；已保留源码位置、类型及其他有效调试信息。",
            unavailable_locations - 10
        ));
    }
    Ok(info)
}

impl Context {
    fn reference(&self, key: Key, value: Value<R>) -> Option<Key> {
        match value {
            Value::UnitRef(offset) => Some((key.0, offset)),
            Value::DebugInfoRef(offset) => self.offsets.get(&offset.0).copied(),
            _ => None,
        }
    }

    fn attr(&self, key: Key, name: gimli::DwAt) -> Result<Option<(Key, Value<R>)>> {
        self.attr_inner(key, name, &mut Vec::new())
    }

    fn attr_inner(
        &self,
        key: Key,
        name: gimli::DwAt,
        visited: &mut Vec<Key>,
    ) -> Result<Option<(Key, Value<R>)>> {
        if visited.len() >= 32 || visited.contains(&key) {
            return Ok(None);
        }
        visited.push(key);
        let entry = self.units[key.0].entry(key.1).map_err(error)?;
        if let Some(value) = entry.attr_value(name).map_err(error)? {
            return Ok(Some((key, value)));
        }
        for link in [gimli::DW_AT_abstract_origin, gimli::DW_AT_specification] {
            if let Some(value) = entry.attr_value(link).map_err(error)? {
                if let Some(next) = self.reference(key, value) {
                    if let Some(value) = self.attr_inner(next, name, visited)? {
                        return Ok(Some(value));
                    }
                }
            }
        }
        Ok(None)
    }

    fn string(&self, key: Key, name: gimli::DwAt) -> Result<Option<String>> {
        self.attr(key, name)?
            .map(|(key, value)| {
                self.dwarf
                    .attr_string(&self.units[key.0], value)
                    .map_err(error)?
                    .to_string_lossy()
                    .map(|s| s.into_owned())
                    .map_err(error)
            })
            .transpose()
    }

    fn number(&self, key: Key, name: gimli::DwAt) -> Result<Option<u64>> {
        Ok(self.attr(key, name)?.and_then(|(_, v)| v.udata_value()))
    }

    fn source_location(&self, key: Key) -> Result<Option<SourceLocation>> {
        let Some((source_key, value)) = self.attr(key, gimli::DW_AT_decl_file)? else {
            return Ok(None);
        };
        let index = match value {
            Value::FileIndex(i) => i,
            v => v.udata_value().unwrap_or(u64::MAX),
        };
        let line = self.number(key, gimli::DW_AT_decl_line)?.filter(|n| *n > 0);
        let column = self
            .number(key, gimli::DW_AT_decl_column)?
            .filter(|n| *n > 0);
        Ok(self.files[source_key.0]
            .get(&index)
            .copied()
            .map(|file| SourceLocation { file, line, column }))
    }

    fn read_files(&mut self, info: &mut FirmwareDebugInfo) -> Result<()> {
        let mut paths = HashMap::new();
        let mut file_records = 0usize;
        for unit in &self.units {
            let comp_dir = unit
                .comp_dir
                .as_ref()
                .map(|r| r.to_string_lossy().map(|s| s.into_owned()))
                .transpose()
                .map_err(error)?
                .unwrap_or_default();
            let name = unit
                .name
                .as_ref()
                .map(|r| r.to_string_lossy().map(|s| s.into_owned()))
                .transpose()
                .map_err(error)?;
            let primary =
                name.map(|name| intern_file(info, &mut paths, join_path(&comp_dir, &name)));
            let mut files = HashMap::new();
            if let Some(program) = &unit.line_program {
                let header = program.header();
                let start = if header.version() < 5 { 1 } else { 0 };
                for (index, file) in header.file_names().iter().enumerate() {
                    file_records += 1;
                    if info.files.len() >= MAX_RECORDS || file_records > MAX_RECORDS {
                        return Err(error("too many DWARF source files"));
                    }
                    let name = self
                        .dwarf
                        .attr_string(unit, file.path_name())
                        .map_err(error)?
                        .to_string_lossy()
                        .map_err(error)?
                        .into_owned();
                    let directory = file
                        .directory(header)
                        .map(|v| {
                            self.dwarf
                                .attr_string(unit, v)
                                .and_then(|r| r.to_string_lossy().map(|s| s.into_owned()))
                        })
                        .transpose()
                        .map_err(error)?
                        .unwrap_or_default();
                    let path = join_path(&join_path(&comp_dir, &directory), &name);
                    let id = intern_file(info, &mut paths, path);
                    files.insert(index as u64 + start, id);
                }
            }
            if files.is_empty() {
                if let Some(primary) = primary {
                    files.insert(1, primary);
                }
            }
            self.files.push(files);
        }
        Ok(())
    }

    fn read_executable_lines(&self, info: &mut FirmwareDebugInfo) -> Result<()> {
        let mut locations = Vec::new();
        let mut rows_seen = 0usize;
        let mut paths: HashMap<_, _> = info
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| (file.path.clone(), i))
            .collect();
        for (index, unit) in self.units.iter().enumerate() {
            let Some(program) = &unit.line_program else {
                continue;
            };
            let mut files = self.files[index].clone();
            let comp_dir = unit
                .comp_dir
                .as_ref()
                .map(|r| r.to_string_lossy().map(|s| s.into_owned()))
                .transpose()
                .map_err(error)?
                .unwrap_or_default();
            let mut rows = program.clone().rows();
            while let Some((header, row)) = rows.next_row().map_err(error)? {
                rows_seen += 1;
                if rows_seen > MAX_RECORDS * 10 {
                    return Err(error("too many DWARF line rows"));
                }
                if row.end_sequence() {
                    continue;
                }
                let Some(line) = row.line() else { continue };
                let index = row.file_index();
                let file = if let Some(&file) = files.get(&index) {
                    file
                } else if let Some(file) = row.file(header) {
                    if info.files.len() >= MAX_RECORDS {
                        return Err(error("too many DWARF source files"));
                    }
                    let name = self
                        .dwarf
                        .attr_string(unit, file.path_name())
                        .map_err(error)?
                        .to_string_lossy()
                        .map_err(error)?
                        .into_owned();
                    let directory = file
                        .directory(header)
                        .map(|v| {
                            self.dwarf
                                .attr_string(unit, v)
                                .and_then(|r| r.to_string_lossy().map(|s| s.into_owned()))
                        })
                        .transpose()
                        .map_err(error)?
                        .unwrap_or_default();
                    let file = intern_file(
                        info,
                        &mut paths,
                        join_path(&join_path(&comp_dir, &directory), &name),
                    );
                    files.insert(index, file);
                    file
                } else {
                    continue;
                };
                locations.push(SourceLocation {
                    file,
                    line: Some(line.get()),
                    column: None,
                });
            }
        }
        locations.sort_unstable_by_key(|location| (location.file, location.line));
        locations.dedup();
        info.executable_lines = locations;
        Ok(())
    }

    fn referenced_type(&self, key: Key) -> Result<Option<(String, Option<u64>)>> {
        let Some((owner, value)) = self.attr(key, gimli::DW_AT_type)? else {
            return Ok(None);
        };
        self.reference(owner, value)
            .map(|key| self.type_name(key, &mut Vec::new()))
            .transpose()
    }

    fn scalar_type(
        &self,
        key: Key,
        visited: &mut Vec<Key>,
    ) -> Result<Option<nya_domain::waveform::ScalarType>> {
        use nya_domain::waveform::ScalarType;
        if visited.len() >= 32 || visited.contains(&key) {
            return Ok(None);
        }
        visited.push(key);
        let tag = self.units[key.0].entry(key.1).map_err(error)?.tag();
        let result = if tag == gimli::DW_TAG_base_type {
            match self
                .attr(key, gimli::DW_AT_encoding)?
                .map(|(_, value)| value)
            {
                Some(Value::Encoding(gimli::DW_ATE_signed | gimli::DW_ATE_signed_char)) => {
                    Some(ScalarType::Signed)
                }
                Some(Value::Encoding(gimli::DW_ATE_unsigned | gimli::DW_ATE_unsigned_char)) => {
                    Some(ScalarType::Unsigned)
                }
                Some(Value::Encoding(gimli::DW_ATE_float)) => Some(ScalarType::Float),
                Some(Value::Encoding(gimli::DW_ATE_boolean)) => Some(ScalarType::Boolean),
                _ => None,
            }
        } else if matches!(
            tag,
            gimli::DW_TAG_variable
                | gimli::DW_TAG_member
                | gimli::DW_TAG_formal_parameter
                | gimli::DW_TAG_constant
                | gimli::DW_TAG_typedef
                | gimli::DW_TAG_const_type
                | gimli::DW_TAG_volatile_type
                | gimli::DW_TAG_restrict_type
                | gimli::DW_TAG_enumeration_type
        ) {
            match self.attr(key, gimli::DW_AT_type)? {
                Some((owner, value)) => match self.reference(owner, value) {
                    Some(next) => self.scalar_type(next, visited)?,
                    None => None,
                },
                None => None,
            }
        } else {
            None
        };
        visited.pop();
        Ok(result)
    }

    fn variable_members(
        &self,
        key: Key,
        visited: &mut Vec<Key>,
        budget: &mut usize,
    ) -> Result<Vec<DebugMember>> {
        match self.attr(key, gimli::DW_AT_type)? {
            Some((owner, value)) => match self.reference(owner, value) {
                Some(target) => self.type_members(target, visited, budget),
                None => Ok(Vec::new()),
            },
            None => Ok(Vec::new()),
        }
    }

    fn type_members(
        &self,
        key: Key,
        visited: &mut Vec<Key>,
        budget: &mut usize,
    ) -> Result<Vec<DebugMember>> {
        if visited.len() >= 32 || visited.contains(&key) || *budget == 0 {
            return Ok(Vec::new());
        }
        visited.push(key);
        let tag = self.units[key.0].entry(key.1).map_err(error)?.tag();
        let mut members = Vec::new();
        match tag {
            gimli::DW_TAG_typedef
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type => {
                members = self.variable_members(key, visited, budget)?;
            }
            gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type | gimli::DW_TAG_union_type => {
                let mut tree = self.units[key.0].entries_tree(Some(key.1)).map_err(error)?;
                let root = tree.root().map_err(error)?;
                let mut children = root.children();
                while let Some(child) = children.next().map_err(error)? {
                    if *budget == 0 {
                        break;
                    }
                    if child.entry().tag() != gimli::DW_TAG_member {
                        continue;
                    }
                    *budget -= 1;
                    let member = (key.0, child.entry().offset());
                    let bit_field = self.attr(member, gimli::DW_AT_bit_size)?.is_some()
                        || self.attr(member, gimli::DW_AT_bit_offset)?.is_some()
                        || self.attr(member, gimli::DW_AT_data_bit_offset)?.is_some();
                    let member_type = self.referenced_type(member)?;
                    members.push(DebugMember {
                        name: self
                            .string(member, gimli::DW_AT_name)?
                            .unwrap_or_else(|| "<匿名成员>".into()),
                        type_name: member_type
                            .as_ref()
                            .map(|t| t.0.clone())
                            .unwrap_or_else(|| "类型未记录".into()),
                        offset: if bit_field {
                            None
                        } else {
                            self.number(member, gimli::DW_AT_data_member_location)?
                        },
                        byte_size: member_type.and_then(|t| t.1),
                        scalar_type: if bit_field {
                            None
                        } else {
                            self.scalar_type(member, &mut Vec::new())?
                        },
                        members: self.variable_members(member, visited, budget)?,
                    });
                }
            }
            gimli::DW_TAG_array_type => {
                let target = self
                    .attr(key, gimli::DW_AT_type)?
                    .and_then(|(owner, value)| self.reference(owner, value));
                if let Some(target) = target {
                    let mut dimensions = Vec::new();
                    let mut tree = self.units[key.0].entries_tree(Some(key.1)).map_err(error)?;
                    let root = tree.root().map_err(error)?;
                    let mut children = root.children();
                    while let Some(child) = children.next().map_err(error)? {
                        if child.entry().tag() != gimli::DW_TAG_subrange_type {
                            continue;
                        }
                        let dim = (key.0, child.entry().offset());
                        let lower = self.number(dim, gimli::DW_AT_lower_bound)?.unwrap_or(0);
                        let count = self.number(dim, gimli::DW_AT_count)?.or(self
                            .number(dim, gimli::DW_AT_upper_bound)?
                            .and_then(|upper| upper.checked_sub(lower)?.checked_add(1)));
                        dimensions.push((lower, count));
                    }
                    if !dimensions.is_empty() {
                        members = self.array_members(target, &dimensions, visited, budget)?;
                    }
                }
            }
            // Pointers stay leaves: inspecting their target requires a live session.
            _ => {}
        }
        visited.pop();
        Ok(members)
    }

    fn array_members(
        &self,
        target: Key,
        dimensions: &[(u64, Option<u64>)],
        visited: &mut Vec<Key>,
        budget: &mut usize,
    ) -> Result<Vec<DebugMember>> {
        let (lower, Some(count)) = dimensions[0] else {
            return Ok(Vec::new());
        };
        let (base_name, base_size) = self.type_name(target, &mut Vec::new())?;
        let remaining = &dimensions[1..];
        let stride = remaining.iter().fold(base_size, |size, (_, count)| {
            size.zip(*count)
                .and_then(|(size, count)| size.checked_mul(count))
        });
        let mut type_name = base_name;
        for (_, count) in remaining {
            type_name.push_str(&format!(
                "[{}]",
                count.map(|n| n.to_string()).unwrap_or_default()
            ));
        }
        let mut members = Vec::new();
        for index in 0..count {
            if *budget == 0 {
                break;
            }
            let Some(label) = lower.checked_add(index) else {
                break;
            };
            *budget -= 1;
            let children = if remaining.is_empty() {
                self.type_members(target, visited, budget)?
            } else {
                self.array_members(target, remaining, visited, budget)?
            };
            members.push(DebugMember {
                name: format!("[{label}]"),
                type_name: type_name.clone(),
                offset: stride.and_then(|size| size.checked_mul(index)),
                byte_size: stride,
                scalar_type: if remaining.is_empty() {
                    self.scalar_type(target, &mut Vec::new())?
                } else {
                    None
                },
                members: children,
            });
        }
        Ok(members)
    }

    fn type_name(&self, key: Key, visited: &mut Vec<Key>) -> Result<(String, Option<u64>)> {
        if visited.len() >= 32 || visited.contains(&key) {
            return Ok(("<递归类型>".into(), None));
        }
        visited.push(key);
        let tag = self.units[key.0].entry(key.1).map_err(error)?.tag();
        let size = self.number(key, gimli::DW_AT_byte_size)?;
        let name = self.string(key, gimli::DW_AT_name)?;
        if let Some(name) = name {
            // A typedef's size is normally on its referenced type.
            let size = if size.is_none() && tag == gimli::DW_TAG_typedef {
                self.type_target(key, visited)?.1
            } else {
                size
            };
            visited.pop();
            return Ok((name, size));
        }
        let (base, base_size) = self.type_target(key, visited)?;
        let result = match tag {
            gimli::DW_TAG_pointer_type => (
                format!("{base} *"),
                size.or(Some(self.units[key.0].encoding().address_size as u64)),
            ),
            gimli::DW_TAG_reference_type => (format!("{base} &"), size),
            gimli::DW_TAG_rvalue_reference_type => (format!("{base} &&"), size),
            gimli::DW_TAG_const_type => (format!("const {base}"), size.or(base_size)),
            gimli::DW_TAG_volatile_type => (format!("volatile {base}"), size.or(base_size)),
            gimli::DW_TAG_restrict_type => (format!("restrict {base}"), size.or(base_size)),
            gimli::DW_TAG_array_type => {
                let mut dimensions = String::new();
                let mut count = Some(1u64);
                let mut tree = self.units[key.0].entries_tree(Some(key.1)).map_err(error)?;
                let root = tree.root().map_err(error)?;
                let mut children = root.children();
                while let Some(child) = children.next().map_err(error)? {
                    if child.entry().tag() != gimli::DW_TAG_subrange_type {
                        continue;
                    }
                    let dim = (key.0, child.entry().offset());
                    let n = self.number(dim, gimli::DW_AT_count)?.or_else(|| {
                        let lower = self
                            .number(dim, gimli::DW_AT_lower_bound)
                            .ok()
                            .flatten()
                            .unwrap_or(0);
                        self.number(dim, gimli::DW_AT_upper_bound)
                            .ok()
                            .flatten()?
                            .checked_sub(lower)?
                            .checked_add(1)
                    });
                    dimensions.push_str(&format!(
                        "[{}]",
                        n.map(|v| v.to_string()).unwrap_or_default()
                    ));
                    count = count.zip(n).and_then(|(a, b)| a.checked_mul(b));
                }
                if dimensions.is_empty() {
                    dimensions.push_str("[]");
                    count = None;
                }
                (
                    format!("{base}{dimensions}"),
                    size.or(base_size.zip(count).and_then(|(a, b)| a.checked_mul(b))),
                )
            }
            gimli::DW_TAG_structure_type => ("<匿名 struct>".into(), size),
            gimli::DW_TAG_union_type => ("<匿名 union>".into(), size),
            gimli::DW_TAG_enumeration_type => ("<匿名 enum>".into(), size),
            gimli::DW_TAG_subroutine_type => (format!("{base} (…)"), size),
            _ => ("<未知类型>".into(), size),
        };
        visited.pop();
        Ok(result)
    }

    fn type_target(&self, key: Key, visited: &mut Vec<Key>) -> Result<(String, Option<u64>)> {
        match self.attr(key, gimli::DW_AT_type)? {
            Some((owner, value)) => match self.reference(owner, value) {
                Some(key) => self.type_name(key, visited),
                None => Ok(("<外部类型>".into(), None)),
            },
            None => Ok(("void".into(), None)),
        }
    }

    fn expression(&self, unit: usize, expr: gimli::Expression<R>) -> Result<(Option<u64>, String)> {
        let mut ops = expr.operations(self.units[unit].encoding());
        let mut parts = Vec::new();
        let mut address = None;
        while let Some(op) = ops.next().map_err(error)? {
            if parts.len() >= 256 {
                return Err(error("DWARF expression too long"));
            }
            match op {
                gimli::Operation::Address { address: value } => address = Some(value),
                gimli::Operation::AddressIndex { index } => {
                    address = Some(
                        self.dwarf
                            .address(&self.units[unit], index)
                            .map_err(error)?,
                    )
                }
                _ => {}
            }
            parts.push(format!("{op:?}"));
        }
        Ok((
            if parts.len() == 1 { address } else { None },
            parts.join("; "),
        ))
    }

    fn location(&self, key: Key) -> Result<VariableLocation> {
        let Some((owner, value)) = self.attr(key, gimli::DW_AT_location)? else {
            return Ok(VariableLocation::Unavailable);
        };
        if let Value::Exprloc(expr) = value {
            let (address, expression) = self.expression(owner.0, expr)?;
            return Ok(address.map_or(
                VariableLocation::Expression(expression),
                VariableLocation::Address,
            ));
        }
        if let Some(mut locations) = self
            .dwarf
            .attr_locations(&self.units[owner.0], value)
            .map_err(error)?
        {
            let mut entries = Vec::new();
            while let Some(location) = locations.next().map_err(error)? {
                if entries.len() >= MAX_RECORDS {
                    return Err(error("too many variable locations"));
                }
                entries.push(VariableLocationRange {
                    range: AddressRange {
                        begin: location.range.begin,
                        end: location.range.end,
                    },
                    expression: self.expression(owner.0, location.data)?.1,
                });
            }
            return Ok(VariableLocation::List(entries));
        }
        Ok(VariableLocation::Unavailable)
    }
}

fn intern_file(
    info: &mut FirmwareDebugInfo,
    paths: &mut HashMap<String, usize>,
    path: String,
) -> usize {
    *paths.entry(path.clone()).or_insert_with(|| {
        let index = info.files.len();
        info.files.push(SourceFile { path });
        index
    })
}

fn join_path(base: &str, name: &str) -> String {
    let name = name.replace('\\', "/");
    let absolute = name.starts_with('/') || name.as_bytes().get(1) == Some(&b':');
    let path = if absolute || base.is_empty() {
        name
    } else {
        format!("{}/{name}", base.replace('\\', "/"))
    };
    let prefix = if path.starts_with("//") {
        "//"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let mut components: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if components
                .last()
                .is_some_and(|p| *p != ".." && !p.ends_with(':')) =>
            {
                components.pop();
            }
            ".." if !prefix.is_empty() => {}
            _ => components.push(part),
        }
    }
    format!("{prefix}{}", components.join("/"))
}
