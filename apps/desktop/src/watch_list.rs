use crate::firmware_browser::BrowserVariable;
use nya_domain::firmware::DebugMember;

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
    nodes: Vec<WatchNode>,
    evaluation_expression: String,
    function: Option<String>,
}

struct WatchNode {
    name: String,
    expression: String,
    type_name: String,
    depth: usize,
    branch: bool,
    expanded: bool,
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
) {
    for member in members {
        let expression = if member.name.starts_with('<') {
            parent.to_owned()
        } else {
            format!("({parent}).{}", member.name)
        };
        nodes.push(WatchNode {
            name: member.name,
            expression: expression.clone(),
            type_name: member.type_name,
            depth,
            branch: !member.members.is_empty(),
            expanded: false,
        });
        append_members(nodes, member.members, depth + 1, &expression);
    }
}

#[derive(Default)]
pub struct WatchList {
    pub entries: Vec<WatchEntry>,
}

impl WatchList {
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
            },
        );
        self.insert(WatchEntry {
            key: WatchKey::Variable(variable.id),
            expression: variable.name,
            type_name: variable.type_name,
            scope: variable.scope,
            nodes,
            evaluation_expression: variable.evaluation_expression,
            function: variable.function,
        })
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
    fn nested_members_keep_order_types_and_independent_expansion() {
        let mut variable = variable(0);
        variable.name = "sensor".into();
        variable.evaluation_expression = "sensor".into();
        variable.type_name = "Sensor".into();
        variable.members = vec![
            DebugMember {
                name: "position".into(),
                type_name: "Point".into(),
                members: vec![DebugMember {
                    name: "x".into(),
                    type_name: "float".into(),
                    members: vec![],
                }],
            },
            DebugMember {
                name: "enabled".into(),
                type_name: "bool".into(),
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
        watches.toggle(0, 0);
        assert_eq!(watches.rows().len(), 1);
        watches.toggle(0, 0);
        assert_eq!(watches.rows().len(), 4);
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
}
