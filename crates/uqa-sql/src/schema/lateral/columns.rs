//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Missing qualified-column evidence remains separate from successful lookup.

use super::{Arc, DiagnosticNamespace, RowSchema, SQLError};

fn namespace(schema: &RowSchema, name: &str) -> DiagnosticNamespace {
    DiagnosticNamespace {
        name: name.into(),
        columns: schema
            .qualified_star_position_layout(name)
            .into_iter()
            .map(|(name, ..)| name.into())
            .collect(),
    }
}

pub(super) fn namespaces(schema: &RowSchema) -> Vec<Arc<DiagnosticNamespace>> {
    if let Some(scope) = &schema.index.cold.lateral_restrictions {
        if !scope.column_namespaces.is_empty() {
            return scope.column_namespaces.clone();
        }
    }
    let mut names = Vec::new();
    for name in schema
        .identities()
        .iter()
        .filter_map(crate::ColumnIdentity::qualifier)
        .chain(schema.relation_qualifiers())
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .map(|name| Arc::new(namespace(schema, name)))
        .collect()
}

impl RowSchema {
    pub(crate) fn missing_qualified_column(&self, qualifier: &str, column: &str) -> SQLError {
        let mut matches = Matches::default();
        for namespace in namespaces(self) {
            if namespace
                .columns
                .iter()
                .filter(|name| name.as_ref() == column)
                .count()
                > 1
            {
                return SQLError::AmbiguousColumn(column.into());
            }
            let relation_distance = distance(qualifier, &namespace.name);
            for name in &namespace.columns {
                if name.as_ref() == column && relation_distance == 0 {
                    matches.exact += 1;
                }
                let column_distance = distance(column, name);
                if column_distance <= column.len() / 2 {
                    matches.candidate(relation_distance + column_distance, &namespace.name, name);
                }
            }
        }
        let detail = match matches.exact {
            0 => None,
            1 => Some(format!("There is a column named \"{column}\" in table \"{qualifier}\", but it cannot be referenced from this part of the query.")),
            _ => Some(format!("There are columns named \"{column}\", but they are in tables that cannot be referenced from this part of the query.")),
        };
        let hint = if detail.is_some() {
            None
        } else {
            matches.hint()
        };
        SQLError::Diagnostic {
            sqlstate: "42703".into(),
            message: format!("column {qualifier}.{column} does not exist"),
            detail,
            hint,
        }
    }
}

struct Matches {
    exact: usize,
    distance: usize,
    candidates: Vec<(String, String)>,
}

impl Default for Matches {
    fn default() -> Self {
        Self {
            exact: 0,
            distance: 4,
            candidates: Vec::new(),
        }
    }
}

impl Matches {
    fn candidate(&mut self, distance: usize, qualifier: &str, column: &str) {
        if distance < self.distance {
            self.distance = distance;
            self.candidates.clear();
            self.candidates.push((qualifier.into(), column.into()));
        } else if distance == self.distance && !self.candidates.is_empty() {
            if self.candidates.len() == 2 {
                self.candidates.clear();
            } else {
                self.candidates.push((qualifier.into(), column.into()));
            }
        }
    }

    fn hint(&self) -> Option<String> {
        match self.candidates.as_slice() {
            [(qualifier, column)] => Some(format!("Perhaps you meant to reference the column \"{qualifier}.{column}\".")),
            [(q, c), (q2, c2)] => Some(format!("Perhaps you meant to reference the column \"{q}.{c}\" or the column \"{q2}.{c2}\".")),
            _ => None,
        }
    }
}

// Identifiers are short; one row of character distances suffices on the error path.
fn distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut row = (0..=right.len()).collect::<Vec<_>>();
    for (i, left) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, right) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(left != *right))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[right.len()]
}
