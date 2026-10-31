//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-snapshot column ownership for filter pushdown.

use super::{FilterPushdownScope, SourcePlan};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    binding::production::{BindingMap, BindingNameSet},
    semantics::{
        query_plan_output_columns_with_control, visit_source_qualifiers_with_control,
        DOC_ID_COLUMN, SCORE_COLUMN, TABLE_OID_COLUMN, XMIN_COLUMN,
    },
    SQLError,
};

pub(super) type ColumnOwners = BindingMap<Option<Produced<String>>>;

pub(super) struct SourceColumns {
    owners: ColumnOwners,
    opaque: BindingNameSet,
    pub(super) qualifiers: BindingNameSet,
    unqualified_relation: bool,
}
impl SourceColumns {
    pub(super) fn new_with_control(
        scope: FilterPushdownScope<'_>,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut output = Self {
            owners: BindingMap::new(),
            opaque: BindingNameSet::new(),
            qualifiers: BindingNameSet::new(),
            unqualified_relation: false,
        };
        visit_source_qualifiers_with_control(source, control, &mut |name| {
            output.qualifiers.insert_with_control(name, control)?;
            Ok(())
        })?;
        output.collect(scope, source, control)?;
        Ok(output)
    }
    #[cfg(test)]
    pub(super) fn new(
        scope: FilterPushdownScope<'_>,
        source: &SourcePlan,
        _: &std::collections::BTreeSet<String>,
    ) -> Result<Self, SQLError> {
        Self::new_with_control(scope, source, &ProductionControl::uncontrolled())
    }
    #[cfg(test)]
    pub(super) fn listed(
        owners: std::collections::BTreeMap<String, Option<String>>,
        qualifiers: std::collections::BTreeSet<String>,
    ) -> Self {
        let control = ProductionControl::uncontrolled();
        Self {
            owners: owners
                .into_iter()
                .map(|(name, owner)| {
                    (
                        name,
                        owner.map(|owner| control.finish(owner, None).unwrap()),
                    )
                })
                .collect(),
            opaque: BindingNameSet::new(),
            qualifiers: qualifiers.into_iter().collect(),
            unqualified_relation: false,
        }
    }
    pub(super) fn owners(&self) -> &ColumnOwners {
        &self.owners
    }
    pub(super) fn only_relation(&self) -> Option<&str> {
        let mut names = self.qualifiers.iter();
        match (names.next(), names.next()) {
            (Some(name), None) => Some(name),
            _ => None,
        }
    }
    pub(super) fn reads_column_of(&self, column: &str, qualifier: &str) -> bool {
        match self.owners.get(column) {
            Some(owner) => owner.as_ref().map(|owner| owner.as_str()) == Some(qualifier),
            None => {
                self.only_relation() == Some(qualifier)
                    && !self.unqualified_relation
                    && self.opaque.contains(qualifier)
                    && !self.qualifiers.contains(column)
            }
        }
    }
    fn collect(
        &mut self,
        scope: FilterPushdownScope<'_>,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match source {
            SourcePlan::RetainedRows(_) => {}
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                ..
            } => {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                let columns = if (scope.is_visible_cte)(name) {
                    None
                } else if let Some(columns) = bound_columns {
                    Some(table_columns(columns.iter().map(String::as_str), control)?)
                } else {
                    relation_source_columns(scope, name, control)?
                };
                if let Some(columns) = columns {
                    self.register_aliases(Some(qualifier), &columns, column_aliases, control)?;
                } else {
                    self.register_opaque(qualifier, column_aliases, control)?;
                }
            }
            SourcePlan::Join {
                left, right, alias, ..
            } => match alias {
                Some(alias) => self.register_opaque(alias, &[], control)?,
                None => {
                    self.collect(scope, left, control)?;
                    self.collect(scope, right, control)?;
                }
            },
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
                internal_relation,
                ..
            } => {
                if alias.is_none() && internal_relation.is_some() {
                    return Ok(());
                }
                self.unqualified_relation |= alias.is_none();
                let width = rows.first().map_or(column_aliases.len(), Vec::len);
                for index in 0..width {
                    if let Some(column) = column_aliases.get(index) {
                        self.register(alias.as_deref(), column, control)?;
                    } else {
                        let column = control.format(format_args!("column{}", index + 1))?;
                        self.register(alias.as_deref(), &column, control)?;
                    }
                }
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
                ..
            } => {
                let output = query_plan_output_columns_with_control(body, control)?
                    .filter(|columns| !columns.iter().any(|column| column == "*"));
                match (alias, output) {
                    (Some(alias), Some(columns)) => {
                        self.register_aliases(Some(alias), &columns, column_aliases, control)?;
                    }
                    (Some(alias), None) => self.register_opaque(alias, column_aliases, control)?,
                    (None, output) => {
                        self.unqualified_relation = true;
                        if let Some(columns) = output {
                            self.register_aliases(None, &columns, &[], control)?;
                        }
                    }
                }
            }
            SourcePlan::Function { column_aliases, .. }
            | SourcePlan::FunctionGroup { column_aliases, .. } => {
                if let Some(qualifier) = source.visible_qualifier() {
                    self.register_opaque(qualifier, column_aliases, control)?;
                }
            }
        }
        Ok(())
    }
    fn register(
        &mut self,
        qualifier: Option<&str>,
        column: &str,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if let Some(owner) = self.owners.get_mut(column) {
            *owner = None;
        } else {
            self.owners.insert_with_control(
                column,
                qualifier.map(|name| control.copy_text(name)).transpose()?,
                control,
            )?;
        }
        Ok(())
    }
    fn register_aliases(
        &mut self,
        qualifier: Option<&str>,
        columns: &[String],
        aliases: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for (index, column) in columns.iter().enumerate() {
            self.register(qualifier, aliases.get(index).unwrap_or(column), control)?;
        }
        Ok(())
    }
    fn register_opaque(
        &mut self,
        qualifier: &str,
        aliases: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.opaque.insert_with_control(qualifier, control)?;
        self.register_aliases(Some(qualifier), aliases, &[], control)
    }
}
fn table_columns<'a>(
    columns: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for column in columns {
        output.push_produced(control.copy_text(column)?)?;
    }
    for pseudo in [TABLE_OID_COLUMN, XMIN_COLUMN, DOC_ID_COLUMN, SCORE_COLUMN] {
        if !output.iter().any(|column| column == pseudo) {
            output.push_produced(control.copy_text(pseudo)?)?;
        }
    }
    Ok(output.finish()?)
}
fn relation_source_columns(
    scope: FilterPushdownScope<'_>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    let FilterPushdownScope {
        catalog,
        resolution,
        ..
    } = scope;
    if catalog.sequence_exists_with_control(resolution, name, control)? {
        let mut columns = ProductionVec::new(*control);
        for name in ["last_value", "log_cnt", "is_called"] {
            columns.push_produced(control.copy_text(name)?)?;
        }
        return Ok(Some(columns.finish()?));
    }
    if let Some(table) = catalog.table_resolved_with_control(resolution, name, control)? {
        return if table.columns.is_empty() {
            Ok(None)
        } else {
            table_columns(
                table.columns.iter().map(|column| column.name.as_str()),
                control,
            )
            .map(Some)
        };
    }
    let mut view_found = false;
    let mut output = None;
    catalog.with_view_definition_with_control(resolution, name, control, &mut |view| {
        view_found = true;
        output = if let Some(columns) = view.output_columns {
            let mut output = ProductionVec::new(*control);
            for column in columns {
                output.push_produced(control.copy_text(column)?)?;
            }
            Some(output.finish()?)
        } else {
            query_plan_output_columns_with_control(view.query, control)?
        };
        Ok(())
    })?;
    if view_found {
        return Ok(output.filter(|columns| !columns.iter().any(|column| column == "*")));
    }
    catalog.with_foreign_table_columns_with_control(resolution, name, control, &mut |columns| {
        let mut names = ProductionVec::new(*control);
        for column in columns {
            names.push_produced(control.copy_text(&column.name)?)?;
        }
        output = Some(names.finish()?);
        Ok(())
    })?;
    Ok(output)
}
