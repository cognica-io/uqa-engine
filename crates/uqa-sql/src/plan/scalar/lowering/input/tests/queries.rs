//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::SelectStmt;

struct Queries {
    literals: BudgetedVec<i64>,
    replace_query: bool,
}

impl ScalarQueryInput for Queries {
    async fn parameter(
        &mut self,
        expression: &Expr,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<usize>, SQLError> {
        input.checkpoint().await?;
        control.check()?;
        Ok(match expression {
            Expr::Column(name) if name.as_str() == "replacement" => Some(7),
            Expr::ScalarSubquery(_) if self.replace_query => Some(9),
            _ => None,
        })
    }

    async fn append(
        &mut self,
        query: &SelectStmt,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<usize, SQLError> {
        input.checkpoint().await?;
        control.check()?;
        let Expr::Literal(Value::Int(value)) = query.projections[0].expr else {
            panic!("fixture query contains one integer literal");
        };
        if value == 99 {
            return Err(SQLError::Internal("rejected query input".into()));
        }
        let index = self.literals.len();
        self.literals.push(value)?;
        Ok(index)
    }
}

type QueryResult = (OwnedScalarExpr, BudgetedVec<i64>);
type QueryInput = ImageDecoder<QueryResult, SQLError>;

fn query_input(
    source: Arc<Expr>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    replace_query: bool,
) -> QueryInput {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut queries = Queries {
            literals: BudgetedVec::new(&native),
            replace_query,
        };
        let scalar = ExpressionPlan::lower_scalar_with_query_input(
            &source,
            &mut queries,
            &control,
            &mut input,
        )
        .await?;
        Ok((scalar, queries.literals))
    })
    .unwrap()
}

fn advance(input: &mut QueryInput) -> Result<Option<QueryResult>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn retained_scalar_query_input_preserves_ordinary_query_slot_order() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (sql, expected) in [
        ("SELECT (SELECT 1) IN (SELECT 2)", vec![1, 2]),
        ("SELECT CASE WHEN EXISTS (SELECT 1) THEN (SELECT 2) ELSE (SELECT 3) END", vec![1, 2, 3]),
        ("SELECT sum((SELECT 1)) FILTER (WHERE EXISTS (SELECT 5)) OVER (PARTITION BY (SELECT 2) ORDER BY (SELECT 3) ROWS (SELECT 4) PRECEDING)", vec![1, 2, 3, 4, 5]),
        ("SELECT ARRAY[(SELECT 1), (SELECT 2)]", vec![1, 2]),
    ] {
        let source = Arc::new(expression(sql));
        let ordinary = ExpressionPlan::lower((*source).clone());
        let mut input = query_input(source, &native, &work, &token, &token, false);
        let mut polls = 0;
        let (scalar, queries) = loop {
            polls += 1;
            if let Some(result) = advance(&mut input).unwrap() {
                break result;
            }
            assert!(polls < 1024);
        };
        assert!(polls > expected.len());
        assert_eq!(*scalar, ordinary.scalar, "{sql}");
        assert_eq!(&*queries, expected);
        assert_eq!(ordinary.subqueries.len(), queries.len());
        assert_eq!(work.used(), 0);
        drop((scalar, queries));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_scalar_parameter_input_suppresses_replaced_subtree_construction() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = Arc::new(expression("SELECT ROW(replacement, (SELECT 99))"));
    let (scalar, queries) = query_input(source, &native, &work, &token, &token, true)
        .read(&mut std::io::empty())
        .unwrap();
    assert_eq!(
        *scalar,
        ScalarExpr::Row(vec![ScalarExpr::Param(7), ScalarExpr::Param(9)])
    );
    assert!(queries.is_empty());
    drop((scalar, queries));
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_scalar_query_input_retires_each_prefix_and_failed_query_owner() {
    let source = Arc::new(expression(
        "SELECT ROW((SELECT 1), (SELECT 2) IN (SELECT 3))",
    ));
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut reader = query_input(source.clone(), &native, &work, &token, &token, false);
    let mut polls = 1;
    while advance(&mut reader).unwrap().is_none() {
        polls += 1;
    }
    assert_eq!((native.used(), work.used()), (0, 0));
    let sibling = native.reserve(31).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader =
                query_input(source.clone(), &native, &work, &original, &invoking, false);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            }
            drop(reader);
            assert_eq!((native.used(), work.used()), (31, 0));
        }
    }
    let rejected = Arc::new(expression("SELECT ROW((SELECT 1), (SELECT 99))"));
    let error = query_input(rejected, &native, &work, &token, &token, false)
        .read(&mut std::io::empty())
        .unwrap_err();
    assert!(error.to_string().contains("rejected query input"));
    assert_eq!((native.used(), work.used()), (31, 0));
    drop(sibling);
}
