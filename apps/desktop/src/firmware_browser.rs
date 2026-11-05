//! A stable tree independent of Slint; visible rows are rebuilt when a branch toggles.
use nya_domain::firmware::*;
use nya_domain::waveform::MemoryChannel;
use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
};

struct Node {
    label: String,
    details: String,
    children: Vec<usize>,
    expanded: bool,
    variable: Option<BrowserVariable>,
    function: bool,
    has_variables: bool,
    search_name: String,
    source_path: Option<String>,
}

#[derive(Clone)]
pub struct BrowserVariable {
    pub id: usize,
    pub name: String,
    pub type_name: String,
    pub scope: String,
    pub memory: Option<MemoryChannel>,
    pub base_address: Option<u64>,
    pub little_endian: bool,
    pub members: Vec<DebugMember>,
    pub evaluation_expression: String,
    pub function: Option<String>,
}

pub struct BrowserRow {
    pub label: String,
    pub type_name: String,
    pub depth: usize,
    pub branch: bool,
    pub expanded: bool,
    pub variable: bool,
    pub can_plot: bool,
    pub source_path: Option<String>,
}

pub struct FirmwareBrowser {
    nodes: Vec<Node>,
    roots: Vec<usize>,
    visible: Vec<usize>,
    selected: Option<usize>,
    show_functions: bool,
    query: String,
    matches: Vec<bool>,
    search_collapsed: HashSet<usize>,
}

impl FirmwareBrowser {
    pub fn new(info: &FirmwareDebugInfo, little_endian: bool, absolute_addresses: bool) -> Self {
        let mut tree = Self {
            nodes: Vec::new(),
            roots: Vec::new(),
            visible: Vec::new(),
            selected: None,
            show_functions: false,
            query: String::new(),
            matches: Vec::new(),
            search_collapsed: HashSet::new(),
        };
        let mut directories = HashMap::new();
        let mut files = Vec::new();
        for file in &info.files {
            let mut parent = None;
            let mut prefix = String::new();
            let mut parts: Vec<_> = file.path.split('/').filter(|s| !s.is_empty()).collect();
            let filename = parts.pop().unwrap_or(&file.path);
            if file.path.starts_with('/') {
                prefix.push('/');
                let root = *directories
                    .entry(prefix.clone())
                    .or_insert_with(|| tree.add(None, "/".into(), "源码路径根目录".into()));
                parent = Some(root);
            }
            for part in parts {
                prefix.push_str(part);
                prefix.push('/');
                let id = *directories
                    .entry(prefix.clone())
                    .or_insert_with(|| tree.add(parent, part.into(), prefix.clone()));
                parent = Some(id);
            }
            let id = tree.add(
                parent,
                filename.into(),
                format!(
                    "源文件\n{}\n\n文件树来自编译器记录的路径；源文件不需要在本机存在。",
                    file.path
                ),
            );
            tree.nodes[id].search_name = file.path.to_lowercase();
            tree.nodes[id].source_path = Some(file.path.clone());
            files.push(id);
        }
        // Keep entries lacking source locations visible without inventing a source file.
        let mut unknown = None;
        let mut functions = Vec::new();
        for function in &info.functions {
            let parent = function
                .declaration
                .as_ref()
                .and_then(|loc| files.get(loc.file).copied())
                .unwrap_or_else(|| {
                    *unknown.get_or_insert_with(|| {
                        tree.add(
                            None,
                            "无源码位置".into(),
                            "这些条目没有可用的源码声明位置。".into(),
                        )
                    })
                });
            let mut details = format!(
                "函数：{}\n链接名：{}\n返回类型：{}\n{}\n{}{}\n",
                function.name,
                function.linkage_name.as_deref().unwrap_or("—"),
                function.return_type.as_deref().unwrap_or("void / 未记录"),
                source_position(info, function.declaration.as_ref()),
                if function.declaration_only {
                    "仅声明；没有函数体地址。"
                } else {
                    ""
                },
                if function.inlined {
                    "内联实例。"
                } else {
                    ""
                }
            );
            details.push_str("\n地址范围（结束地址不包含在内）：\n");
            for range in &function.ranges {
                let _ = writeln!(details, "0x{:X} .. 0x{:X}", range.begin, range.end);
            }
            if function.ranges.is_empty() {
                details.push_str("未记录 / 已优化\n");
            }
            let id = tree.add(Some(parent), format!("ƒ {}", function.name), details);
            tree.nodes[id].function = true;
            tree.nodes[id].search_name = format!(
                "{} {}",
                function.name,
                function.linkage_name.as_deref().unwrap_or_default()
            )
            .to_lowercase();
            functions.push(id);
        }
        for (variable_id, variable) in info.variables.iter().enumerate() {
            let parent = variable
                .function
                .and_then(|index| functions.get(index).copied())
                .or_else(|| {
                    variable
                        .declaration
                        .as_ref()
                        .and_then(|loc| files.get(loc.file).copied())
                })
                .unwrap_or_else(|| {
                    *unknown.get_or_insert_with(|| {
                        tree.add(
                            None,
                            "无源码位置".into(),
                            "这些条目没有可用的源码声明位置。".into(),
                        )
                    })
                });
            let kind = match variable.kind {
                VariableKind::Global => "全局 / 静态变量",
                VariableKind::Local => "局部变量",
                VariableKind::Parameter => "参数",
            };
            let mut details = format!(
                "{kind}：{}\n类型：{}\n大小：{}\n{}\n",
                variable.name,
                variable.type_name.as_deref().unwrap_or("未记录"),
                variable
                    .byte_size
                    .map(|size| format!("{size} bytes"))
                    .unwrap_or_else(|| "未记录".into()),
                source_position(info, variable.declaration.as_ref())
            );
            if let Some(value) = &variable.constant {
                let _ = writeln!(details, "编译期常量：{value}");
            }
            details.push_str("\n存储位置：\n");
            match &variable.location {
                VariableLocation::Address(address) => {
                    let _ = writeln!(details, "静态地址 0x{address:X}");
                }
                VariableLocation::Expression(expression) => {
                    let _ = writeln!(details, "{expression}");
                }
                VariableLocation::List(locations) => {
                    for loc in locations.iter().take(1000) {
                        let _ = writeln!(
                            details,
                            "PC 0x{:X} .. 0x{:X}: {}",
                            loc.range.begin, loc.range.end, loc.expression
                        );
                    }
                    if locations.len() > 1000 {
                        details.push_str("仅预览前 1000 个位置范围。\n");
                    }
                }
                VariableLocation::Unavailable => {
                    details.push_str("不可用：编译器未记录、仅声明或已优化。\n")
                }
            }
            details.push_str("\n位置表达式需要目标寄存器、栈帧或内存上下文；这里展示静态信息。\n");
            let prefix = if variable.kind == VariableKind::Parameter {
                "p"
            } else {
                "v"
            };
            let id = tree.add(
                Some(parent),
                format!(
                    "{prefix} {} : {}",
                    variable.name,
                    variable.type_name.as_deref().unwrap_or("?")
                ),
                details,
            );
            tree.nodes[id].variable = Some(BrowserVariable {
                id: variable_id,
                name: variable.name.clone(),
                type_name: variable
                    .type_name
                    .clone()
                    .unwrap_or_else(|| "类型未记录".into()),
                members: variable.members.clone(),
                function: variable
                    .function
                    .and_then(|id| info.functions.get(id))
                    .map(|f| f.name.clone()),
                evaluation_expression: if variable.kind == VariableKind::Global {
                    variable
                        .declaration
                        .as_ref()
                        .and_then(|loc| info.files.get(loc.file))
                        .map(|file| {
                            format!(
                                "'{}'::{}",
                                file.path.replace('\\', "\\\\").replace('\'', "\\'"),
                                variable.name
                            )
                        })
                        .unwrap_or_else(|| variable.name.clone())
                } else {
                    variable.name.clone()
                },
                scope: format!(
                    "{} · {}",
                    variable
                        .function
                        .and_then(|id| info.functions.get(id))
                        .map(|function| function.name.as_str())
                        .unwrap_or(kind),
                    source_position(info, variable.declaration.as_ref()),
                ),
                base_address: match &variable.location {
                    VariableLocation::Address(address) if absolute_addresses => Some(*address),
                    _ => None,
                },
                little_endian,
                memory: match (&variable.location, variable.byte_size, variable.scalar_type) {
                    (VariableLocation::Address(address), Some(size), Some(scalar_type))
                        if absolute_addresses =>
                    {
                        usize::try_from(size).ok().and_then(|byte_size| {
                            let channel = MemoryChannel {
                                address: *address,
                                byte_size,
                                scalar_type,
                                little_endian,
                            };
                            channel.validate().ok().map(|_| channel)
                        })
                    }
                    _ => None,
                },
            });
        }
        let mut counts = vec![(0, 0); files.len()];
        for function in &info.functions {
            if let Some(count) = function
                .declaration
                .as_ref()
                .and_then(|loc| counts.get_mut(loc.file))
            {
                count.0 += 1;
            }
        }
        for variable in &info.variables {
            if let Some(count) = variable
                .declaration
                .as_ref()
                .and_then(|loc| counts.get_mut(loc.file))
            {
                count.1 += 1;
            }
        }
        for (index, file) in files.iter().enumerate() {
            let (function_count, variable_count) = counts[index];
            let _ = writeln!(
                tree.nodes[*file].details,
                "\n包含 {function_count} 个函数条目、{variable_count} 个变量/参数条目。"
            );
        }
        // Parents are created before their children, so propagate variable
        // presence upward after the complete tree has been built.
        for id in (0..tree.nodes.len()).rev() {
            tree.nodes[id].has_variables = tree.nodes[id].variable.is_some()
                || tree.nodes[id]
                    .children
                    .iter()
                    .any(|child| tree.nodes[*child].has_variables);
        }
        let labels: Vec<_> = tree.nodes.iter().map(|n| n.label.clone()).collect();
        for node in &mut tree.nodes {
            node.children.sort_by(|a, b| labels[*a].cmp(&labels[*b]));
        }
        tree.roots.sort_by(|a, b| labels[*a].cmp(&labels[*b]));
        // Reveal directory and file nodes initially; function/local branches remain collapsed.
        for id in directories.values() {
            tree.nodes[*id].expanded = true;
        }
        tree
    }

    fn add(&mut self, parent: Option<usize>, label: String, details: String) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            label,
            details,
            children: Vec::new(),
            expanded: false,
            variable: None,
            function: false,
            has_variables: false,
            search_name: String::new(),
            source_path: None,
        });
        match parent {
            Some(parent) => self.nodes[parent].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    pub fn rows(&mut self) -> Vec<BrowserRow> {
        self.visible.clear();
        let mut rows = Vec::new();
        let mut stack: Vec<_> = self.roots.iter().rev().map(|id| (*id, 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            let node = &self.nodes[id];
            if !self.node_visible(id) {
                continue;
            }
            self.visible.push(id);
            rows.push(BrowserRow {
                label: node
                    .variable
                    .as_ref()
                    .map(|v| v.name.clone())
                    .unwrap_or_else(|| node.label.clone()),
                type_name: node
                    .variable
                    .as_ref()
                    .map(|v| v.type_name.clone())
                    .unwrap_or_default(),
                depth,
                branch: self.has_visible_children(id),
                expanded: self.expanded(id),
                variable: node.variable.is_some(),
                can_plot: node.variable.as_ref().is_some_and(|v| v.memory.is_some()),
                source_path: node.source_path.clone(),
            });
            if self.expanded(id) {
                stack.extend(node.children.iter().rev().map(|id| (*id, depth + 1)));
            }
        }
        rows
    }

    pub fn select(&mut self, row: usize) -> Option<String> {
        self.select_with_toggle(row, true)
    }

    pub fn select_without_toggle(&mut self, row: usize) -> Option<String> {
        self.select_with_toggle(row, false)
    }

    fn select_with_toggle(&mut self, row: usize, toggle: bool) -> Option<String> {
        let id = *self.visible.get(row)?;
        self.selected = Some(id);
        let branch = self.has_visible_children(id);
        if branch && toggle {
            if self.query.is_empty() {
                self.nodes[id].expanded = !self.nodes[id].expanded;
            } else if !self.search_collapsed.remove(&id) {
                self.search_collapsed.insert(id);
            }
        }
        Some(self.nodes[id].details.clone())
    }

    pub fn is_branch(&self, row: usize) -> bool {
        self.visible
            .get(row)
            .is_some_and(|id| self.has_visible_children(*id))
    }

    fn node_visible(&self, id: usize) -> bool {
        if !self.query.is_empty() {
            self.matches[id]
        } else if self.show_functions {
            true
        } else {
            self.nodes[id].has_variables && !self.nodes[id].function
        }
    }

    fn has_visible_children(&self, id: usize) -> bool {
        self.nodes[id]
            .children
            .iter()
            .any(|child| self.node_visible(*child))
    }

    fn expanded(&self, id: usize) -> bool {
        if self.query.is_empty() {
            self.nodes[id].expanded
        } else {
            !self.search_collapsed.contains(&id)
        }
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.trim().replace('\\', "/").to_lowercase();
        self.selected = None;
        self.search_collapsed.clear();
        self.matches = vec![false; self.nodes.len()];
        if self.query.is_empty() {
            return;
        }
        // Include a matching file/function's contents, then retain its ancestors.
        let mut stack: Vec<_> = self.roots.iter().map(|id| (*id, false)).collect();
        while let Some((id, inherited)) = stack.pop() {
            let node = &self.nodes[id];
            let matched = inherited
                || (!node.search_name.is_empty() && node.search_name.contains(&self.query));
            self.matches[id] = matched;
            stack.extend(node.children.iter().map(|child| (*child, matched)));
        }
        for id in (0..self.nodes.len()).rev() {
            self.matches[id] |= self.nodes[id]
                .children
                .iter()
                .any(|child| self.matches[*child]);
        }
    }

    pub fn set_show_functions(&mut self, show: bool) {
        self.show_functions = show;
        self.selected = None;
    }

    pub fn selected_variable(&self) -> Option<BrowserVariable> {
        self.nodes.get(self.selected?)?.variable.clone()
    }

    pub fn variable_at(&self, row: usize) -> Option<BrowserVariable> {
        self.nodes.get(*self.visible.get(row)?)?.variable.clone()
    }

    pub fn resolve_variable(&self, name: &str) -> Option<BrowserVariable> {
        let mut matches = self
            .nodes
            .iter()
            .filter_map(|n| n.variable.as_ref())
            .filter(|v| v.name == name);
        let variable = matches.next()?.clone();
        matches.next().is_none().then_some(variable)
    }

    pub fn resolve_saved_variable(
        &self,
        name: &str,
        scope: &str,
        function: &Option<String>,
    ) -> Option<BrowserVariable> {
        let mut matches = self
            .nodes
            .iter()
            .filter_map(|node| node.variable.as_ref())
            .filter(|variable| {
                variable.name == name && variable.scope == scope && &variable.function == function
            });
        let variable = matches.next()?.clone();
        matches.next().is_none().then_some(variable)
    }
}

fn source_position(info: &FirmwareDebugInfo, location: Option<&SourceLocation>) -> String {
    match location.and_then(|loc| info.files.get(loc.file).map(|file| (loc, file))) {
        Some((loc, file)) => format!(
            "声明位置：{}:{}:{}",
            file.path,
            loc.line
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into()),
            loc.column
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        None => "声明位置：未记录".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nya_firmware_backend::NativeFirmwareBackend;
    use std::path::Path;

    #[test]
    fn expanding_files_and_functions_reveals_owned_variables_and_details() {
        let image = NativeFirmwareBackend
            .parse(
                Path::new("firmware.axf"),
                include_bytes!("../../../crates/firmware-backend/tests/fixtures/arm-dwarf4.axf"),
            )
            .unwrap();
        let mut browser = FirmwareBrowser::new(&image.debug, image.little_endian, true);
        let rows = browser.rows();
        assert!(!rows.iter().any(|r| r.label == "ƒ compute"));
        let file = rows.iter().position(|r| r.label == "main.c").unwrap();
        assert!(browser.selected_variable().is_none());
        assert_eq!(
            rows[file].source_path.as_deref(),
            Some("/firmware/source/main.c")
        );
        assert!(browser.select_without_toggle(file).is_some());
        assert!(!browser.rows()[file].expanded);
        assert!(browser.is_branch(file));
        assert!(!browser.is_branch(usize::MAX));
        assert!(browser
            .select(file)
            .unwrap()
            .contains("/firmware/source/main.c"));
        let rows = browser.rows();
        assert!(!rows.iter().any(|r| r.label == "ƒ compute"));
        assert!(rows
            .iter()
            .any(|r| r.label == "sensor" && r.type_name == "Sensor"));
        browser.set_show_functions(true);
        let rows = browser.rows();
        let compute = rows.iter().position(|r| r.label == "ƒ compute").unwrap();
        let details = browser.select(compute).unwrap();
        assert!(details.contains("main.c:8:"));
        assert!(details.contains("返回类型：int"));
        let rows = browser.rows();
        let local = rows
            .iter()
            .position(|r| r.label == "nested" && r.type_name == "int")
            .unwrap();
        assert_eq!(rows[local].depth, rows[compute].depth + 1);
        assert!(!rows[local].branch);
        assert!(!browser.is_branch(local));
        let details = browser.select(local).unwrap();
        assert!(!browser.rows()[local].expanded);
        let variable = browser.selected_variable().unwrap();
        assert_eq!(variable.name, "nested");
        assert_eq!(variable.type_name, "int");
        assert!(variable.scope.contains("compute"));
        assert!(variable.scope.contains("main.c:"));
        assert!(details.contains("局部变量：nested"));
        assert!(details.contains("FrameOffset"));
        assert!(browser.select(usize::MAX).is_none());
        browser.select(compute).unwrap();
        assert!(browser.selected_variable().is_none());
        assert!(!browser.rows().iter().any(|r| r.label == "nested"));
    }

    #[test]
    fn search_reveals_hidden_functions_and_restores_expansion() {
        let image = NativeFirmwareBackend
            .parse(
                Path::new("firmware.axf"),
                include_bytes!("../../../crates/firmware-backend/tests/fixtures/arm-dwarf4.axf"),
            )
            .unwrap();
        let mut browser = FirmwareBrowser::new(&image.debug, image.little_endian, true);
        let original: Vec<_> = browser.rows().into_iter().map(|row| row.label).collect();
        browser.set_query("COMPUTE");
        let rows = browser.rows();
        assert!(rows.iter().any(|row| row.label == "ƒ compute"));
        assert!(rows.iter().any(|row| row.label == "main.c"));
        assert!(!rows.iter().any(|row| row.label == "ƒ main"));
        let variable = rows.iter().position(|row| row.label == "nested").unwrap();
        browser.select(variable).unwrap();
        assert_eq!(browser.selected_variable().unwrap().name, "nested");
        browser.set_query("not_a_real_file_or_function");
        assert!(browser.rows().is_empty());
        assert!(browser.select(0).is_none());
        browser.set_query("  source\\main.c  ");
        assert!(browser.rows().iter().any(|row| row.label == "main.c"));
        browser.set_query("");
        assert_eq!(
            browser
                .rows()
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>(),
            original
        );
    }

    #[test]
    fn default_tree_only_shows_files_containing_variables() {
        let image = NativeFirmwareBackend
            .parse(
                Path::new("firmware.axf"),
                include_bytes!("../../../crates/firmware-backend/tests/fixtures/arm-dwarf4.axf"),
            )
            .unwrap();
        let mut info = image.debug;
        info.files.push(SourceFile {
            path: "/empty/empty.c".into(),
        });
        let mut browser = FirmwareBrowser::new(&info, true, true);
        let labels: Vec<_> = browser.rows().into_iter().map(|row| row.label).collect();
        assert!(labels.contains(&"main.c".to_string()));
        assert!(!labels.contains(&"empty.c".to_string()));
        assert!(!labels.contains(&"empty".to_string()));

        browser.set_query("empty.c");
        assert!(browser.rows().iter().any(|row| row.label == "empty.c"));
        browser.set_query("");
        assert!(!browser.rows().iter().any(|row| row.label == "empty.c"));
        browser.set_show_functions(true);
        assert!(browser.rows().iter().any(|row| row.label == "empty.c"));
    }

    #[test]
    fn identical_basenames_in_different_directories_remain_separate() {
        let info = FirmwareDebugInfo {
            files: vec![
                SourceFile {
                    path: "C:/project/a/common.c".into(),
                },
                SourceFile {
                    path: "C:/project/b/common.c".into(),
                },
            ],
            ..Default::default()
        };
        let mut browser = FirmwareBrowser::new(&info, true, true);
        browser.set_show_functions(true);
        let rows = browser.rows();
        let files: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.label == "common.c")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(files.len(), 2);
        assert!(browser.select(files[0]).unwrap().contains("a/common.c"));
        assert!(browser.select(files[1]).unwrap().contains("b/common.c"));
    }
}
