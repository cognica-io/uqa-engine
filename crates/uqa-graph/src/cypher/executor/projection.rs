//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RETURN/WITH projection, aggregation, sorting, SKIP, and LIMIT.

use super::{
    aggregate_avg, aggregate_extreme, aggregate_sum, is_aggregate, nonnegative_i64_to_usize,
    return_label, sort_keyed, trunc_f64_to_i64, usize_to_i64, BTreeMap, BTreeSet, Binding,
    BindingRow, CypherError, CypherExecutor, CypherExpr, GraphStore, OrderByItem, ResultRow,
    ReturnItem, Value,
};

impl<G: GraphStore> CypherExecutor<'_, G> {
    pub(crate) fn exec_return_like(
        &self,
        items: &[ReturnItem],
        distinct: bool,
        order_by: Option<&[OrderByItem]>,
        skip: Option<&CypherExpr>,
        limit: Option<&CypherExpr>,
        bindings: &[BindingRow],
    ) -> Result<(Vec<String>, Vec<ResultRow>), CypherError> {
        let mut columns: Vec<String> = items
            .iter()
            .enumerate()
            .map(|(i, item)| return_label(item, i))
            .collect();
        // Result rows are keyed by column name; repeated unaliased
        // labels (e.g. `RETURN size(a), size(b)`) must stay distinct
        // so later columns do not clobber earlier ones.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (i, column) in columns.iter_mut().enumerate() {
            if !seen.insert(column.clone()) {
                let mut candidate = format!("{column}_{i}");
                while !seen.insert(candidate.clone()) {
                    candidate.push('_');
                }
                *column = candidate;
            }
        }

        let has_aggregate = items.iter().any(|i| is_aggregate(&i.expr));
        let mut rows: Vec<ResultRow> = if has_aggregate {
            let mut out = self.aggregate_return(items, &columns, bindings)?;
            if let Some(order) = order_by {
                self.sort_result_rows(&mut out, order)?;
            }
            out
        } else {
            // For non-aggregate flows, ORDER BY / SKIP / LIMIT operate
            // on the *binding rows* so the ordering expression can
            // still reference variables that won't survive projection
            // (e.g. `RETURN n.name ORDER BY n.age`).
            let mut binding_rows: Vec<BindingRow> = bindings.to_vec();
            if let Some(order) = order_by {
                self.sort_binding_rows(&mut binding_rows, order, items, &columns)?;
            }
            if let Some(skip_expr) = skip {
                let n =
                    nonnegative_i64_to_usize(self.eval_int(skip_expr, &BTreeMap::new())?, "SKIP")?;
                if n >= binding_rows.len() {
                    binding_rows.clear();
                } else {
                    binding_rows.drain(0..n);
                }
            }
            if let Some(limit_expr) = limit {
                let n = nonnegative_i64_to_usize(
                    self.eval_int(limit_expr, &BTreeMap::new())?,
                    "LIMIT",
                )?;
                binding_rows.truncate(n);
            }
            let mut out = Vec::with_capacity(binding_rows.len());
            for row in &binding_rows {
                let mut result = ResultRow::new();
                for (i, item) in items.iter().enumerate() {
                    let value = self.eval(&item.expr, row)?;
                    result.insert(columns[i].clone(), value);
                }
                out.push(result);
            }
            out
        };

        if distinct {
            let mut seen: BTreeSet<Vec<Value>> = BTreeSet::new();
            rows.retain(|row| {
                let key: Vec<Value> = columns
                    .iter()
                    .map(|c| row.get(c).cloned().unwrap_or(Value::Null))
                    .collect();
                seen.insert(key)
            });
        }

        if has_aggregate {
            if let Some(skip_expr) = skip {
                let n =
                    nonnegative_i64_to_usize(self.eval_int(skip_expr, &BTreeMap::new())?, "SKIP")?;
                if n >= rows.len() {
                    rows.clear();
                } else {
                    rows.drain(0..n);
                }
            }
            if let Some(limit_expr) = limit {
                let n = nonnegative_i64_to_usize(
                    self.eval_int(limit_expr, &BTreeMap::new())?,
                    "LIMIT",
                )?;
                rows.truncate(n);
            }
        }

        Ok((columns, rows))
    }

    pub(super) fn aggregate_return(
        &self,
        items: &[ReturnItem],
        columns: &[String],
        bindings: &[BindingRow],
    ) -> Result<Vec<ResultRow>, CypherError> {
        // Group by the non-aggregate items.
        let group_by_idx: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| !is_aggregate(&it.expr))
            .map(|(i, _)| i)
            .collect();

        let mut groups: BTreeMap<Vec<Value>, Vec<BindingRow>> = BTreeMap::new();
        if group_by_idx.is_empty() && bindings.is_empty() {
            // Aggregates over zero rows still produce one output row
            // (count(*) = 0, sum(...) = null, collect(...) = []).
            groups.insert(Vec::new(), Vec::new());
        }
        for row in bindings {
            let key: Vec<Value> = group_by_idx
                .iter()
                .map(|&i| self.eval(&items[i].expr, row))
                .collect::<Result<_, _>>()?;
            groups.entry(key).or_default().push(row.clone());
        }

        let mut out = Vec::with_capacity(groups.len());
        for (group_key, members) in groups {
            let mut result = ResultRow::new();
            // Non-aggregates: use group key.
            for (out_pos, &i) in group_by_idx.iter().enumerate() {
                result.insert(columns[i].clone(), group_key[out_pos].clone());
            }
            for (i, item) in items.iter().enumerate() {
                if !is_aggregate(&item.expr) {
                    continue;
                }
                let value = self.eval_aggregate(&item.expr, &members)?;
                result.insert(columns[i].clone(), value);
            }
            out.push(result);
        }
        Ok(out)
    }

    pub(super) fn eval_aggregate(
        &self,
        expr: &CypherExpr,
        members: &[BindingRow],
    ) -> Result<Value, CypherError> {
        let CypherExpr::FunctionCall(fc) = expr else {
            return Err(CypherError::Unsupported(format!(
                "non-function aggregate: {expr:?}"
            )));
        };
        let name = fc.name.to_lowercase();
        if name == "count" {
            let is_star = fc.args.is_empty()
                || fc
                    .args
                    .iter()
                    .any(|a| matches!(a, CypherExpr::Variable(v) if v.name == "*"));
            if is_star {
                return Ok(Value::Int(usize_to_i64(
                    members.len(),
                    "aggregate row count",
                )?));
            }
            let mut count = 0i64;
            let mut seen: BTreeSet<Value> = BTreeSet::new();
            for row in members {
                let v = self.eval(&fc.args[0], row)?;
                if v == Value::Null {
                    continue;
                }
                if fc.distinct {
                    if seen.insert(v) {
                        count = count.checked_add(1).ok_or_else(|| {
                            CypherError::TypeError("count() result exceeds bigint range".into())
                        })?;
                    }
                } else {
                    count = count.checked_add(1).ok_or_else(|| {
                        CypherError::TypeError("count() result exceeds bigint range".into())
                    })?;
                }
            }
            return Ok(Value::Int(count));
        }

        // Evaluate the argument across members, skipping nulls, with
        // optional DISTINCT dedup.
        let mut values: Vec<Value> = Vec::new();
        let mut seen: BTreeSet<Value> = BTreeSet::new();
        for row in members {
            let v = self.eval(&fc.args[0], row)?;
            if v == Value::Null {
                continue;
            }
            if fc.distinct && !seen.insert(v.clone()) {
                continue;
            }
            values.push(v);
        }

        match name.as_str() {
            "collect" => Ok(Value::List(values)),
            "min" | "max" => Ok(aggregate_extreme(&values, name == "min")),
            "sum" => aggregate_sum(&values),
            "avg" => aggregate_avg(&values),
            other => Err(CypherError::Unsupported(format!("aggregate {other}"))),
        }
    }

    pub(super) fn sort_result_rows(
        &self,
        rows: &mut [ResultRow],
        order: &[OrderByItem],
    ) -> Result<(), CypherError> {
        let mut keyed: Vec<(Vec<Value>, ResultRow)> = rows
            .iter()
            .cloned()
            .map(|row| -> Result<_, CypherError> {
                let bindings: BindingRow = row
                    .iter()
                    .map(|(k, v)| (k.clone(), Binding::Value(v.clone())))
                    .collect();
                let key: Vec<Value> = order
                    .iter()
                    .map(|o| self.eval(&o.expr, &bindings))
                    .collect::<Result<_, _>>()?;
                Ok((key, row))
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_keyed(&mut keyed, order);
        for (i, (_, row)) in keyed.into_iter().enumerate() {
            rows[i] = row;
        }
        Ok(())
    }

    pub(super) fn sort_binding_rows(
        &self,
        rows: &mut [BindingRow],
        order: &[OrderByItem],
        items: &[ReturnItem],
        columns: &[String],
    ) -> Result<(), CypherError> {
        let mut keyed: Vec<(Vec<Value>, BindingRow)> = rows
            .iter()
            .cloned()
            .map(|row| -> Result<_, CypherError> {
                // Overlay projected aliases on top of the source bindings
                // so ORDER BY can reference either.
                let mut overlay = row.clone();
                for (i, item) in items.iter().enumerate() {
                    let v = self.eval(&item.expr, &row)?;
                    overlay.insert(columns[i].clone(), Binding::Value(v));
                }
                let key: Vec<Value> = order
                    .iter()
                    .map(|o| self.eval(&o.expr, &overlay))
                    .collect::<Result<_, _>>()?;
                Ok((key, row))
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_keyed(&mut keyed, order);
        for (i, (_, row)) in keyed.into_iter().enumerate() {
            rows[i] = row;
        }
        Ok(())
    }

    pub(super) fn eval_int(&self, expr: &CypherExpr, row: &BindingRow) -> Result<i64, CypherError> {
        match self.eval(expr, row)? {
            Value::Int(n) => Ok(n),
            Value::Float(f) => trunc_f64_to_i64(f, "integer expression"),
            other => Err(CypherError::TypeError(format!(
                "expected integer, got {other:?}"
            ))),
        }
    }

    // ------------------------------------------------------------------
    // Expression evaluation
    // ------------------------------------------------------------------
}
