//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cypher expression, operator, collection, and CASE evaluation.

use super::{
    agtype, agtype_add, agtype_div, agtype_mod, agtype_pow, nonnegative_i64_to_usize, null_or_bool,
    numeric_op, regex_match, str_predicate, strict_bool, usize_to_i64, value_property, BTreeMap,
    BinaryOp, Binding, BindingRow, CaseExpr, CypherError, CypherExecutor, CypherExpr, GraphStore,
    InList, IsNotNull, IsNull, ListComprehension, ListIndex, ListLiteral, ListSlice, Literal,
    MapLiteral, Parameter, PropertyAccess, UnaryOp, Value, Variable,
};

impl<G: GraphStore> CypherExecutor<'_, G> {
    pub(crate) fn eval(&self, expr: &CypherExpr, row: &BindingRow) -> Result<Value, CypherError> {
        match expr {
            CypherExpr::Literal(Literal { value }) => Ok(value.clone()),
            CypherExpr::Parameter(Parameter { name }) => self
                .params
                .get(name)
                .cloned()
                .ok_or_else(|| CypherError::UndefinedParameter(name.clone())),
            CypherExpr::Variable(Variable { name }) => match row.get(name) {
                Some(b) => b.to_value(),
                None => Err(CypherError::UndefinedVariable(name.clone())),
            },
            CypherExpr::PropertyAccess(PropertyAccess { variable, keys }) => {
                let Some(binding) = row.get(variable) else {
                    return Err(CypherError::UndefinedVariable(variable.clone()));
                };
                let mut value = match binding {
                    Binding::Vertex(_) | Binding::Edge(_) => binding.property(&keys[0]),
                    Binding::EdgeList(_) => {
                        return Err(CypherError::TypeError(
                            "scalar object must be a vertex or edge".into(),
                        ));
                    }
                    Binding::Value(v) => value_property(v, &keys[0]).ok_or_else(|| {
                        CypherError::TypeError("scalar object must be a vertex or edge".into())
                    })?,
                };
                for key in &keys[1..] {
                    value = value_property(&value, key).ok_or_else(|| {
                        CypherError::TypeError("scalar object must be a vertex or edge".into())
                    })?;
                }
                Ok(value)
            }
            CypherExpr::BinaryOp(b) => self.eval_binary(b, row),
            CypherExpr::UnaryOp(u) => self.eval_unary(u, row),
            CypherExpr::ListIndex(li) => self.eval_list_index(li, row),
            CypherExpr::ListSlice(ls) => self.eval_list_slice(ls, row),
            CypherExpr::ListLiteral(ll) => self.eval_list_literal(ll, row),
            CypherExpr::ListComprehension(lc) => self.eval_list_comprehension(lc, row),
            CypherExpr::InList(il) => self.eval_in_list(il, row),
            CypherExpr::IsNull(IsNull { expr }) => {
                Ok(Value::Bool(matches!(self.eval(expr, row)?, Value::Null)))
            }
            CypherExpr::IsNotNull(IsNotNull { expr }) => {
                Ok(Value::Bool(!matches!(self.eval(expr, row)?, Value::Null)))
            }
            CypherExpr::CaseExpr(c) => self.eval_case(c, row),
            CypherExpr::FunctionCall(fc) => self.eval_function(fc, row),
            CypherExpr::MapLiteral(ml) => self.eval_map_literal(ml, row),
            CypherExpr::ExistsPattern(pattern) => {
                let matches = self.match_path_pattern(pattern, row)?;
                Ok(Value::Bool(!matches.is_empty()))
            }
        }
    }

    pub(super) fn eval_binary(
        &self,
        expr: &BinaryOp,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        let lhs = self.eval(&expr.left, row)?;
        let rhs = self.eval(&expr.right, row)?;
        match expr.op.as_str() {
            "AND" => {
                let left_bool = strict_bool(&lhs)?;
                let right_bool = strict_bool(&rhs)?;
                Ok(match (left_bool, right_bool) {
                    (Some(false), _) | (_, Some(false)) => Value::Bool(false),
                    (Some(true), Some(true)) => Value::Bool(true),
                    _ => Value::Null,
                })
            }
            "OR" => {
                let left_bool = strict_bool(&lhs)?;
                let right_bool = strict_bool(&rhs)?;
                Ok(match (left_bool, right_bool) {
                    (Some(true), _) | (_, Some(true)) => Value::Bool(true),
                    (Some(false), Some(false)) => Value::Bool(false),
                    _ => Value::Null,
                })
            }
            "XOR" => {
                let left_bool = strict_bool(&lhs)?;
                let right_bool = strict_bool(&rhs)?;
                Ok(match (left_bool, right_bool) {
                    (Some(x), Some(y)) => Value::Bool(x ^ y),
                    _ => Value::Null,
                })
            }
            "=" => Ok(null_or_bool(&lhs, &rhs, agtype::eq(&lhs, &rhs))),
            "<>" => Ok(null_or_bool(&lhs, &rhs, !agtype::eq(&lhs, &rhs))),
            "<" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) == std::cmp::Ordering::Less,
            )),
            ">" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) == std::cmp::Ordering::Greater,
            )),
            "<=" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) != std::cmp::Ordering::Greater,
            )),
            ">=" => Ok(null_or_bool(
                &lhs,
                &rhs,
                agtype::cmp(&lhs, &rhs) != std::cmp::Ordering::Less,
            )),
            "+" => agtype_add(&lhs, &rhs),
            "-" => numeric_op(&lhs, &rhs, "agtype_sub", i64::wrapping_sub, |a, b| a - b),
            "*" => numeric_op(&lhs, &rhs, "agtype_mul", i64::wrapping_mul, |a, b| a * b),
            "/" => agtype_div(&lhs, &rhs),
            "%" => agtype_mod(&lhs, &rhs),
            "^" => agtype_pow(&lhs, &rhs),
            "STARTS WITH" => Ok(str_predicate(&lhs, &rhs, |a, b| a.starts_with(b))),
            "ENDS WITH" => Ok(str_predicate(&lhs, &rhs, |a, b| a.ends_with(b))),
            "CONTAINS" => Ok(str_predicate(&lhs, &rhs, |a, b| a.contains(b))),
            "=~" => regex_match(&lhs, &rhs),
            other => Err(CypherError::Unsupported(format!("binary op {other}"))),
        }
    }

    pub(super) fn eval_unary(&self, u: &UnaryOp, row: &BindingRow) -> Result<Value, CypherError> {
        let operand = self.eval(&u.operand, row)?;
        match u.op.as_str() {
            "NOT" => Ok(match strict_bool(&operand)? {
                Some(b) => Value::Bool(!b),
                None => Value::Null,
            }),
            "-" => match operand {
                Value::Int(n) => Ok(Value::Int(n.wrapping_neg())),
                Value::Float(f) => Ok(Value::Float(-f)),
                Value::Null => Ok(Value::Null),
                _ => Err(CypherError::TypeError(
                    "Invalid input parameter type for agtype_neg".into(),
                )),
            },
            other => Err(CypherError::Unsupported(format!("unary op {other}"))),
        }
    }

    pub(super) fn eval_list_literal(
        &self,
        ll: &ListLiteral,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        let mut out = Vec::with_capacity(ll.elements.len());
        for element in &ll.elements {
            out.push(self.eval(element, row)?);
        }
        Ok(Value::List(out))
    }

    pub(super) fn eval_map_literal(
        &self,
        ml: &MapLiteral,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        let mut out = BTreeMap::new();
        for (key, expr) in &ml.pairs {
            out.insert(key.clone(), self.eval(expr, row)?);
        }
        Ok(Value::Map(out))
    }

    pub(super) fn eval_list_comprehension(
        &self,
        lc: &ListComprehension,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        let source = self.eval(&lc.list_expr, row)?;
        let items = match source {
            Value::List(items) => items,
            Value::Null => return Ok(Value::Null),
            other => {
                return Err(CypherError::TypeError(format!(
                    "list comprehension requires a list, got agtype {}",
                    agtype::agtype_type_name(&other)
                )));
            }
        };
        let mut out = Vec::new();
        for item in items {
            let mut scoped = row.clone();
            scoped.insert(lc.variable.clone(), Binding::Value(item.clone()));
            if let Some(filter) = &lc.filter {
                if !self.eval_predicate(filter, &scoped)? {
                    continue;
                }
            }
            match &lc.map_expr {
                Some(map_expr) => out.push(self.eval(map_expr, &scoped)?),
                None => out.push(item),
            }
        }
        Ok(Value::List(out))
    }

    pub(super) fn eval_in_list(&self, il: &InList, row: &BindingRow) -> Result<Value, CypherError> {
        let needle = self.eval(&il.expr, row)?;
        let haystack = self.eval(&il.list_expr, row)?;
        let items = match haystack {
            Value::List(items) => items,
            Value::Null => return Ok(Value::Null),
            _ => {
                return Err(CypherError::TypeError("object of IN must be a list".into()));
            }
        };
        if items.is_empty() {
            return Ok(Value::Bool(false));
        }
        if needle == Value::Null {
            return Ok(Value::Null);
        }
        let mut saw_null = false;
        for item in &items {
            if *item == Value::Null {
                saw_null = true;
            } else if agtype::eq(item, &needle) {
                return Ok(Value::Bool(true));
            }
        }
        if saw_null {
            Ok(Value::Null)
        } else {
            Ok(Value::Bool(false))
        }
    }

    pub(super) fn eval_list_index(
        &self,
        li: &ListIndex,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        let target = self.eval(&li.expr, row)?;
        let index = self.eval(&li.index, row)?;
        if target == Value::Null || index == Value::Null {
            return Ok(Value::Null);
        }
        if let Some(props) = agtype::entity_properties(&target) {
            return match index {
                Value::Str(key) => Ok(props.get(&key).cloned().unwrap_or(Value::Null)),
                _ => Err(CypherError::TypeError(
                    "object index must resolve to a string value".into(),
                )),
            };
        }
        match (&target, &index) {
            (Value::List(items), Value::Int(n)) => {
                let idx = if *n < 0 {
                    let len = usize_to_i64(items.len(), "list length")?;
                    let adjusted = len.checked_add(*n).ok_or_else(|| {
                        CypherError::TypeError("list index arithmetic overflow".into())
                    })?;
                    if adjusted < 0 {
                        return Ok(Value::Null);
                    }
                    nonnegative_i64_to_usize(adjusted, "list index")?
                } else {
                    nonnegative_i64_to_usize(*n, "list index")?
                };
                Ok(items.get(idx).cloned().unwrap_or(Value::Null))
            }
            (Value::List(_), _) => Err(CypherError::TypeError(
                "array index must resolve to an integer value".into(),
            )),
            (Value::Map(m), Value::Str(k)) => Ok(m.get(k).cloned().unwrap_or(Value::Null)),
            (Value::Map(_), _) => Err(CypherError::TypeError(
                "object index must resolve to a string value".into(),
            )),
            _ => Err(CypherError::TypeError(
                "scalar object must be a vertex or edge".into(),
            )),
        }
    }

    pub(super) fn eval_list_slice(
        &self,
        ls: &ListSlice,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        let target = self.eval(&ls.expr, row)?;
        let items = match target {
            Value::List(items) => items,
            Value::Null => return Ok(Value::Null),
            _ => {
                return Err(CypherError::TypeError("slice must access a list".into()));
            }
        };
        let len = usize_to_i64(items.len(), "list length")?;
        let resolve = |expr: &Option<Box<CypherExpr>>, default: i64| -> Result<i64, CypherError> {
            match expr {
                Some(e) => match self.eval(e, row)? {
                    Value::Int(n) => Ok(if n < 0 { (len + n).max(0) } else { n.min(len) }),
                    Value::Null => Ok(default),
                    _ => Err(CypherError::TypeError(
                        "slice bound must resolve to an integer value".into(),
                    )),
                },
                None => Ok(default),
            }
        };
        let start = resolve(&ls.start, 0)?;
        let end = resolve(&ls.end, len)?;
        if start >= end {
            return Ok(Value::List(Vec::new()));
        }
        let start = nonnegative_i64_to_usize(start, "slice start")?;
        let end = nonnegative_i64_to_usize(end, "slice end")?;
        Ok(Value::List(items[start..end].to_vec()))
    }

    pub(super) fn eval_case(&self, c: &CaseExpr, row: &BindingRow) -> Result<Value, CypherError> {
        let operand_value = match &c.operand {
            Some(expr) => Some(self.eval(expr, row)?),
            None => None,
        };
        for (cond, result) in &c.whens {
            let matched = if let Some(operand) = &operand_value {
                let candidate = self.eval(cond, row)?;
                *operand != Value::Null
                    && candidate != Value::Null
                    && agtype::eq(&candidate, operand)
            } else {
                self.eval_predicate(cond, row)?
            };
            if matched {
                return self.eval(result, row);
            }
        }
        if let Some(else_expr) = &c.else_expr {
            return self.eval(else_expr, row);
        }
        Ok(Value::Null)
    }
}
