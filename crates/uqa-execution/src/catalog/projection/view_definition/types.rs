//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type names, typed constants and coercions in reconstructed SQL, as `ruleutils.c` prints `Const`, `CoerceToDomain` and array constructors: a user-defined type named by identity is spelled by its current name, an enum constant by its current label, a domain coercion over the value of its base type, and a cast of an `ARRAY[...]` constructor as a conversion of each element.

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, UserTypeIdentity};
use uqa_sql::ir::ScalarExpr;
use uqa_sql::plan::QueryPlan;

use super::{Deparser, SQLError, Scope};
use crate::catalog::projection::regtypes::{
    catalog_type_display_name, catalog_user_type_identity, CatalogEnumLabels,
};

impl Deparser<'_> {
    /// The type a stored type name denotes: an identity resolves through the catalog, a built-in name by itself.
    pub(super) fn resolved_type(&self, ty: &str) -> Option<ColumnType> {
        match UserTypeIdentity::parse(ty) {
            Some(identity) => catalog_user_type_identity(self.catalog, identity),
            None => ColumnType::from_sql_name(ty).ok(),
        }
    }

    /// `format_type_be` of a stored type name.
    pub(super) fn type_display(&self, ty: &str) -> String {
        match UserTypeIdentity::parse(ty) {
            Some(identity) => catalog_user_type_identity(self.catalog, identity).map_or_else(
                || ty.to_string(),
                |resolved| catalog_type_display_name(&self.dynamic, &resolved),
            ),
            None => super::expressions::type_name(ty),
        }
    }

    /// A typed constant, as `get_const_expr` prints a `Const`: a non-negative `integer`, a `numeric` written with a decimal point and a `boolean` print bare, an enum or enum-array constant shows its current label text, and every other constant prints its output text with its type.
    pub(super) fn typed_literal(&self, value: &Value, ty: &str) -> Result<String, SQLError> {
        let display = self.type_display(ty);
        let Some(resolved) = self.resolved_type(ty) else {
            return Ok(format!(
                "({})::{display}",
                super::expressions::literal(value)?
            ));
        };
        let enum_bearing = uqa_sql::expr::enums::is_enum_bearing(&resolved);
        if !enum_bearing {
            if matches!(value, Value::Null) {
                return Ok(format!("NULL::{display}"));
            }
            let mut base = &resolved;
            while let ColumnType::Domain { base: inner, .. } = base {
                base = inner;
            }
            let bare = match (value, base) {
                (Value::Int(number), ColumnType::Integer) => {
                    (0..=i64::from(i32::MAX)).contains(number)
                }
                (Value::Decimal(number), ColumnType::Numeric { .. }) => {
                    let text = number.to_sql_string();
                    !text.starts_with('-') && text.contains('.')
                }
                (Value::Bool(_), ColumnType::Boolean) => true,
                _ => false,
            };
            if bare {
                return super::expressions::literal(value);
            }
            let text = uqa_sql::result::format_postgres_text(value, &resolved, None)?;
            return Ok(format!("'{}'::{display}", text.replace('\'', "''")));
        }
        if matches!(value, Value::Null) {
            return Ok(format!("NULL::{display}"));
        }
        let labels = CatalogEnumLabels {
            catalog: self.catalog,
            resolution: &self.dynamic,
        };
        let text = uqa_sql::expr::enums::render_enum_labels(Some(&labels), value)?;
        let resolved = self
            .resolved_type(ty)
            .ok_or_else(|| SQLError::Internal(format!("stored constant type {ty} disappeared")))?;
        let text = uqa_sql::result::format_postgres_text(&text, &resolved, None)?;
        Ok(format!("'{}'::{display}", text.replace('\'', "''")))
    }

    /// Whether two stored type names denote the same type.
    fn same_type(&self, left: &str, right: &str) -> bool {
        match (self.resolved_type(left), self.resolved_type(right)) {
            (Some(left), Some(right)) => left.catalog_name() == right.catalog_name(),
            _ => left == right,
        }
    }

    /// Coercions `ruleutils.c` prints differently from a plain cast, or `None` for an ordinary cast.
    pub(super) fn coercion(
        &self,
        expr: &ScalarExpr,
        ty: &str,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<Option<String>, SQLError> {
        // A constant already of the cast's type prints once.
        if let ScalarExpr::TypedLiteral {
            ty: constant_type, ..
        } = expr
        {
            if self.same_type(constant_type, ty) {
                return self.expression(expr, scope, subqueries).map(Some);
            }
        }
        let Some(target) = self.resolved_type(ty) else {
            return Ok(None);
        };
        let mut base = &target;
        while let ColumnType::Domain { base: inner, .. } = base {
            base = inner;
        }
        if let (ScalarExpr::Array(items), ColumnType::Array(element)) = (expr, base) {
            let display = self.type_display(ty);
            if items.is_empty() {
                return Ok(Some(format!("ARRAY[]::{display}")));
            }
            let mut leaf = element.as_ref();
            while let ColumnType::Array(inner) = leaf {
                leaf = inner;
            }
            let elements = self.array_elements(items, &leaf.catalog_name(), scope, subqueries)?;
            let array = format!("ARRAY[{elements}]");
            // A domain over the array type coerces the converted array.
            return Ok(Some(if matches!(target, ColumnType::Domain { .. }) {
                format!("({array})::{display}")
            } else {
                array
            }));
        }
        if let ColumnType::Domain { base, .. } = &target {
            if matches!(expr, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                let operand = self.cast(expr, &base.catalog_name(), scope, subqueries)?;
                return Ok(Some(format!("({operand})::{}", self.type_display(ty))));
            }
        }
        Ok(None)
    }

    fn array_elements(
        &self,
        items: &[ScalarExpr],
        element: &str,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        items
            .iter()
            .map(|item| match item {
                ScalarExpr::Array(inner) => self
                    .array_elements(inner, element, scope, subqueries)
                    .map(|elements| format!("ARRAY[{elements}]")),
                item => self.cast(item, element, scope, subqueries),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|items| items.join(", "))
    }
}
