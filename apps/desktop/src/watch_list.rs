use crate::firmware_browser::BrowserVariable;

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
        })
    }

    pub fn add_variable(&mut self, variable: BrowserVariable) -> bool {
        self.insert(WatchEntry {
            key: WatchKey::Variable(variable.id),
            expression: variable.name,
            type_name: variable.type_name,
            scope: variable.scope,
        })
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
