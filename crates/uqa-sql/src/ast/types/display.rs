//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type names in diagnostics, as `format_type_be` spells them: a user-defined type is qualified by its schema when the running statement's search path does not include that schema. The statement executor records the search path for the statement it runs on this thread; without a recorded path, `PostgreSQL`'s default search path applies.

use std::cell::RefCell;
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

use super::ColumnType;

thread_local! {
    static SEARCH_PATH: RefCell<Option<Arc<[String]>>> = const { RefCell::new(None) };
}

/// The search path diagnostics use while a statement runs on this thread. Dropping the scope restores the enclosing statement's path, so nested SQL keeps its own.
pub struct TypeDisplayScope {
    previous: Option<Arc<[String]>>,
}

impl TypeDisplayScope {
    #[must_use]
    pub fn enter(search_path: &[String]) -> Self {
        let previous = SEARCH_PATH.with(|current| current.replace(Some(search_path.into())));
        Self { previous }
    }

    /// Record the search path of the next statement in the same scope, which an earlier statement may have changed.
    pub fn refresh(&self, search_path: &[String]) {
        SEARCH_PATH.with(|current| {
            current.replace(Some(search_path.into()));
        });
    }
}

impl Drop for TypeDisplayScope {
    fn drop(&mut self) {
        SEARCH_PATH.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

fn schema_visible(schema: &str) -> bool {
    schema == "pg_catalog"
        || SEARCH_PATH.with(|current| {
            current.borrow().as_ref().map_or_else(
                || schema == "public",
                |path| path.iter().any(|entry| entry == schema),
            )
        })
}

/// A user-defined type's name, qualified by its schema when the schema is not visible.
pub(super) fn visible_type_name(schema: &str, name: &str) -> String {
    visible_type_name_with_control(schema, name, &ProductionControl::uncontrolled())
        .expect("ordinary visible type name")
        .into_uncontrolled()
        .expect("ordinary type name has no reservation")
}

pub(super) fn visible_type_name_with_control(
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let local = crate::expr::quote_ident_with_control(name, control)?;
    if schema_visible(schema) {
        Ok(local)
    } else {
        let schema = crate::expr::quote_ident_with_control(schema, control)?;
        control.format(format_args!("{}.{}", schema.as_str(), local.as_str()))
    }
}

impl ColumnType {
    /// `format_type_be` of this type for a diagnostic.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.display_name_with_control(&ProductionControl::uncontrolled())
            .expect("ordinary diagnostic type name")
            .into_uncontrolled()
            .expect("ordinary type name has no reservation")
    }

    pub(crate) fn display_name_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.check()?;
        match self {
            ColumnType::Enum(reference) => {
                visible_type_name_with_control(&reference.schema, &reference.name, control)
            }
            ColumnType::Composite(reference) => {
                visible_type_name_with_control(&reference.schema, &reference.name, control)
            }
            ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
                visible_type_name_with_control(&reference.schema, &reference.name, control)
            }
            ColumnType::Domain { schema, name, .. } => {
                visible_type_name_with_control(schema, name, control)
            }
            // An array type of any dimensionality is the element's one array type.
            ColumnType::Array(element) => {
                let mut leaf = element.as_ref();
                while let ColumnType::Array(inner) = leaf {
                    control.check()?;
                    leaf = inner;
                }
                if matches!(
                    leaf,
                    ColumnType::Enum(_)
                        | ColumnType::Composite(_)
                        | ColumnType::Domain { .. }
                        | ColumnType::DeclaredRange(_)
                        | ColumnType::DeclaredMultirange(_)
                ) {
                    let leaf = leaf.display_name_with_control(control)?;
                    control.format(format_args!("{}[]", leaf.as_str()))
                } else {
                    self.regtype_name_with_control(control)
                }
            }
            other => other.regtype_name_with_control(control),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TypeDisplayScope;
    use crate::ast::{ColumnType, EnumTypeReference};

    #[test]
    fn hidden_user_types_are_qualified_in_diagnostics() {
        let mood = ColumnType::Enum(EnumTypeReference {
            schema: "hidden".into(),
            name: "hmood".into(),
            oid: 1,
            array_oid: 2,
        });
        // Without a statement, the default search path applies.
        assert_eq!(mood.display_name(), "hidden.hmood");
        let scope = TypeDisplayScope::enter(&["public".into()]);
        assert_eq!(mood.display_name(), "hidden.hmood");
        assert_eq!(
            ColumnType::Array(Box::new(mood.clone())).display_name(),
            "hidden.hmood[]"
        );
        scope.refresh(&["hidden".into(), "public".into()]);
        assert_eq!(mood.display_name(), "hmood");
        drop(scope);
        assert_eq!(mood.display_name(), "hidden.hmood");
        assert_eq!(ColumnType::Integer.display_name(), "integer");
    }
}
