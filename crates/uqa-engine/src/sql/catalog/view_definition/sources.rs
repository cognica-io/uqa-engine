//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query namespaces and catalog-visible relation names for view SQL.

use std::fmt::Write as _;

use uqa_planner::{QueryPlan, SourcePlan, TableFunctionPlan};
use uqa_sql::ast::JoinKind;

use super::{query_columns, quote_ident, Column, Deparser, RelationIdentity, SQLError, Scope};

impl Deparser<'_> {
    pub(super) fn relation_name(&self, name: &str, scope: &Scope) -> Result<String, SQLError> {
        let (schema, local) =
            RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
        let Some(schema) = schema else {
            return Ok(quote_ident(&local));
        };
        let visible = self
            .catalog
            .relation_kind_resolution(&self.dynamic, &quote_ident(&local))?
            .into_found()
            .is_some_and(|(visible, _)| visible == name);
        let virtual_visible = schema == "pg_catalog"
            && super::super::resolve_virtual_relation(&self.dynamic, &local).is_some();
        if (visible || virtual_visible) && !scope.ctes.contains_key(&local) {
            Ok(quote_ident(&local))
        } else {
            Ok(format!("{}.{}", quote_ident(&schema), quote_ident(&local)))
        }
    }

    pub(super) fn table_columns(&self, name: &str, scope: &Scope) -> Result<Vec<String>, SQLError> {
        if let Some(columns) = cte_source_columns(scope, name) {
            return Ok(columns.clone());
        }
        if let Some(table) = self.catalog.table_resolved(&self.bound, name)? {
            return Ok(table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect());
        }
        if let Some(view) = self.catalog.view_resolved(&self.bound, name)? {
            return Ok(view
                .output_columns
                .clone()
                .unwrap_or_else(|| query_columns(&view.query)));
        }
        if let Some(table) = self.catalog.foreign_table_resolved(&self.bound, name)? {
            return Ok(table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect());
        }
        if let Some(columns) =
            super::super::virtual_relation_schema(self.catalog, &self.bound, name)?
        {
            return Ok(columns.into_iter().map(|(name, _)| name).collect());
        }
        Err(SQLError::Internal(format!(
            "view definition has no source schema for {name}"
        )))
    }

    pub(super) fn source_columns(
        &self,
        source: &SourcePlan,
        scope: &Scope,
    ) -> Result<Vec<Column>, SQLError> {
        let (names, qualifier, rendered, aliases) = match source {
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                ..
            } => {
                let local = RelationIdentity::parse_reference(name)
                    .map_err(SQLError::Internal)?
                    .1;
                (
                    self.table_columns(name, scope)?,
                    alias.as_ref().unwrap_or(qualifier).clone(),
                    alias.clone().unwrap_or(local),
                    column_aliases,
                )
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let alias = alias.clone().unwrap_or_else(|| "unnamed_subquery".into());
                (query_columns(body), alias.clone(), alias, column_aliases)
            }
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
                ..
            } => {
                let alias = alias.clone().unwrap_or_else(|| "\"*VALUES*\"".into());
                let names = (1..=rows.first().map_or(0, Vec::len))
                    .map(|index| format!("column{index}"))
                    .collect();
                (names, alias.clone(), alias, column_aliases)
            }
            SourcePlan::Function {
                name,
                output_name,
                alias,
                column_aliases,
                ordinality,
                ..
            } => {
                let output = if output_name.is_empty() {
                    name.rsplit('.').next().unwrap_or(name)
                } else {
                    output_name
                };
                let alias = alias.clone().unwrap_or_else(|| output.into());
                let mut names = vec![output.to_string()];
                if *ordinality {
                    names.push("ordinality".into());
                }
                (names, alias.clone(), alias, column_aliases)
            }
            SourcePlan::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => {
                let mut names = functions
                    .iter()
                    .flat_map(|function| {
                        if function.column_aliases.is_empty() {
                            vec![function.output_name.clone()]
                        } else {
                            function.column_aliases.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                if *ordinality {
                    names.push("ordinality".into());
                }
                let alias = alias
                    .clone()
                    .unwrap_or_else(|| functions[0].output_name.clone());
                (names, alias.clone(), alias, column_aliases)
            }
            SourcePlan::Join { .. } => return self.join_columns(source, scope),
        };
        Ok(names
            .into_iter()
            .enumerate()
            .map(|(index, name)| Column {
                name: aliases.get(index).cloned().unwrap_or(name),
                qualifier: qualifier.clone(),
                rendered_qualifier: rendered.clone(),
                merged: None,
                relation: column_relation(source, scope, index, aliases.len()),
                merged_expression: None,
            })
            .collect())
    }

    fn join_columns(&self, source: &SourcePlan, scope: &Scope) -> Result<Vec<Column>, SQLError> {
        let SourcePlan::Join {
            left,
            right,
            kind,
            using,
            natural,
            alias,
            column_aliases,
            ..
        } = source
        else {
            unreachable!()
        };

        let left = self.source_columns(left, scope)?;
        let right = self.source_columns(right, scope)?;
        let using = using.as_ref().map_or_else(
            || {
                if *natural {
                    left.iter()
                        .filter(|column| right.iter().any(|other| column.name == other.name))
                        .map(|column| column.name.clone())
                        .collect()
                } else {
                    Vec::new()
                }
            },
            |using| using.columns.clone(),
        );
        let mut columns = merged_columns(&left, &right, &using, *kind);
        if let Some(alias) = alias {
            for (index, column) in columns.iter_mut().enumerate() {
                column.qualifier.clone_from(alias);
                column.rendered_qualifier.clone_from(alias);
                column.merged = None;
                column.relation = None;
                column.merged_expression = None;
                if let Some(name) = column_aliases.get(index) {
                    column.name.clone_from(name);
                }
            }
        }
        Ok(columns)
    }

    pub(super) fn source(
        &self,
        source: &SourcePlan,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        match source {
            SourcePlan::Table {
                name,
                alias,
                column_aliases,
                include_descendants,
                ..
            } => {
                let mut rendered = self.relation_name(name, scope)?;
                if !include_descendants {
                    rendered = format!("ONLY {rendered}");
                }
                relation_alias(&mut rendered, alias.as_deref(), column_aliases);
                Ok(rendered)
            }
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let mut rendered = format!("({})", self.query(body, &scope.child(), None)?);
                relation_alias(&mut rendered, alias.as_deref(), column_aliases);
                Ok(rendered)
            }
            SourcePlan::Values {
                rows,
                alias,
                column_aliases,
                ..
            } => {
                let mut rendered = format!("( VALUES {})", self.values(rows, scope, subqueries)?);
                relation_alias(&mut rendered, alias.as_deref(), column_aliases);
                Ok(rendered)
            }
            SourcePlan::Join { .. } => self.join(source, scope, subqueries),
            SourcePlan::Function {
                name,
                binding,
                args,
                alias,
                column_aliases,
                column_types,
                ordinality,
                ..
            } => {
                let mut rendered =
                    self.function(name, binding.as_ref(), args, scope, subqueries)?;
                if *ordinality {
                    rendered.push_str(" WITH ORDINALITY");
                }
                let alias = alias
                    .as_deref()
                    .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name));
                function_alias(&mut rendered, Some(alias), column_aliases, column_types);
                Ok(rendered)
            }
            SourcePlan::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => {
                let functions = functions
                    .iter()
                    .map(|function| self.table_function(function, scope, subqueries))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut rendered = format!("ROWS FROM({})", functions.join(", "));
                if *ordinality {
                    rendered.push_str(" WITH ORDINALITY");
                }
                relation_alias(&mut rendered, alias.as_deref(), column_aliases);
                Ok(rendered)
            }
        }
    }

    fn table_function(
        &self,
        function: &TableFunctionPlan,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let mut rendered = self.function(
            &function.name,
            function.binding.as_ref(),
            &function.args,
            scope,
            subqueries,
        )?;
        function_alias(
            &mut rendered,
            None,
            &function.column_aliases,
            &function.column_types,
        );
        Ok(rendered)
    }

    fn join(
        &self,
        source: &SourcePlan,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let SourcePlan::Join {
            left,
            right,
            kind,
            on,
            using,
            alias,
            column_aliases,
            lateral,
            natural,
            ..
        } = source
        else {
            unreachable!()
        };
        let left_sql = self.source(left, scope, subqueries)?;
        let right_sql = self.source(right, scope, subqueries)?;
        let keyword = match kind {
            JoinKind::Inner => "JOIN",
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Right => "RIGHT JOIN",
            JoinKind::Full => "FULL JOIN",
            JoinKind::Cross => "CROSS JOIN",
        };
        let lateral = if *lateral { "LATERAL " } else { "" };
        let mut rendered = format!(
            "{left_sql}\n{}     {keyword} {lateral}{right_sql}",
            " ".repeat(scope.indent)
        );
        if let Some(on) = on {
            let condition = self.expression(on, scope, subqueries)?;
            if self.pretty {
                write!(rendered, " ON {condition}").expect("writing to a String cannot fail");
            } else {
                write!(rendered, " ON ({condition})").expect("writing to a String cannot fail");
            }
        } else if let Some(using) = using {
            write!(rendered, " USING ({})", identifier_list(&using.columns))
                .expect("writing to a String cannot fail");
            if let Some(alias) = &using.alias {
                write!(rendered, " AS {}", quote_ident(alias))
                    .expect("writing to a String cannot fail");
            }
        } else if *natural {
            let left = self.source_columns(left, scope)?;
            let right = self.source_columns(right, scope)?;
            let common = left
                .iter()
                .filter(|column| right.iter().any(|other| column.name == other.name))
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            if common.is_empty() {
                rendered.push_str(" ON (true)");
            } else {
                write!(rendered, " USING ({})", identifier_list(&common))
                    .expect("writing to a String cannot fail");
            }
        }
        if !self.pretty || alias.is_some() {
            rendered = format!("({rendered})");
        }
        relation_alias(&mut rendered, alias.as_deref(), column_aliases);
        Ok(rendered)
    }
}

fn merged_columns(
    left: &[Column],
    right: &[Column],
    using: &[String],
    kind: JoinKind,
) -> Vec<Column> {
    let mut columns = Vec::new();
    for name in using {
        let Some(left) = left.iter().find(|column| column.name == *name) else {
            continue;
        };
        let Some(right) = right.iter().find(|column| column.name == *name) else {
            continue;
        };
        let mut column = if kind == JoinKind::Right {
            right.clone()
        } else {
            left.clone()
        };
        if kind == JoinKind::Full {
            column.merged = Some(quote_ident(name));
            column.merged_expression = Some(uqa_planner::ScalarExpr::Func {
                name: "coalesce".into(),
                binding: None,
                args: vec![
                    uqa_planner::ScalarExpr::qualified_column(&left.qualifier, name),
                    uqa_planner::ScalarExpr::qualified_column(&right.qualifier, name),
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            });
        }
        columns.push(column);
    }
    columns.extend(
        left.iter()
            .chain(right)
            .filter(|column| !using.contains(&column.name))
            .cloned(),
    );
    // Qualified references to the two inputs remain visible even for a merged USING column.
    columns.extend(
        left.iter()
            .chain(right)
            .filter(|column| using.contains(&column.name))
            .cloned(),
    );
    columns
}

pub(super) fn source_count(source: &SourcePlan) -> usize {
    match source {
        SourcePlan::Join { left, right, .. } => source_count(left) + source_count(right),
        _ => 1,
    }
}

pub(super) fn identifier_list(names: &[String]) -> String {
    names
        .iter()
        .map(|name| quote_ident(name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn relation_alias(rendered: &mut String, alias: Option<&str>, columns: &[String]) {
    if let Some(alias) = alias {
        rendered.push(' ');
        rendered.push_str(&quote_ident(alias));
    }
    if !columns.is_empty() {
        rendered.push('(');
        rendered.push_str(&identifier_list(columns));
        rendered.push(')');
    }
}

fn function_alias(
    rendered: &mut String,
    alias: Option<&str>,
    columns: &[String],
    types: &[String],
) {
    if types.is_empty() {
        relation_alias(rendered, alias, columns);
        return;
    }
    if let Some(alias) = alias {
        rendered.push(' ');
        rendered.push_str(&quote_ident(alias));
    }
    rendered.push('(');
    rendered.push_str(
        &columns
            .iter()
            .zip(types)
            .map(|(name, ty)| format!("{} {ty}", quote_ident(name)))
            .collect::<Vec<_>>()
            .join(", "),
    );
    rendered.push(')');
}

fn column_relation(
    source: &SourcePlan,
    scope: &Scope,
    index: usize,
    alias_count: usize,
) -> Option<String> {
    match source {
        SourcePlan::Table { name, .. }
            if index >= alias_count && cte_source_columns(scope, name).is_none() =>
        {
            Some(name.clone())
        }
        _ => None,
    }
}

fn cte_source_columns<'a>(scope: &'a Scope, name: &str) -> Option<&'a Vec<String>> {
    let (schema, local) = RelationIdentity::parse_reference(name).ok()?;
    if schema.is_some() {
        return None;
    }
    scope.ctes.get(&local)
}
