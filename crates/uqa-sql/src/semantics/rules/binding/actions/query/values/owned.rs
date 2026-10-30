//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native VALUES action construction retains default provenance and every generated input under the invoking request's allowance.

use super::{overrides_identity, ValuesInput};
use crate::{
    ast::{ColumnDef, Expr, FunctionOrderSyntax},
    plan::{
        copy::{copy_syntax_with_control, SyntaxProduction},
        CommandPlan, InsertPlan, MutableCommandPlan,
    },
    production::Buffer,
    semantics::rules::analysis::{rule_returning_columns_with_control, RuleAnalysisContext},
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod source;

impl ValuesInput {
    pub fn describe_with_control(
        insert: &InsertPlan,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, SQLError> {
        control.check()?;
        if insert.source.is_some() {
            return insert
                .rule_values_input
                .as_ref()
                .map(|input| input.copy_resolved(&[], control))
                .transpose();
        }
        let width = insert.rows.first().map_or(insert.columns.len(), Vec::len);
        for row in &insert.rows {
            control.check()?;
            if row.len() != width {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "VALUES lists must all be the same length".into(),
                });
            }
        }
        let output = SyntaxProduction::new(*control);
        let mut defaults = output.vector(0)?;
        for position in 0..width {
            output.check()?;
            if insert
                .rows
                .iter()
                .any(|row| matches!(row[position], ScalarExpr::Default))
            {
                defaults.push(position)?;
            }
        }
        let defaults = defaults.finish()?;
        let value = Self {
            width,
            inline: insert.rows.len() == 1,
            unresolved: output.set_copy(defaults.iter().copied())?,
            defaults,
            written: output.set_copy((0..width).filter(|position| {
                insert
                    .rows
                    .iter()
                    .any(|row| !matches!(row[*position], ScalarExpr::Default))
            }))?,
        };
        Ok(Some(output.finish(value)?))
    }

    fn copy_resolved(
        &self,
        resolved: &[bool],
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let value = Self {
            width: self.width,
            inline: self.inline,
            defaults: output.copy(&self.defaults)?,
            unresolved: output.set_copy(
                self.unresolved
                    .iter()
                    .copied()
                    .filter(|position| !resolved.get(*position).copied().unwrap_or(false)),
            )?,
            written: output.set_copy(self.written.iter().copied())?,
        };
        output.finish(value)
    }
}

struct InputColumns {
    definitions: Produced<Vec<ColumnDef>>,
    selected: Buffer<(usize, Produced<ColumnType>)>,
}

fn input_columns(
    context: RuleAnalysisContext<'_>,
    mut command: MutableCommandPlan,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<(MutableCommandPlan, InputColumns), SQLError> {
    let definitions =
        rule_returning_columns_with_control(context, &insert(&command)?.table, control)?;
    command = command.set_implicit_insert_columns(
        definitions
            .iter()
            .take(width)
            .map(|column| column.name.as_str()),
        control,
    )?;
    let insert = insert(&command)?;
    if insert.columns.len() != width {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: if insert.columns.len() < width {
                "INSERT has more expressions than target columns"
            } else {
                "INSERT has more target columns than expressions"
            }
            .into(),
        });
    }
    let mut selected = Buffer::new(control);
    for target in &insert.columns {
        control.check()?;
        let index = definitions
            .iter()
            .position(|column| column.name == target.column)
            .ok_or_else(|| SQLError::UnknownColumn(target.column.clone()))?;
        let ty = crate::assignment::targets::assignment_input_type_with_control(
            target,
            &definitions[index].ty,
            context.composites,
            control,
        )?;
        selected.push((index, ty), control)?;
    }
    Ok((
        command,
        InputColumns {
            definitions,
            selected,
        },
    ))
}

fn insert(command: &MutableCommandPlan) -> Result<&InsertPlan, SQLError> {
    let CommandPlan::Insert(insert) = &**command else {
        return Err(SQLError::Internal(
            "VALUES action preparation requires INSERT".into(),
        ));
    };
    Ok(insert)
}

fn default_expression(
    context: RuleAnalysisContext<'_>,
    column: &ColumnDef,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Expr>>, SQLError> {
    if let Some(identity) = column
        .auto_increment
        .as_ref()
        .filter(|identity| identity.is_identity())
    {
        let sequence = identity
            .sequence
            .as_deref()
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "no owned sequence found".into(),
            })?;
        let output = SyntaxProduction::new(*control);
        let mut arguments = output.vector(1)?;
        arguments.push(Expr::Literal(Value::Str(output.text(sequence)?)))?;
        let expression = Expr::Func {
            name: output.text("nextval")?,
            binding: None,
            args: arguments.finish()?,
            order_syntax: FunctionOrderSyntax::Ordinary,
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        return Ok(Some(output.finish(expression)?));
    }
    if let Some(default) = &column.default {
        return Ok(Some(copy_syntax_with_control(default, control)?));
    }
    let mut ty = &column.ty;
    while let ColumnType::Domain { oid, base, .. } = ty {
        if let Some(domain) = context.domains.domain_by_oid_with_control(*oid, control)? {
            if let Some(default) = &domain.definition.default {
                return Ok(Some(copy_syntax_with_control(default, control)?));
            }
        }
        ty = base;
    }
    Ok(None)
}
