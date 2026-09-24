//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SET, SET LOCAL and temporary function configuration over retained parameter values.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterAssignment {
    Session,
    Local,
    Save,
}

#[derive(Clone, Copy, Debug)]
pub struct ParameterScope(usize);

/// Values remain owned by the host; SQL decides which assignment lifetime restores them.
#[derive(Clone)]
pub struct ParameterScopes<T> {
    transaction: BTreeMap<String, T>,
    functions: Vec<BTreeMap<String, T>>,
}

impl<T> Default for ParameterScopes<T> {
    fn default() -> Self {
        Self {
            transaction: BTreeMap::new(),
            functions: Vec::new(),
        }
    }
}

impl<T> ParameterScopes<T> {
    /// A session assignment supersedes pending LOCAL and function configuration restoration for this parameter.
    pub fn session_assignment(&mut self, name: &str) {
        self.transaction.remove(name);
        for saved in &mut self.functions {
            saved.remove(name);
        }
    }

    pub fn enter_function(&mut self) -> ParameterScope {
        self.functions.push(BTreeMap::new());
        ParameterScope(self.functions.len())
    }

    pub fn leave_function(&mut self, scope: ParameterScope) -> BTreeMap<String, T> {
        assert_eq!(scope.0, self.functions.len(), "parameter scope order");
        self.functions.pop().expect("active parameter scope")
    }

    /// Record a successful assignment. An out-of-transaction LOCAL returns its immediate restoration value.
    pub fn assigned(
        &mut self,
        name: String,
        previous: T,
        action: ParameterAssignment,
        in_transaction: bool,
    ) -> Option<T> {
        match action {
            ParameterAssignment::Session => {
                self.session_assignment(&name);
            }
            ParameterAssignment::Local => {
                if self.functions.iter().any(|saved| saved.contains_key(&name)) {
                    return None;
                }
                if !in_transaction {
                    return Some(previous);
                }
                self.transaction.entry(name).or_insert(previous);
            }
            ParameterAssignment::Save => {
                self.functions
                    .last_mut()
                    .expect("function configuration requires a parameter scope")
                    .entry(name)
                    .or_insert(previous);
            }
        }
        None
    }

    pub fn finish_transaction(&mut self) -> BTreeMap<String, T> {
        std::mem::take(&mut self.transaction)
    }

    /// RESET ALL overrides ordinary settings while leaving both authorization parameters alone.
    pub fn reset_all(&mut self) {
        fn authorization(name: &str) -> bool {
            matches!(name, "role" | "session_authorization")
        }
        self.transaction.retain(|name, _| authorization(name));
        for saved in &mut self.functions {
            saved.retain(|name, _| authorization(name));
        }
    }
}

#[cfg(test)]
mod tests;
