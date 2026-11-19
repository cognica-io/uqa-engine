//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sources without structural children share bounded payload construction and retain scalar prefixes through later metadata failures.

use super::super::{
    input::{scalars::construct_scalars, text, ScalarInput, SourceMetadataInput, TextInput},
    rows::construct_rows,
    Copying, Result,
};
use crate::{
    ast::{ColumnType, Expr, FromClause, FunctionBinding, OperatorJoinRelations, TableFunction},
    plan::{retirement::Pending, SourcePlan, TableFunctionPlan},
};
use uqa_core::memory::BudgetedVec;

pub(in crate::plan::copy::lowering) trait Input<'s>:
    ScalarInput<&'s Expr>
    + TextInput<&'s str>
    + SourceMetadataInput<&'s ColumnType, &'s FunctionBinding>
{
}
impl<'s, T> Input<'s> for T where
    T: ScalarInput<&'s Expr>
        + TextInput<&'s str>
        + SourceMetadataInput<&'s ColumnType, &'s FunctionBinding>
{
}

pub(in crate::plan::copy::lowering) async fn construct<'s, I: Input<'s>>(
    source: &'s FromClause,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<SourcePlan>> {
    input.checkpoint().await?;
    copying.check()?;
    let value = match source {
        FromClause::RetainedRows(rows) => SourcePlan::RetainedRows(rows.clone()),
        FromClause::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            include_descendants,
        } => SourcePlan::Table {
            name: field!(copying, "name", |field| field
                .take(input.text(name, field.control).await?)),
            executable_query: field!(copying, "executable_query", |_field| None),
            qualifier: field!(copying, "qualifier", |field| field
                .take(input.text(qualifier, field.control).await?)),
            alias: text::optional_text(alias.as_deref(), copying, input).await?,
            column_aliases: text::texts(column_aliases, copying, input).await?,
            bound_columns: match bound_columns {
                Some(columns) => Some(text::texts(columns, copying, input).await?),
                None => None,
            },
            include_descendants: *include_descendants,
        },
        FromClause::Values {
            lateral,
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
            internal_column_collations,
        } => {
            let rows = Pending::new(field!(copying, "rows", |field| construct_rows(
                rows,
                input,
                field.control,
                field.fields.as_mut().expect("row records"),
                field.memory.as_mut().expect("row allowance"),
            )
            .await?));
            let alias = text::optional_text(alias.as_deref(), copying, input).await?;
            let column_aliases = text::texts(column_aliases, copying, input).await?;
            let internal_column_types = types(internal_column_types, copying, input).await?;
            let internal_column_collations =
                collations(internal_column_collations, copying, input).await?;
            SourcePlan::Values {
                lateral: *lateral,
                rows: rows.take(),
                alias,
                column_aliases,
                internal_relation: *internal_relation,
                internal_column_types,
                internal_column_collations,
            }
        }
        FromClause::Function { .. } => function(source, copying, input).await?,
        FromClause::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => {
            let functions = field!(copying, "functions", |field| copy_functions(
                functions, &mut field, input
            )
            .await?);
            let alias = text::optional_text(alias.as_deref(), copying, input).await?;
            let column_aliases = text::texts(column_aliases, copying, input).await?;
            SourcePlan::FunctionGroup {
                functions: functions.take(),
                alias,
                column_aliases,
                ordinality: *ordinality,
            }
        }
        FromClause::Join { .. } | FromClause::Subquery { .. } => {
            unreachable!("structural sources retain their own child-plan constructor")
        }
    };
    Ok(Pending::new(value))
}

async fn function<'s, I: Input<'s>>(
    source: &'s FromClause,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<SourcePlan> {
    let FromClause::Function {
        name,
        binding,
        output_name,
        relations,
        args,
        alias,
        column_aliases,
        ordinality,
        column_types,
    } = source
    else {
        unreachable!("single table function")
    };
    let name = copying.take(input.text(name, copying.control).await?);
    let binding = field!(copying, "binding", |field| copy_binding(
        binding.as_ref(),
        &mut field,
        input
    )
    .await?);
    let output_name = field!(copying, "output_name", |field| field
        .take(input.text(output_name, field.control).await?));
    let relations = field!(copying, "relations", |field| copy_relations(
        relations.as_ref(),
        &mut field,
        input
    )
    .await?);
    let args = field!(copying, "args", |field| construct_scalars(
        args,
        &mut field,
        input,
        |value| value,
        |_, value| value
    )
    .await?);
    let alias = text::optional_text(alias.as_deref(), copying, input).await?;
    let column_aliases = text::texts(column_aliases, copying, input).await?;
    let column_types = text::texts(column_types, copying, input).await?;
    Ok(SourcePlan::Function {
        name,
        binding,
        output_name,
        relations,
        args: args.take(),
        alias,
        column_aliases,
        ordinality: *ordinality,
        column_types,
    })
}

async fn copy_binding<'s, I: Input<'s>>(
    source: Option<&'s FunctionBinding>,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Option<FunctionBinding>> {
    match source {
        Some(source) => Ok(Some(
            copying.take(input.binding(source, copying.control).await?),
        )),
        None => Ok(None),
    }
}

async fn copy_relations<'s, I: Input<'s>>(
    source: Option<&'s OperatorJoinRelations>,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Option<OperatorJoinRelations>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let left = field!(copying, "left", |field| field
        .take(input.text(&source.left, field.control).await?));
    let right = field!(copying, "right", |field| field
        .take(input.text(&source.right, field.control).await?));
    Ok(Some(OperatorJoinRelations { left, right }))
}

async fn copy_functions<'s, I: Input<'s>>(
    source: &'s [TableFunction],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Vec<TableFunctionPlan>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = Pending::new(BudgetedVec::new(
        copying.control.budget().expect("source allowance"),
    ));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("source fields")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let mut item = copying.begin_input_field()?;
        let name = item.take(input.text(&source.name, item.control).await?);
        let binding = field!(item, "binding", |field| copy_binding(
            source.binding.as_ref(),
            &mut field,
            input
        )
        .await?);
        let output_name = field!(item, "output_name", |field| field
            .take(input.text(&source.output_name, field.control).await?));
        let relations = field!(item, "relations", |field| copy_relations(
            source.relations.as_ref(),
            &mut field,
            input
        )
        .await?);
        let args = field!(item, "args", |field| construct_scalars(
            &source.args,
            &mut field,
            input,
            |value| value,
            |_, value| value
        )
        .await?);
        let column_aliases = text::texts(&source.column_aliases, &mut item, input).await?;
        let column_types = text::texts(&source.column_types, &mut item, input).await?;
        values
            .push(TableFunctionPlan {
                name,
                binding,
                output_name,
                relations,
                args: args.take(),
                column_aliases,
                column_types,
            })
            .expect("admitted table function");
        copying.finish_input_field("item", item);
    }
    let (values, memory) = values.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(values))
}

async fn types<'s, I: Input<'s>>(
    source: &'s [Option<ColumnType>],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Vec<Option<ColumnType>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = BudgetedVec::new(copying.control.budget().expect("source allowance"));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("source fields")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let value = match source {
            Some(source) => Some(field.take(input.ty(source, field.control).await?)),
            None => None,
        };
        values.push(value).expect("admitted source type");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.into_parts();
    copying.absorb(Some(memory));
    Ok(values)
}

async fn collations<'s, I: Input<'s>>(
    source: &[Option<u32>],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Vec<Option<u32>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = BudgetedVec::new(copying.control.budget().expect("source allowance"));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("source fields")
        .reserve(source.len())?;
    for source in source {
        input.checkpoint().await?;
        let field = copying.begin_input_field()?;
        values.push(*source).expect("admitted source collation");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.into_parts();
    copying.absorb(Some(memory));
    Ok(values)
}
