//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assign input and result collations after expression typing and coercion.

use super::{ColumnType, Expr, ExpressionContext, Field, Node, SQLError, TypedNode};
use crate::catalog::collations::derivation::Derivation;
use crate::catalog::type_metadata::{pg_type_modifier, pg_type_oid};

impl ExpressionContext<'_> {
    pub(super) fn collated_expression(
        &self,
        clause: crate::catalog::collations::expression::Clause<'_, Expr>,
        expected: Option<&ColumnType>,
    ) -> Result<TypedNode, SQLError> {
        let source = self.expression_type(clause.argument)?;
        let oid = crate::catalog::collations::expression::select(
            clause.name,
            clause.oid,
            source.as_ref(),
            self.types,
        )?;
        let mut argument = clause.argument;
        if expected.is_some_and(|target| {
            source.as_ref().is_none_or(|source| {
                pg_type_oid(source) != pg_type_oid(target) || pg_type_modifier(target) >= 0
            })
        }) {
            while let Some(nested) = crate::catalog::collations::expression::ast_clause(argument)? {
                argument = nested.argument;
            }
        }
        let mut value = self.encode(argument, expected)?;
        if expected.is_none_or(crate::schema::composites::type_is_collatable) {
            value.node = collate(value.node, i64::from(oid));
        }
        Ok(value)
    }
}

/// Fresh encoder nodes hold their declared type's collation in result fields. This pass consumes those defaults once, after all type/coercion errors have been resolved; serialized nodes already contain selected collations and must not be reassigned.
pub(super) fn assign(node: &mut Node) -> Result<(), SQLError> {
    visit(node, None).map(|_| ())
}

/// FIELDSELECT observes the selected ROW child's parse-time collation before the enclosing collation pass.
pub(super) fn initial(node: &Node) -> Result<u32, SQLError> {
    let field = match node.kind.as_str() {
        "COLLATEEXPR" => "collOid",
        "VAR" => "varcollid",
        "CONST" => "constcollid",
        "PARAM" => "paramcollid",
        "COERCETODOMAINVALUE" | "CASETESTEXPR" => "collation",
        "FIELDSELECT" => "resultcollid",
        // These children acquire a collation in the later pass; its type defaults are not parse-time identities.
        "RELABELTYPE" | "COERCEVIAIO" | "COERCETODOMAIN" | "ARRAYCOERCEEXPR" | "OPEXPR"
        | "NULLIFEXPR" | "DISTINCTEXPR" | "FUNCEXPR" | "SUBSCRIPTINGREF" | "ARRAYEXPR"
        | "CASEEXPR" | "COALESCEEXPR" | "MINMAXEXPR" | "ROWEXPR" | "ROWCOMPAREEXPR"
        | "BOOLEXPR" | "BOOLEANTEST" | "NULLTEST" | "SCALARARRAYOPEXPR" => return Ok(0),
        _ => return Err(super::super::invalid("unknown fresh expression collation")),
    };
    u32::try_from(node.integer(field)?)
        .map_err(|_| super::super::invalid("invalid expression collation"))
}

pub(super) fn collate(argument: Node, oid: i64) -> Node {
    Node::new(
        "COLLATEEXPR",
        [
            ("arg", argument.into()),
            ("collOid", oid.into()),
            ("location", (-1).into()),
        ],
    )
}

/// A type coercion keeps only the outer explicit collation, outside the converted value.
pub(super) fn peel(mut node: Node) -> Result<(Node, Option<i64>), SQLError> {
    let mut selected = None;
    while node.kind == "COLLATEEXPR" {
        selected.get_or_insert(node.integer("collOid")?);
        node = match node.fields.into_iter().find(|(name, _)| name == "arg") {
            Some((_, Field::Node(argument))) => argument,
            _ => return Err(super::super::invalid("invalid collation argument")),
        };
    }
    Ok((node, selected))
}

fn visit(node: &mut Node, case_value: Option<i64>) -> Result<Derivation, SQLError> {
    if node.kind == "NAMEDARGEXPR" {
        return visit_field(field_mut(node, "arg")?, case_value);
    }
    let leaf = match node.kind.as_str() {
        "VAR" => Some("varcollid"),
        "CONST" => Some("constcollid"),
        "PARAM" => Some("paramcollid"),
        "COERCETODOMAINVALUE" => Some("collation"),
        "CASETESTEXPR" => {
            if let Some(collation) = case_value {
                *field_mut(node, "collation")? = collation.into();
            }
            Some("collation")
        }
        _ => None,
    };
    if let Some(field) = leaf {
        return Ok(Derivation::implicit(node.integer(field)?));
    }
    match node.kind.as_str() {
        "FIELDSELECT" => {
            visit_field(field_mut(node, "arg")?, case_value)?;
            Ok(Derivation::implicit(node.integer("resultcollid")?))
        }
        "ROWEXPR" => {
            for item in list_mut(node, "args")? {
                visit_field(item, case_value)?;
            }
            Ok(Derivation::None)
        }
        "ROWCOMPAREEXPR" => row_comparison(node, case_value),
        "SUBSCRIPTINGREF" => {
            for name in ["refupperindexpr", "reflowerindexpr"] {
                visit_field(field_mut(node, name)?, case_value)?;
            }
            let input = visit_field(field_mut(node, "refexpr")?, case_value)?;
            let result = input.result(node.integer("refcollid")?);
            *field_mut(node, "refcollid")? = result.oid().into();
            Ok(result)
        }
        "COLLATEEXPR" => {
            visit_field(field_mut(node, "arg")?, case_value)?;
            Ok(Derivation::Explicit(node.integer("collOid")?))
        }
        "CASEEXPR" => case(node, case_value),
        _ => ordinary(node, case_value),
    }
}

fn ordinary(node: &mut Node, case_value: Option<i64>) -> Result<Derivation, SQLError> {
    let result_field = match node.kind.as_str() {
        "OPEXPR" | "NULLIFEXPR" | "DISTINCTEXPR" => Some("opcollid"),
        "FUNCEXPR" => Some("funccollid"),
        "RELABELTYPE" | "COERCEVIAIO" | "COERCETODOMAIN" | "ARRAYCOERCEEXPR" => {
            Some("resultcollid")
        }
        "ARRAYEXPR" => Some("array_collid"),
        "COALESCEEXPR" => Some("coalescecollid"),
        "MINMAXEXPR" => Some("minmaxcollid"),
        "BOOLEXPR" | "BOOLEANTEST" | "NULLTEST" | "SCALARARRAYOPEXPR" => None,
        _ => return Err(super::super::invalid("unknown fresh expression node")),
    };
    let declared = result_field.map_or(Ok(0), |name| node.integer(name))?;
    let mut input = Derivation::None;
    for (name, field) in &mut node.fields {
        if matches!(name.as_str(), "arg" | "args" | "elements" | "elemexpr") {
            // An array coercion's CaseTestExpr is its own element placeholder.
            let scope = if name == "elemexpr" { None } else { case_value };
            input = input.merge(visit_field(field, scope)?)?;
        }
    }
    let result = if node.kind == "COERCETODOMAIN" {
        input.domain(declared)
    } else {
        input.result(declared)
    };
    if let Some(name) = result_field {
        *field_mut(node, name)? = result.oid().into();
    }
    if let Some((_, value)) = node
        .fields
        .iter_mut()
        .find(|(name, _)| name == "inputcollid")
    {
        *value = input.oid().into();
    }
    Ok(result)
}

fn case(node: &mut Node, outer_case_value: Option<i64>) -> Result<Derivation, SQLError> {
    let declared = node.integer("casecollid")?;
    let base = visit_field(field_mut(node, "arg")?, outer_case_value)?;
    let has_base = !matches!(node.field("arg")?, Field::Null);
    let case_value = if has_base {
        Some(base.oid())
    } else {
        outer_case_value
    };
    let mut result = Derivation::None;
    for when in list_mut(node, "args")? {
        let Field::Node(when) = when else {
            return Err(super::super::invalid("invalid CASE branch"));
        };
        visit_field(field_mut(when, "expr")?, case_value)?;
        result = result.merge(visit_field(field_mut(when, "result")?, case_value)?)?;
    }
    result = result
        .merge(visit_field(field_mut(node, "defresult")?, case_value)?)?
        .result(declared);
    *field_mut(node, "casecollid")? = result.oid().into();
    Ok(result)
}

fn row_comparison(node: &mut Node, case_value: Option<i64>) -> Result<Derivation, SQLError> {
    let (mut left, mut right) = (None, None);
    for (name, field) in &mut node.fields {
        match name.as_str() {
            "largs" => left = Some(field),
            "rargs" => right = Some(field),
            _ => {}
        }
    }
    let (Some(Field::List(left)), Some(Field::List(right))) = (left, right) else {
        return Err(super::super::invalid("invalid row comparison arguments"));
    };
    if left.len() != right.len() {
        return Err(super::super::invalid("invalid row comparison width"));
    }
    let collations = std::iter::once(Ok(Field::Atom("o".into())))
        .chain(left.iter_mut().zip(right).map(|(left, right)| {
            visit_field(left, case_value)?
                .merge(visit_field(right, case_value)?)
                .map(|state| state.oid().into())
        }))
        .collect::<Result<Vec<_>, _>>()?;
    *field_mut(node, "inputcollids")? = Field::List(collations);
    Ok(Derivation::None)
}

fn visit_field(field: &mut Field, case_value: Option<i64>) -> Result<Derivation, SQLError> {
    match field {
        Field::Node(node) => visit(node, case_value),
        Field::Null => Ok(Derivation::None),
        Field::List(items) => items.iter_mut().try_fold(Derivation::None, |state, item| {
            state.merge(visit_field(item, case_value)?)
        }),
        _ => Err(super::super::invalid("expected expression node")),
    }
}

fn field_mut<'a>(node: &'a mut Node, name: &str) -> Result<&'a mut Field, SQLError> {
    node.fields
        .iter_mut()
        .find_map(|(field, value)| (field == name).then_some(value))
        .ok_or_else(|| super::super::invalid("missing expression field"))
}

fn list_mut<'a>(node: &'a mut Node, name: &str) -> Result<&'a mut [Field], SQLError> {
    match field_mut(node, name)? {
        Field::List(values) => Ok(values),
        Field::Null => Ok(&mut []),
        _ => Err(super::super::invalid("expected expression list")),
    }
}

#[cfg(test)]
mod tests;
