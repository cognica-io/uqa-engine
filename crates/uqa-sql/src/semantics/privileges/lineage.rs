//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    context::{PrivilegeRelation, PrivilegeScope},
    ownership::{
        copy_names, extend_required, insert, require, Columns, Named, OutputColumn, Required,
        SourceLineage,
    },
};
use crate::{plan::QueryPlan, SQLError, ScalarExpr};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};

pub(super) fn opaque_columns<'a>(
    names: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Columns, SQLError> {
    Columns::collect(
        names.into_iter().map(|name| {
            Ok(OutputColumn {
                name: control.copy_text(name)?,
                sources: Required::new(),
            })
        }),
        control,
    )
}

pub(super) fn query_columns(
    query: &QueryPlan,
    aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<Columns, SQLError> {
    let mut columns = ProductionVec::new(*control);
    crate::semantics::projection::visit_query_output_columns(
        query,
        aliases,
        control,
        &mut |name| {
            columns.push_produced(control.finish(
                OutputColumn {
                    name: control.copy_text(name)?,
                    sources: Required::new(),
                },
                control.empty_reservation(),
            )?)?;
            Ok(())
        },
    )?;
    Ok(Columns::from_produced(columns.finish()?))
}

pub(super) fn opaque_lineage(
    output: Columns,
    qualifier: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<SourceLineage, SQLError> {
    let mut qualifiers = Named::new();
    if let Some(qualifier) = qualifier {
        insert(&mut qualifiers, qualifier, output.copy(control)?, control)?;
    }
    Ok(SourceLineage {
        output,
        qualifiers,
        ..SourceLineage::default()
    })
}

pub(super) fn table_lineage(
    name: &str,
    qualifier: &str,
    alias: Option<&str>,
    column_aliases: &[String],
    ctes: &PrivilegeScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<SourceLineage, SQLError> {
    let control = ctes.control();
    if ctes.is_visible_cte_with_control(name)? {
        let mut output = if let Some(columns) = ctes.materialized_for_scan(name)? {
            opaque_columns(columns.iter().map(String::as_str), &control)?
        } else if let Some(query) = ctes
            .deferred_reference_with_control(name)?
            .and_then(|cte| cte.body.query())
        {
            query_columns(query, &[], &control)?
        } else {
            Columns::default()
        };
        output.rename(column_aliases, &control)?;
        return opaque_lineage(output, Some(alias.unwrap_or(qualifier)), &control);
    }
    let Some(relation) = ctes
        .catalog
        .relation_with_control(&ctes.resolution, name, &control)?
    else {
        return Ok(SourceLineage::default());
    };
    relation_lineage(
        &relation,
        [alias.unwrap_or(qualifier)],
        column_aliases,
        &control,
    )
}

pub(super) fn relation_lineage<'a>(
    relation: &PrivilegeRelation,
    qualifiers: impl IntoIterator<Item = &'a str>,
    aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<SourceLineage, SQLError> {
    let mut output = Columns::collect(
        relation.columns.iter().map(|column| {
            let mut sources = Required::new();
            require(&mut sources, &relation.canonical, column, control)?;
            Ok(OutputColumn {
                name: control.copy_text(column)?,
                sources,
            })
        }),
        control,
    )?;
    output.rename(aliases, control)?;
    let mut lineage = SourceLineage {
        output,
        ..SourceLineage::default()
    };
    for qualifier in qualifiers {
        insert(
            &mut lineage.qualifiers,
            qualifier,
            lineage.output.copy(control)?,
            control,
        )?;
        if relation.kind.has_system_columns() {
            insert(
                &mut lineage.system_qualifiers,
                qualifier,
                copy_names([relation.canonical.as_str()], control)?,
                control,
            )?;
        }
    }
    insert(
        &mut lineage.tables,
        &relation.canonical,
        copy_names(relation.columns.iter().map(String::as_str), control)?,
        control,
    )?;
    Ok(lineage)
}

pub(super) fn join_lineage(
    left: SourceLineage,
    right: SourceLineage,
    using: Option<&crate::ast::JoinUsing>,
    natural: bool,
    alias: Option<&str>,
    column_aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<SourceLineage, SQLError> {
    let mut using_columns = ProductionVec::new(*control);
    if let Some(using) = using {
        for column in &using.columns {
            using_columns.push_copy(column.as_str())?;
        }
    } else if natural {
        for column in left
            .output
            .iter()
            .filter(|left| right.output.iter().any(|right| right.name == left.name))
        {
            using_columns.push_copy(column.name.as_str())?;
        }
    }
    let mut using_set = ProductionMap::new(*control);
    for column in using_columns.iter() {
        using_set.insert_copy(*column, ())?;
    }
    let mut output = ProductionVec::new(*control);
    for column in using_columns.iter() {
        let mut sources = Required::new();
        for candidate in left
            .output
            .iter()
            .chain(right.output.iter())
            .filter(|candidate| candidate.name.as_str() == *column)
        {
            extend_required(&mut sources, &candidate.sources, control)?;
        }
        output.push_produced(control.finish(
            OutputColumn {
                name: control.copy_text(column)?,
                sources,
            },
            control.empty_reservation(),
        )?)?;
    }
    for column in left
        .output
        .iter()
        .chain(right.output.iter())
        .filter(|column| using_set.get(&column.name.as_str()).is_none())
    {
        output
            .push_produced(control.finish(column.copy(control)?, control.empty_reservation())?)?;
    }
    let mut result = SourceLineage {
        output: Columns::from_produced(output.finish()?),
        ..SourceLineage::default()
    };
    result.include_tables(&left, control)?;
    result.include_tables(&right, control)?;
    if let Some(alias) = alias {
        result.output.rename(column_aliases, control)?;
        insert(
            &mut result.qualifiers,
            alias,
            result.output.copy(control)?,
            control,
        )?;
    } else {
        result.qualifiers = left.qualifiers;
        result.qualifiers.append(right.qualifiers);
        result.system_qualifiers = left.system_qualifiers;
        result.system_qualifiers.append(right.system_qualifiers);
    }
    Ok(result)
}

pub(super) fn values_lineage(
    rows: &[Vec<ScalarExpr>],
    alias: Option<&str>,
    column_aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<SourceLineage, SQLError> {
    let columns = if column_aliases.is_empty() {
        Columns::collect(
            (1..=rows.first().map_or(0, Vec::len)).map(|index| {
                Ok(OutputColumn {
                    name: control.format(format_args!("column{index}"))?,
                    sources: Required::new(),
                })
            }),
            control,
        )?
    } else {
        opaque_columns(column_aliases.iter().map(String::as_str), control)?
    };
    opaque_lineage(columns, alias, control)
}

pub(super) fn function_lineage(
    name: &str,
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<SourceLineage, SQLError> {
    let unqualified = name.rsplit('.').next().unwrap_or(name);
    let columns = if column_aliases.is_empty() {
        opaque_columns(
            [if output_name.is_empty() {
                unqualified
            } else {
                output_name
            }],
            control,
        )?
    } else {
        opaque_columns(column_aliases.iter().map(String::as_str), control)?
    };
    opaque_lineage(columns, Some(alias.unwrap_or(unqualified)), control)
}
