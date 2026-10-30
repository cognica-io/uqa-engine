//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture a complete source shape before publishing the retained INSERT consumer's binding.

use super::{InsertSelectConsumer, InsertSourceContext};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    assignment::targets::whole_column_with_control, ast::AssignmentTarget,
    catalog::errors::dml_storage_error_with_control, plan::InsertPlan, SQLError, ScalarExpr,
};

pub(super) enum InsertSelectColumns {
    Explicit,
    Implicit(Produced<Vec<AssignmentTarget<ScalarExpr>>>),
}

impl InsertSelectColumns {
    fn for_source(
        catalog: &dyn uqa_sql::semantics::mutation_rows::MutationRowCatalog,
        statement: &InsertPlan,
        source_columns: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        if !statement.columns.is_empty() {
            return Ok(Self::Explicit);
        }
        let mut names = ProductionVec::new(*control);
        catalog
            .visit_column_names_with_control(&statement.table, control, &mut |name| {
                names.push_produced(whole_column_with_control(name, control)?)?;
                Ok(())
            })
            .map_err(|error| dml_storage_error_with_control("INSERT SELECT", error, control))?;
        let names = names.finish()?;
        if !names.is_empty() {
            return Ok(Self::Implicit(names));
        }
        let mut names = ProductionVec::new(*control);
        names.reserve(source_columns.len())?;
        for name in source_columns {
            names.push_produced(whole_column_with_control(name, control)?)?;
        }
        Ok(Self::Implicit(names.finish()?))
    }

    pub(super) fn as_slice<'a>(
        &'a self,
        statement: &'a InsertPlan,
    ) -> &'a [AssignmentTarget<ScalarExpr>] {
        match self {
            Self::Explicit => &statement.columns,
            Self::Implicit(columns) => columns,
        }
    }
}

impl<S: Clone + 'static> InsertSelectConsumer<S> {
    pub fn begin(
        &self,
        services: InsertSourceContext<'_, S>,
        source_columns: &[String],
        _schema: &crate::RowSchema,
    ) -> Result<(), SQLError> {
        let control = self.production.control();
        control.check()?;
        let mut state = self.state.borrow_mut();
        let result_width = source_columns.len();
        let implicit_columns = state.stmt.columns.is_empty();
        let selected = InsertSelectColumns::for_source(
            services.rows.referential.assignment.rows.relations,
            &state.stmt,
            source_columns,
            &control,
        )?;
        let columns = selected.as_slice(&state.stmt);
        uqa_sql::assignment::columns::validate_mutation_targets_with_control(
            services.rows.referential.assignment.columns,
            &state.stmt.table,
            columns.iter(),
            "INSERT SELECT",
            true,
            &control,
        )?;
        if result_width > columns.len() || (!implicit_columns && result_width != columns.len()) {
            return Err(SQLError::TypeMismatch(
                control
                    .format(format_args!(
                        "INSERT SELECT width {result_width} != column count {}",
                        columns.len()
                    ))?
                    .into_parts()
                    .0,
            ));
        }
        if let (Some(existing_columns), Some(existing_width)) =
            (state.columns.as_ref(), state.result_width)
        {
            if existing_columns.as_slice(&state.stmt) != columns || existing_width != result_width {
                return Err(SQLError::Internal(
                    control
                        .copy_text(
                            "INSERT SELECT row consumer was rebound to a different source shape",
                        )?
                        .into_parts()
                        .0,
                ));
            }
            return Ok(());
        }
        let discarded_identities =
            super::super::identity_targets::user_value_identity_columns_with_control(
                services.identities,
                &state.stmt,
                columns.iter().map(|column| column.column.as_str()),
                &control,
            )?;
        let statement_relation = crate::mutation::constraints::statement_relation_with_control(
            services.rows.referential.constraints,
            &state.stmt.table,
            &control,
        )?;
        let mut supplied_columns = ProductionVec::new(control);
        for name in columns
            .iter()
            .take(result_width)
            .map(|column| &column.column)
            .chain(state.conflict_update_columns.iter())
        {
            supplied_columns.push_produced(control.copy_text(name)?)?;
        }
        let supplied_columns = supplied_columns.finish()?;
        state.discarded_identities = discarded_identities;
        state.statement_relation = statement_relation;
        state.supplied_columns = supplied_columns;
        state.columns = Some(selected);
        state.result_width = Some(result_width);
        Ok(())
    }
}
