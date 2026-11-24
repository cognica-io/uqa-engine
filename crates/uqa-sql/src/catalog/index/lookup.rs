//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Equality domains and ordinary single-column keys usable by parameterized relation scans.

use crate::ast::{BinaryOp, ColumnDef, ColumnType, IndexKey, TableKeyConstraint};
use crate::{SQLError, ScalarExpr, SubqueryId};
use uqa_core::catalog_index::CatalogIndexRow;

/// Above this many distinct membership keys the scan uses its ordinary spillable membership filter.
pub const MAX_MEMBERSHIP_PROBES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqualityDomain {
    Integer,
    Text,
    Boolean,
    Bytes,
}

#[derive(Debug, Clone, Copy)]
pub struct LookupColumn {
    pub position: usize,
    pub domain: EqualityDomain,
    pub unique: bool,
}

impl EqualityDomain {
    pub fn of(ty: &ColumnType, collation: Option<u32>) -> Option<Self> {
        match ty {
            ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger => {
                Some(Self::Integer)
            }
            ColumnType::Text | ColumnType::Varchar(_)
                if collation.is_none_or(|oid| {
                    i64::from(oid) == crate::catalog::collations::DEFAULT_COLLATION_OID
                }) =>
            {
                Some(Self::Text)
            }
            ColumnType::Boolean => Some(Self::Boolean),
            ColumnType::Bytea => Some(Self::Bytes),
            _ => None,
        }
    }
}

pub fn is_unique_column<'a>(
    table: &str,
    column: &ColumnDef,
    columns: &[ColumnDef],
    constraints: &[TableKeyConstraint],
    indexes: impl Iterator<Item = &'a CatalogIndexRow>,
) -> Result<bool, SQLError> {
    let field = column.name.as_str();
    if column.unique
        || (column.primary_key && columns.iter().filter(|column| column.primary_key).count() == 1)
        || constraints
            .iter()
            .any(|key| !key.without_overlaps && matches!(&key.columns[..], [key] if key == field))
    {
        return Ok(true);
    }
    for index in indexes
        .filter(|index| index.table_name == table && index.index_type.eq_ignore_ascii_case("btree"))
    {
        let decode =
            |error| SQLError::Internal(format!("decode stored index for equality lookup: {error}"));
        let definition =
            super::stored::index_definition(index.definition_json.as_deref()).map_err(decode)?;
        if definition.unique && definition.predicate.is_none() {
            let keys: Vec<IndexKey> = serde_json::from_str(&index.columns_json).map_err(decode)?;
            if matches!(&keys[..], [key] if key.column() == Some(field)) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubqueryKeys {
    Scalar(SubqueryId),
    Membership(SubqueryId),
}

impl SubqueryKeys {
    /// Select a usable key from the stable conjuncts placed on one relation.
    pub fn find_usable<'a, T>(
        terms: &'a [ScalarExpr],
        control: &uqa_core::memory::ProductionControl<'_>,
        mut bind: impl FnMut(&'a str, Self) -> Result<Option<T>, SQLError>,
    ) -> Result<Option<T>, SQLError> {
        for term in terms {
            control.check()?;
            if let Some((field, keys)) = Self::predicate(term) {
                if let Some(bound) = bind(field, keys)? {
                    return Ok(Some(bound));
                }
            }
        }
        Ok(None)
    }

    pub fn slot(self) -> SubqueryId {
        match self {
            Self::Scalar(id) | Self::Membership(id) => id,
        }
    }

    /// A positive conjunct is necessary for a row to pass the filter. Disjunctions and negations retain their original evaluation.
    pub fn first_in_filter<'a>(
        filter: &'a ScalarExpr,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<(&'a str, Self)>, SQLError> {
        let mut first = None;
        crate::semantics::visit_filter_parts_with_control(filter, control, &mut |part| {
            if first.is_none() {
                first = Self::predicate(part);
            }
            Ok(())
        })?;
        Ok(first)
    }

    pub fn predicate(expression: &ScalarExpr) -> Option<(&str, Self)> {
        let (column, keys) = match expression {
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs,
                rhs,
            } => match (lookup_operand(lhs), lookup_operand(rhs)) {
                (column, ScalarExpr::ScalarSubquery(id))
                | (ScalarExpr::ScalarSubquery(id), column) => (column, Self::Scalar(*id)),
                _ => return None,
            },
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated: false,
            } => (lookup_operand(expr), Self::Membership(*subquery)),
            _ => return None,
        };
        match column {
            ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } => {
                Some((column.as_str(), keys))
            }
            _ => None,
        }
    }
}

/// Analysis retains the original implicit collation on scalar subquery results. An explicit or different selection can change equality and cannot drive a physical key.
fn lookup_operand(expr: &ScalarExpr) -> &ScalarExpr {
    match expr {
        ScalarExpr::Collated {
            expr,
            ty,
            collation,
        } if collation
            .implicit_oid()
            .is_some_and(|oid| EqualityDomain::of(ty, Some(oid)) == Some(EqualityDomain::Text)) =>
        {
            expr
        }
        other => other,
    }
}
