use crate::firmware_browser::BrowserVariable;
use nya_domain::firmware::DebugMember;
use nya_domain::waveform::MemoryChannel;
use serde::{Deserialize, Serialize};
pub const MAX_CHANNELS: usize = 16;
pub const UNAVAILABLE: &str =
    "需固定地址的数值变量；栈变量、复合类型和未解析表达式不支持无暂停采集";

#[derive(PartialEq, Eq)]
enum WatchKey {
    Expression(String),
    Variable(usize),
}

pub struct WatchEntry {
    key: WatchKey,
    pub expression: String,
    pub type_name: String,
    pub scope: String,
    pub memory: Option<MemoryChannel>,
    pub waveform: bool,
    nodes: Vec<WatchNode>,
    evaluation_expression: String,
    function: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedWatch {
    pub expression: String,
    pub scope: Option<String>,
    pub function: Option<String>,
    pub waveform: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waveform_members: Vec<String>,
}

impl WatchEntry {
    pub fn channel_id(&self, node: usize) -> Option<usize> {
        match self.key {
            WatchKey::Variable(id) if node <= 4096 => id.checked_mul(4097)?.checked_add(node),
            WatchKey::Variable(_) => None,
            WatchKey::Expression(_) => None,
        }
    }

    pub fn node_memory(&self, node: usize) -> Option<&MemoryChannel> {
        if node == 0 {
            self.memory.as_ref()
        } else {
            self.nodes.get(node)?.memory.as_ref()
        }
    }

    pub fn node_waveform(&self, node: usize) -> bool {
        if node == 0 {
            self.waveform
        } else {
            self.nodes.get(node).is_some_and(|n| n.waveform)
        }
    }

    pub fn waveform_nodes(&self) -> impl Iterator<Item = (usize, &str, &MemoryChannel)> {
        self.nodes.iter().enumerate().filter_map(|(index, node)| {
            self.node_waveform(index)
                .then_some((index, node.path.as_str(), node.memory.as_ref()?))
        })
    }
}

struct WatchNode {
    name: String,
    expression: String,
    type_name: String,
    depth: usize,
    branch: bool,
    expanded: bool,
    memory: Option<MemoryChannel>,
    waveform: bool,
    path: String,
}

pub struct DisplayRow {
    pub name: String,
    pub expression: String,
    pub function: Option<String>,
    pub type_name: String,
    pub scope: String,
    pub entry: usize,
    pub node: usize,
    pub depth: usize,
    pub branch: bool,
    pub expanded: bool,
}

fn append_members(
    nodes: &mut Vec<WatchNode>,
    members: Vec<DebugMember>,
    depth: usize,
    parent: &str,
    parent_path: &str,
    base: Option<u64>,
    little_endian: bool,
) {
    for member in members {
        let address = base
            .zip(member.offset)
            .and_then(|(base, offset)| base.checked_add(offset));
        let memory = address
            .zip(member.byte_size)
            .zip(member.scalar_type)
            .and_then(|((address, size), scalar_type)| {
                let channel = MemoryChannel {
                    address,
                    byte_size: usize::try_from(size).ok()?,
                    scalar_type,
                    little_endian,
                };
                channel.validate().ok().map(|_| channel)
            });
        let expression = if member.name.starts_with('<') {
            parent.to_owned()
        } else {
            format!("({parent}).{}", member.name)
        };
        let path = if member.name.starts_with('<') {
            parent_path.to_owned()
        } else {
            format!("{parent_path}.{}", member.name)
        };
        nodes.push(WatchNode {
            name: member.name,
            expression: expression.clone(),
            type_name: member.type_name,
            depth,
            branch: !member.members.is_empty(),
            expanded: false,
            memory,
            waveform: false,
            path: path.clone(),
        });
        append_members(
            nodes,
            member.members,
            depth + 1,
            &expression,
            &path,
            address,
            little_endian,
        );
    }
}

#[derive(Default)]
pub struct WatchList {
    pub entries: Vec<WatchEntry>,
}

impl WatchList {
    pub fn saved(&self) -> Vec<SavedWatch> {
        self.entries
            .iter()
            .map(|entry| SavedWatch {
                expression: entry.expression.clone(),
                scope: match entry.key {
                    WatchKey::Variable(_) => Some(entry.scope.clone()),
                    WatchKey::Expression(_) => None,
                },
                function: entry.function.clone(),
                waveform: entry.waveform,
                waveform_members: entry
                    .nodes
                    .iter()
                    .filter(|node| node.waveform)
                    .map(|node| node.path.clone())
                    .collect(),
            })
            .collect()
    }

    pub fn restore(
        &mut self,
        saved: &[SavedWatch],
        browser: Option<&crate::firmware_browser::FirmwareBrowser>,
    ) -> usize {
        self.entries.clear();
        let mut missing = 0;
        for item in saved {
            if let Some(scope) = &item.scope {
                if let Some(variable) = browser
                    .and_then(|b| b.resolve_saved_variable(&item.expression, scope, &item.function))
                {
                    let variable_id = variable.id;
                    if item.waveform {
                        if self.add_waveform(variable).is_err() {
                            missing += 1;
                        }
                    } else {
                        self.add_variable(variable);
                    }
                    let Some(index) = self
                        .entries
                        .iter()
                        .position(|entry| entry.key == WatchKey::Variable(variable_id))
                    else {
                        continue;
                    };
                    for path in &item.waveform_members {
                        let node = self.entries.get(index).and_then(|entry| {
                            let mut matches = entry
                                .nodes
                                .iter()
                                .enumerate()
                                .filter(|(_, node)| &node.path == path);
                            let found = matches.next()?.0;
                            matches.next().is_none().then_some(found)
                        });
                        if node.is_none_or(|node| self.toggle_waveform(index, node).is_err()) {
                            missing += 1;
                        }
                    }
                } else {
                    missing += 1;
                }
            } else {
                self.add_expression(&item.expression);
            }
        }
        missing
    }
    pub fn add_expression(&mut self, expression: &str) -> bool {
        let expression = expression.trim();
        if expression.is_empty() {
            return false;
        }
        self.insert(WatchEntry {
            key: WatchKey::Expression(expression.into()),
            expression: expression.into(),
            type_name: "类型待求值".into(),
            scope: "手动表达式".into(),
            memory: None,
            waveform: false,
            nodes: Vec::new(),
            evaluation_expression: expression.into(),
            function: None,
        })
    }

    pub fn add_variable(&mut self, variable: BrowserVariable) -> bool {
        let mut nodes = Vec::new();
        append_members(
            &mut nodes,
            variable.members,
            1,
            &variable.evaluation_expression,
            &variable.name,
            variable.base_address,
            variable.little_endian,
        );
        nodes.insert(
            0,
            WatchNode {
                name: variable.name.clone(),
                expression: variable.evaluation_expression.clone(),
                type_name: variable.type_name.clone(),
                depth: 0,
                branch: !nodes.is_empty(),
                expanded: true,
                memory: variable.memory.clone(),
                waveform: false,
                path: variable.name.clone(),
            },
        );
        self.insert(WatchEntry {
            key: WatchKey::Variable(variable.id),
            expression: variable.name,
            type_name: variable.type_name,
            scope: variable.scope,
            memory: variable.memory,
            waveform: false,
            nodes,
            evaluation_expression: variable.evaluation_expression,
            function: variable.function,
        })
    }

    pub fn add_waveform(&mut self, variable: BrowserVariable) -> Result<(), &'static str> {
        if variable.memory.is_none() {
            return Err(UNAVAILABLE);
        }
        let index = self
            .entries
            .iter()
            .position(|entry| entry.key == WatchKey::Variable(variable.id));
        if self.waveform_count() >= MAX_CHANNELS && !index.is_some_and(|i| self.entries[i].waveform)
        {
            return Err("最多同时采集 16 个通道");
        }
        let index = index.unwrap_or_else(|| {
            self.add_variable(variable);
            self.entries.len() - 1
        });
        self.entries[index].waveform = true;
        Ok(())
    }

    fn waveform_count(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| {
                usize::from(entry.waveform)
                    + entry.nodes.iter().filter(|node| node.waveform).count()
            })
            .sum()
    }

    pub fn toggle_waveform(&mut self, index: usize, node: usize) -> Result<(), &'static str> {
        let entry = self.entries.get(index).ok_or("监视变量不存在")?;
        if entry.node_memory(node).is_none() || entry.channel_id(node).is_none() {
            return Err(UNAVAILABLE);
        }
        if !entry.node_waveform(node) && self.waveform_count() >= MAX_CHANNELS {
            return Err("最多同时采集 16 个通道");
        }
        if node == 0 {
            self.entries[index].waveform = !self.entries[index].waveform;
        } else {
            self.entries[index].nodes[node].waveform = !self.entries[index].nodes[node].waveform;
        }
        Ok(())
    }

    pub fn rows(&self) -> Vec<DisplayRow> {
        let mut rows = Vec::new();
        for (entry_index, entry) in self.entries.iter().enumerate() {
            if entry.nodes.is_empty() {
                rows.push(DisplayRow {
                    name: entry.expression.clone(),
                    expression: entry.evaluation_expression.clone(),
                    function: entry.function.clone(),
                    type_name: entry.type_name.clone(),
                    scope: entry.scope.clone(),
                    entry: entry_index,
                    node: 0,
                    depth: 0,
                    branch: false,
                    expanded: false,
                });
                continue;
            }
            let mut collapsed_depth = None;
            for (node_index, node) in entry.nodes.iter().enumerate() {
                if collapsed_depth.is_some_and(|depth| node.depth > depth) {
                    continue;
                }
                collapsed_depth = if node.branch && !node.expanded {
                    Some(node.depth)
                } else {
                    None
                };
                rows.push(DisplayRow {
                    name: node.name.clone(),
                    expression: node.expression.clone(),
                    function: entry.function.clone(),
                    type_name: node.type_name.clone(),
                    scope: entry.scope.clone(),
                    entry: entry_index,
                    node: node_index,
                    depth: node.depth,
                    branch: node.branch,
                    expanded: node.expanded,
                });
            }
        }
        rows
    }

    pub fn toggle(&mut self, entry: usize, node: usize) {
        if let Some(node) = self
            .entries
            .get_mut(entry)
            .and_then(|entry| entry.nodes.get_mut(node))
        {
            if node.branch {
                node.expanded = !node.expanded;
            }
        }
    }

    fn insert(&mut self, entry: WatchEntry) -> bool {
        if self
            .entries
            .iter()
            .any(|existing| existing.key == entry.key)
        {
            return false;
        }
        self.entries.push(entry);
        true
    }

    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.entries.len() {
            return false;
        }
        self.entries.remove(index);
        true
    }

    pub fn reset_firmware(&mut self) {
        self.entries
            .retain(|entry| matches!(entry.key, WatchKey::Expression(_)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(id: usize) -> BrowserVariable {
        BrowserVariable {
            id,
            name: "counter".into(),
            type_name: "int".into(),
            scope: format!("main.c:{}", id + 1),
            memory: Some(MemoryChannel {
                address: 0x20000000 + id as u64 * 4,
                byte_size: 4,
                scalar_type: nya_domain::waveform::ScalarType::Signed,
                little_endian: true,
            }),
            base_address: Some(0x20000000 + id as u64 * 4),
            little_endian: true,
            members: Vec::new(),
            evaluation_expression: "counter".into(),
            function: None,
        }
    }

    #[test]
    fn expressions_are_trimmed_deduplicated_and_removable() {
        let mut watches = WatchList::default();
        assert!(!watches.add_expression("  "));
        assert!(watches.add_expression(" counter + 1 "));
        assert!(!watches.add_expression("counter + 1"));
        assert_eq!(watches.entries[0].expression, "counter + 1");
        assert!(!watches.remove(usize::MAX));
        assert!(watches.remove(0));
        assert!(watches.entries.is_empty());
    }

    #[test]
    fn member_waveforms_restore_and_share_channel_limit() {
        use nya_domain::firmware::{
            DebugVariable, FirmwareDebugInfo, VariableKind, VariableLocation,
        };
        let info = FirmwareDebugInfo {
            variables: vec![DebugVariable {
                name: "device".into(),
                type_name: Some("Device".into()),
                byte_size: Some(80),
                scalar_type: None,
                declaration: None,
                function: None,
                kind: VariableKind::Global,
                location: VariableLocation::Address(0x20000000),
                constant: None,
                members: (0..17)
                    .map(|i| DebugMember {
                        name: format!("value{i}"),
                        type_name: "int".into(),
                        offset: Some(i * 4),
                        byte_size: Some(4),
                        scalar_type: Some(nya_domain::waveform::ScalarType::Signed),
                        members: vec![],
                    })
                    .collect(),
            }],
            ..Default::default()
        };
        let browser = crate::firmware_browser::FirmwareBrowser::new(&info, true, true);
        let mut watches = WatchList::default();
        assert!(watches.add_variable(browser.resolve_variable("device").unwrap()));
        for node in 1..=MAX_CHANNELS {
            watches.toggle_waveform(0, node).unwrap();
        }
        assert!(watches.toggle_waveform(0, 17).is_err());
        let saved = watches.saved();
        let mut restored = WatchList::default();
        assert_eq!(restored.restore(&saved, Some(&browser)), 0);
        assert_eq!(restored.entries[0].waveform_nodes().count(), MAX_CHANNELS);
        assert_eq!(
            restored.saved()[0].waveform_members,
            saved[0].waveform_members
        );
        restored.toggle_waveform(0, 1).unwrap();
        restored.toggle_waveform(0, 17).unwrap();
        assert!(restored.entries[0].node_waveform(17));
    }

    #[test]
    fn nested_members_keep_order_types_and_independent_expansion() {
        let mut variable = variable(0);
        variable.name = "sensor".into();
        variable.evaluation_expression = "sensor".into();
        variable.type_name = "Sensor".into();
        variable.members = vec![
            DebugMember {
                name: "position".into(),
                type_name: "Point".into(),
                offset: Some(4),
                byte_size: None,
                scalar_type: None,
                members: vec![DebugMember {
                    name: "x".into(),
                    type_name: "float".into(),
                    offset: Some(8),
                    byte_size: Some(4),
                    scalar_type: Some(nya_domain::waveform::ScalarType::Float),
                    members: vec![],
                }],
            },
            DebugMember {
                name: "enabled".into(),
                type_name: "bool".into(),
                offset: Some(0),
                byte_size: Some(1),
                scalar_type: Some(nya_domain::waveform::ScalarType::Boolean),
                members: vec![],
            },
        ];
        let mut watches = WatchList::default();
        assert!(watches.add_variable(variable));
        let rows = watches.rows();
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["sensor", "position", "enabled"]
        );
        assert_eq!(rows[1].type_name, "Point");
        watches.toggle(0, rows[1].node);
        let rows = watches.rows();
        assert_eq!(rows[2].name, "x");
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[2].expression, "((sensor).position).x");
        assert_eq!(rows[2].type_name, "float");
        assert_eq!(
            watches.entries[0]
                .node_memory(rows[2].node)
                .unwrap()
                .address,
            0x2000000c
        );
        assert_ne!(
            watches.entries[0].channel_id(0),
            watches.entries[0].channel_id(rows[2].node)
        );
        let member = rows[2].node;
        watches.toggle_waveform(0, member).unwrap();
        assert_eq!(watches.saved()[0].waveform_members, ["sensor.position.x"]);
        watches.toggle(0, 0);
        assert_eq!(watches.rows().len(), 1);
        let plots: Vec<_> = watches.entries[0].waveform_nodes().collect();
        assert_eq!(plots.len(), 1);
        assert_eq!(plots[0].1, "sensor.position.x");
        assert_eq!(plots[0].2.address, 0x2000000c);
        watches.toggle(0, 0);
        assert_eq!(watches.rows().len(), 4);
        watches.toggle_waveform(0, member).unwrap();
        assert_eq!(watches.entries[0].waveform_nodes().count(), 0);
        assert!(watches.remove(0));
        assert!(watches.rows().is_empty());
    }

    #[test]
    fn same_named_variables_keep_their_identity_and_reset_with_firmware() {
        let mut watches = WatchList::default();
        assert!(watches.add_variable(variable(0)));
        assert!(!watches.add_variable(variable(0)));
        assert!(watches.add_variable(variable(1)));
        assert!(watches.add_expression("counter"));
        assert_eq!(watches.entries.len(), 3);
        assert_ne!(watches.entries[0].scope, watches.entries[1].scope);
        watches.reset_firmware();
        assert_eq!(watches.entries.len(), 1);
        assert_eq!(watches.entries[0].scope, "手动表达式");
        assert!(watches.add_variable(variable(0)));
    }
    #[test]
    fn waveform_entries_share_watch_identity_and_enforce_capacity() {
        let mut watches = WatchList::default();
        assert!(watches.add_variable(variable(0)));
        watches.add_waveform(variable(0)).unwrap();
        watches.add_waveform(variable(0)).unwrap();
        assert_eq!(watches.entries.len(), 1);
        assert!(watches.entries[0].waveform);
        watches.toggle_waveform(0, 0).unwrap();
        assert!(!watches.entries[0].waveform);
        for id in 0..MAX_CHANNELS {
            watches.add_waveform(variable(id)).unwrap();
        }
        assert!(watches.add_waveform(variable(MAX_CHANNELS)).is_err());
        assert_eq!(watches.entries.len(), MAX_CHANNELS);
        watches.add_waveform(variable(0)).unwrap();
        watches.toggle_waveform(0, 0).unwrap();
        watches.add_waveform(variable(MAX_CHANNELS)).unwrap();
        assert!(watches.toggle_waveform(0, 0).is_err());
        assert!(!watches.entries[0].waveform);
        assert!(watches.add_expression("counter + 1"));
        assert_eq!(
            watches.toggle_waveform(watches.entries.len() - 1, 0),
            Err(UNAVAILABLE)
        );
        let mut local = variable(99);
        local.memory = None;
        assert_eq!(watches.add_waveform(local), Err(UNAVAILABLE));
        assert!(watches.toggle_waveform(usize::MAX, 0).is_err());
        watches.reset_firmware();
        assert_eq!(watches.entries.len(), 1);
        assert!(!watches.entries[0].waveform);
    }
}
