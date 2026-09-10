//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only relation shapes used to bind stored rule actions.
use super::{apply_positional_aliases, cte_output_columns, BTreeMap, SQLError, CTE};
use crate::ast::{ColumnType, Statement};

pub trait RuleSourceCatalog {
    fn query_source_columns(
        &self,
        name: &str,
        relations_bound: bool,
    ) -> Result<Option<Vec<String>>, SQLError>;
    fn rule_relation_columns(&self, name: &str) -> Result<Vec<(String, ColumnType)>, SQLError>;
}

#[derive(Clone, Default)]
pub(super) struct RuleBindingContext<'a> {
    pub(super) catalog: Option<&'a dyn RuleSourceCatalog>,
    pub(super) relations_bound: bool,
    pub(super) ctes: BTreeMap<String, Vec<String>>,
}

impl<'a> RuleBindingContext<'a> {
    pub(super) fn with_catalog(catalog: &'a dyn RuleSourceCatalog, relations_bound: bool) -> Self {
        Self {
            catalog: Some(catalog),
            relations_bound,
            ctes: BTreeMap::new(),
        }
    }

    pub(super) fn relation_columns(&self, name: &str) -> Result<Vec<String>, SQLError> {
        if let Some(columns) = self.ctes.get(&name.to_ascii_lowercase()) {
            return Ok(columns.clone());
        }
        self.catalog.map_or_else(
            || Ok(Vec::new()),
            |catalog| {
                catalog
                    .query_source_columns(name, self.relations_bound)?
                    .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
            },
        )
    }

    pub(super) fn with_ctes(&self, ctes: &[CTE]) -> Result<Self, SQLError> {
        let mut context = self.clone();
        for cte in ctes {
            let key = cte.name.to_ascii_lowercase();
            if cte.recursive {
                context.ctes.entry(key.clone()).or_default();
            }
            let mut columns = cte_output_columns(&cte.body, &context)?;
            apply_positional_aliases(&mut columns, &cte.columns);
            if let Some(search) = &cte.search {
                columns.push(search.sequence_column.clone());
            }
            if let Some(cycle) = &cte.cycle {
                columns.push(cycle.mark_column.clone());
                columns.push(cycle.path_column.clone());
            }
            context.ctes.insert(key, columns);
        }
        Ok(context)
    }
}

pub fn rule_action_target_row_type(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
) -> Result<Vec<(String, ColumnType)>, SQLError> {
    let table = match action {
        Statement::Insert(statement) => &statement.table,
        Statement::Update(statement) => &statement.table,
        Statement::Delete(statement) => &statement.table,
        _ => return Ok(Vec::new()),
    };
    catalog.rule_relation_columns(table)
}
pub fn rule_action_target_columns(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
) -> Result<std::collections::BTreeSet<String>, SQLError> {
    Ok(rule_action_target_row_type(catalog, action)?
        .into_iter()
        .map(|(column, _)| column)
        .collect())
}
