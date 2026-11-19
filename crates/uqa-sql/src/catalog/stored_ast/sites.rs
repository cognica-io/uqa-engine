//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Carry what binding recorded for stored syntax back into that syntax: exact routine identities and ordering, the OID identities of user-defined cast types, and the enum constants that binding converted from `unknown` literals.

use super::{
    input::{Actions, Reader},
    routines::apply_routine_reference_from,
    BTreeSet, Expr, SQLError, Statement,
};
use crate::binding::{
    stored_routines::StoredCallOrigin,
    syntax_sites::{SyntaxSites, ValueSite},
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl},
    Value,
};

/// Apply the sites of a bound copy of `expression`. Returns whether the syntax changed.
pub fn bind_stored_expression_sites(
    expression: &mut Expr,
    sites: &SyntaxSites,
) -> Result<bool, SQLError> {
    StoredCallOrigin::Definition.bind_expression_sites(expression, sites)
}

/// Apply the sites of a bound copy of `statement`. Returns whether the syntax changed.
pub fn bind_stored_statement_sites(
    statement: &mut Statement,
    sites: &SyntaxSites,
) -> Result<bool, SQLError> {
    StoredCallOrigin::Definition.bind_statement_sites(statement, sites)
}

impl StoredCallOrigin {
    pub(crate) fn bind_expression_sites(
        self,
        expression: &mut Expr,
        sites: &SyntaxSites,
    ) -> Result<bool, SQLError> {
        apply_sites(sites, Syntax::Expression(expression), self)
    }

    pub(crate) fn bind_statement_sites(
        self,
        statement: &mut Statement,
        sites: &SyntaxSites,
    ) -> Result<bool, SQLError> {
        apply_sites(sites, Syntax::Statement(statement), self)
    }
    /// The caller keeps the returned lease with its edited syntax until the field is reprojected.
    pub(crate) async fn bind_expression_sites_with_work<
        W: crate::catalog::work::Work + crate::plan::ScalarInputWork,
    >(
        self,
        expression: &mut Expr,
        sites: &SyntaxSites,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<bool>, SQLError>
    where
        W::Error: Into<SQLError> + Send,
    {
        apply_from(sites, Syntax::Expression(expression), self, control, work).await
    }

    pub(crate) async fn bind_statement_sites_with_work<
        W: crate::catalog::work::Work + crate::plan::ScalarInputWork,
    >(
        self,
        statement: &mut Statement,
        sites: &SyntaxSites,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Produced<bool>, SQLError>
    where
        W::Error: Into<SQLError> + Send,
    {
        apply_from(sites, Syntax::Statement(statement), self, control, work).await
    }
}

enum Syntax<'a> {
    Expression(&'a mut Expr),
    Statement(&'a mut Statement),
}

fn apply_sites(
    sites: &SyntaxSites,
    syntax: Syntax<'_>,
    origin: StoredCallOrigin,
) -> Result<bool, SQLError> {
    let control = ProductionControl::uncontrolled();
    let edited = match syntax {
        Syntax::Expression(expression) => crate::catalog::work::inline(
            origin.bind_expression_sites_with_work(expression, sites, &control, &mut ()),
        ),
        Syntax::Statement(statement) => crate::catalog::work::inline(
            origin.bind_statement_sites_with_work(statement, sites, &control, &mut ()),
        ),
    }?;
    Ok(edited.into_uncontrolled().expect("ordinary syntax binding"))
}

struct Selected<'a> {
    routines: std::slice::Iter<'a, crate::binding::stored_routines::BoundRoutineReference>,
    values: std::iter::Peekable<std::slice::Iter<'a, ValueSite>>,
    origin: StoredCallOrigin,
    changed: bool,
    memory: Option<MemoryReservation>,
}

impl Selected<'_> {
    fn take<T>(&mut self, produced: Produced<T>, control: &ProductionControl<'_>) -> T {
        let (value, lease) = produced.into_parts();
        self.memory = control.combine(self.memory.take(), lease);
        value
    }
}

async fn apply_from<W: crate::catalog::work::Work + crate::plan::ScalarInputWork>(
    sites: &SyntaxSites,
    syntax: Syntax<'_>,
    origin: StoredCallOrigin,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<bool>, SQLError>
where
    W::Error: Into<SQLError> + Send,
{
    let mut selected = Selected {
        routines: sites.routines.iter(),
        values: sites.values.iter().peekable(),
        origin,
        changed: false,
        memory: control.empty_reservation(),
    };
    let mut reader = Reader::new(&mut selected, control, work);
    match syntax {
        Syntax::Expression(expression) => reader.bind_expr(expression, &BTreeSet::new()).await?,
        Syntax::Statement(statement) => reader.bind_statement(statement).await?,
    }
    if let Some(reference) = selected.routines.next() {
        return Err(SQLError::Internal(format!(
            "stored catalog routine binding entry `{}` has no matching call",
            reference.name
        )));
    }
    if selected.values.next().is_some() {
        return Err(SQLError::Internal(
            "stored catalog binding has an expression site without matching syntax".into(),
        ));
    }
    Ok(control.finish(selected.changed, selected.memory)?)
}

impl<W: crate::catalog::work::Work + crate::plan::ScalarInputWork> Actions<W> for Selected<'_>
where
    W::Error: Into<SQLError> + Send,
{
    async fn routine(
        &mut self,
        name: &mut String,
        binding: Option<&mut Option<crate::ast::FunctionBinding>>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let reference = self.routines.next().ok_or_else(|| {
            SQLError::Internal(format!(
                "stored catalog routine binding has no entry for call `{name}`"
            ))
        })?;
        let result =
            apply_routine_reference_from(name, binding, reference, self.origin, control, work)
                .await?;
        let changed = self.take(result, control);
        self.changed |= changed;
        Ok(())
    }
    async fn expression(
        &mut self,
        node: &mut Expr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let changed = self.apply_value_site(node, control, work).await?;
        self.changed |= changed;
        Ok(())
    }
}

fn mismatch(what: &str) -> SQLError {
    SQLError::Internal(format!(
        "stored catalog binding does not match the {what} of its syntax"
    ))
}

impl Selected<'_> {
    async fn apply_value_site<W: crate::catalog::work::Work + crate::plan::ScalarInputWork>(
        &mut self,
        node: &mut Expr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<bool, SQLError>
    where
        W::Error: Into<SQLError> + Send,
    {
        let mut changed = false;
        loop {
            crate::catalog::work::ReferenceWork { control, work }
                .checkpoint()
                .await?;
            if let Some(ValueSite::DiscardCollation) = self.values.peek() {
                self.values.next();
                if !crate::catalog::collations::expression::discard_ast_clause(node)? {
                    return Err(mismatch("discarded collation"));
                }
                changed = true;
                continue;
            }
            if let Some(ValueSite::RowDistinct) = self.values.peek() {
                self.values.next();
                crate::type_resolution::row_distinct::rewrite_ast(node)?;
                changed = true;
                continue;
            }
            if let Some(ValueSite::CompositeRow(binding)) = self.values.peek() {
                let copy = binding
                    .clone_with_input_work(control, work)
                    .await
                    .map_err(Into::into)?;
                let binding = self.take(copy, control);
                self.values.next();
                return bind_composite_constructor(node, binding).map(|field| changed || field);
            }
            if let Some(ValueSite::Membership { array_items }) = self.values.peek() {
                let mut positions = uqa_core::memory::ProductionVec::new(*control);
                positions.reserve(array_items.len())?;
                for chunk in array_items.chunks(4096 / size_of::<usize>()) {
                    crate::catalog::work::ReferenceWork { control, work }
                        .checkpoint()
                        .await?;
                    positions.extend_from_slice(chunk)?;
                }
                let (array_items, _positions) = positions.finish()?.into_parts();
                let shape = crate::type_resolution::membership::MembershipShape { array_items };
                self.values.next();
                let mut original = std::mem::replace(node, Expr::Star);
                let Expr::InList {
                    expr,
                    list,
                    negated,
                } = &mut original
                else {
                    return Err(mismatch("membership comparison"));
                };
                self.memory =
                    control.combine(self.memory.take(), control.reserve(size_of::<Expr>())?);
                let result = crate::type_resolution::membership::rewrite(
                    Box::new(std::mem::replace(expr.as_mut(), Expr::Star)),
                    std::mem::take(list),
                    *negated,
                    &shape,
                    control,
                )?;
                *node = self.take(result, control);
                changed = true;
                continue;
            }
            // A relabel wraps the node, whose own site the visitor reads when it descends into the wrapped node.
            if let Some(ValueSite::Relabel(ty)) = self.values.peek() {
                let spelling = crate::catalog::work::Work::text(work, ty, control).await?;
                let ty = self.take(spelling, control);
                self.values.next();
                let inner = std::mem::replace(node, Expr::Literal(Value::Null));
                self.memory =
                    control.combine(self.memory.take(), control.reserve(size_of::<Expr>())?);
                *node = Expr::Cast {
                    implicit: true,
                    expr: Box::new(inner),
                    ty,
                };
                return Ok(true);
            }
            return match node {
                Expr::Cast { ty, .. } => {
                    let Some(ValueSite::Cast(bound)) = self.values.next() else {
                        return Err(mismatch("cast"));
                    };
                    // The bound name keeps the selected catalog identity independently of the caller's later search_path.
                    if ty != bound {
                        let spelling =
                            crate::catalog::work::Work::text(work, bound, control).await?;
                        *ty = self.take(spelling, control);
                        return Ok(true);
                    }
                    Ok(changed)
                }
                Expr::Literal(Value::Str(_) | Value::Null) => match self.values.next() {
                    Some(ValueSite::Literal) => Ok(changed),
                    Some(ValueSite::Constant { value, ty }) => {
                        let value = crate::plan::ScalarInputWork::value(work, value, control)
                            .await
                            .map_err(Into::into)?;
                        let ty = crate::catalog::work::Work::text(work, ty, control).await?;
                        *node = Expr::TypedLiteral {
                            collation_oid: None,
                            composite_source: None,
                            value: self.take(value, control),
                            ty: self.take(ty, control),
                        };
                        Ok(true)
                    }
                    _ => Err(mismatch("literal")),
                },
                Expr::Func { order_syntax, .. } => {
                    let Some(ValueSite::FunctionOrder(bound)) = self.values.next() else {
                        return Err(mismatch("function ordering"));
                    };
                    if order_syntax == bound {
                        return Ok(changed);
                    }
                    if !order_syntax.is_legacy() {
                        return Err(mismatch("function ordering"));
                    }
                    *order_syntax = *bound;
                    Ok(true)
                }
                _ => match self.values.next() {
                    Some(ValueSite::Node) => Ok(changed),
                    _ => Err(mismatch("expression")),
                },
            };
        }
    }
}

fn bind_composite_constructor(
    node: &mut Expr,
    binding: crate::ast::CompositeRowBinding,
) -> Result<bool, SQLError> {
    if let Expr::CompositeRow {
        binding: current, ..
    } = node
    {
        let changed = *current != binding;
        *current = binding;
        return Ok(changed);
    }
    let mut original = std::mem::replace(node, Expr::Star);
    let mut row = match &mut original {
        Expr::Cast { expr, .. } => std::mem::replace(expr.as_mut(), Expr::Star),
        _ => original,
    };
    while let Expr::Cast { expr, ty, .. } = &mut row {
        if crate::ColumnType::from_sql_name(ty).ok() != Some(crate::ColumnType::Record) {
            break;
        }
        row = std::mem::replace(expr.as_mut(), Expr::Star);
    }
    let Expr::Row(items) = &mut row else {
        return Err(SQLError::Internal(
            "stored catalog binding does not match the composite constructor of its syntax".into(),
        ));
    };
    *node = Expr::CompositeRow {
        items: std::mem::take(items),
        binding,
    };
    Ok(true)
}

#[cfg(test)]
mod tests;

/// Assignment of a stored `unknown` literal to `target`, as `coerce_to_target_type` converts an untyped constant with the target type's input function when a default, generation expression or result is analyzed: the literal becomes a constant of the enum or enum-array type, or of that base type of a domain, whose own check applies when the value is assigned. Any other expression is coerced when it is evaluated.
pub fn fold_assigned_stored_literal(
    expression: &mut Expr,
    target: &crate::ast::ColumnType,
    catalog: Option<&dyn crate::expr::enums::EnumLabelCatalog>,
) -> Result<bool, SQLError> {
    crate::catalog::work::inline(fold_assigned_stored_literal_from(
        expression,
        target,
        &|| catalog,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut (),
    ))
    .map(|changed| *changed)
}

pub(crate) async fn fold_assigned_stored_literal_from<'a, W: crate::catalog::work::Work + Send>(
    expression: &mut Expr,
    target: &crate::ColumnType,
    catalog: &impl Fn() -> Option<&'a dyn crate::expr::enums::EnumLabelCatalog>,
    control: &uqa_core::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<uqa_core::memory::Produced<bool>, SQLError> {
    let unchanged = || Ok(control.finish(false, control.empty_reservation())?);
    let Expr::Literal(value @ (Value::Str(_) | Value::Null)) = expression else {
        return unchanged();
    };
    let mut base = target;
    while let crate::ColumnType::Domain { base: inner, .. } = base {
        control.check()?;
        work.checkpoint().await?;
        base = inner;
    }
    if catalog().is_none() {
        return unchanged();
    }
    let Some(value) =
        crate::expr::enums::fold_unknown_literal_with_work(catalog, value, base, control, work)
            .await?
    else {
        return unchanged();
    };
    let (name, name_memory) = base
        .catalog_name_with_input_work(
            control,
            &mut crate::catalog::work::ReferenceWork { control, work },
        )
        .await?
        .into_parts();
    let (value, value_memory) = value.into_parts();
    *expression = Expr::TypedLiteral {
        collation_oid: None,
        composite_source: None,
        value,
        ty: name,
    };
    Ok(control.finish(true, control.combine(name_memory, value_memory))?)
}
