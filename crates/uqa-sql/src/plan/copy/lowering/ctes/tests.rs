//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::input::{ScalarInput, TextInput};
use super::{metadata, Copying, Result};
use crate::{
    ast::{Expr, SelectStmt, Statement, CTE},
    construction::{self, Work},
    plan::{
        mutable::memory::RewriteMemory, retirement::Pending, CteCyclePlan, CteSearchPlan,
        ExpressionPlan, OwnedScalarExpr, UnifiedPlan,
    },
    SQLError,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{BudgetedVec, MemoryBudget, Produced, ProductionControl},
    CancellationToken, Value,
};

struct Queries<'a> {
    work: Work<'a>,
    values: BudgetedVec<i64>,
}
impl<'s> crate::plan::scalar::ScalarQueryInput<&'s Expr, &'s SelectStmt> for Queries<'_> {
    async fn append(
        &mut self,
        query: &'s SelectStmt,
        control: &ProductionControl<'_>,
    ) -> Result<usize> {
        self.work.checkpoint().await;
        control.check()?;
        let Expr::Literal(Value::Int(value)) = query.projections[0].expr else {
            panic!("literal query fixture")
        };
        if value == 99 {
            return Err(SQLError::Internal("rejected cycle query".into()));
        }
        let index = self.values.len();
        self.values.push(value)?;
        Ok(index)
    }
}
struct Native<'a> {
    work: Work<'a>,
    queries: Queries<'a>,
}
impl<'s> ScalarInput<&'s Expr> for Native<'_> {
    async fn checkpoint(&mut self) -> Result<()> {
        self.work.checkpoint().await;
        Ok(())
    }
    async fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr> {
        ExpressionPlan::lower_scalar_with_query_input(
            expression,
            &mut self.queries,
            &control,
            &mut self.work,
        )?
        .await
    }
}
impl<'s> TextInput<&'s str> for Native<'_> {
    async fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        let text = crate::catalog::work::copy_text(&mut self.work, source, &control).await?;
        if source == "fail_path" {
            return Err(SQLError::Internal("rejected cycle path".into()));
        }
        Ok(text)
    }
}

type Output = (
    Option<CteSearchPlan>,
    Pending<CteCyclePlan>,
    RewriteMemory,
    BudgetedVec<i64>,
    BudgetedVec<i64>,
);
type Input = ImageDecoder<Output, SQLError>;

fn input(
    source: Arc<CTE>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        let mut driver = construction::Driver::new(control)?;
        let source = source.as_ref();
        let native = &native;
        let output = driver.work().submit(move |work| async move {
            let make = || Native {
                queries: Queries {
                    work: work.clone(),
                    values: BudgetedVec::new(native),
                },
                work: work.clone(),
            };
            let mut mark = make();
            let mut default = make();
            let mut copying = Copying::for_rewrite(control).unwrap();
            let search = match &source.search {
                Some(search) => {
                    Some(metadata::construct_search(search, &mut copying, &mut mark).await?)
                }
                None => None,
            };
            let mut field = copying.begin_input_field()?;
            let cycle = metadata::construct_cycle(
                source.cycle.as_ref().unwrap(),
                &mut field,
                &mut mark,
                &mut default,
            )
            .await?;
            copying.finish_input_field("cycle", field);
            Ok::<_, SQLError>((
                search,
                cycle,
                copying.into_rewrite_memory(),
                mark.queries.values,
                default.queries.values,
            ))
        })?;
        while driver.advance().await? {
            channel.checkpoint().await?;
        }
        construction::receive(output).await
    })
    .unwrap()
}
fn advance(input: &mut Input) -> Result<Option<Output>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}
fn complete(input: &mut Input) -> (Output, usize) {
    for polls in 1..100_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("cycle construction stopped progressing")
}
fn fixture() -> CTE {
    let Statement::Select(mut query) = crate::compile("WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM t WHERE n < 3) SEARCH DEPTH FIRST BY n SET seq CYCLE n SET mark TO 1 DEFAULT 0 USING path SELECT * FROM t").unwrap().remove(0) else { panic!("query fixture") };
    query.with.remove(0)
}
fn query(value: i64) -> Expr {
    let Statement::Select(mut query) = crate::compile(&format!("SELECT (SELECT {value})"))
        .unwrap()
        .remove(0)
    else {
        unreachable!()
    };
    query.projections.remove(0).expr
}

#[test]
fn retained_cte_metadata_preserves_ordinary_names_flags_and_independent_query_slots() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for breadth_first in [false, true] {
        let mut source = fixture();
        source.search.as_mut().unwrap().breadth_first = breadth_first;
        let cycle = source.cycle.as_mut().unwrap();
        cycle.mark_value = query(1);
        cycle.mark_default = query(2);
        let mut statement = crate::compile("SELECT 1").unwrap().remove(0);
        let Statement::Select(query) = &mut statement else {
            unreachable!()
        };
        query.with.push(source.clone());
        let UnifiedPlan::Query(expected) = UnifiedPlan::lower(statement.clone()) else {
            unreachable!()
        };
        let control = ProductionControl::new(&native, &token, &token);
        let controlled = UnifiedPlan::lower_borrowed_for_rewrite(
            &statement,
            &crate::plan::NoRegisteredAggregates,
            &control,
        )
        .unwrap();
        let crate::plan::MutableExecutablePlan::Query(controlled) = controlled else {
            unreachable!()
        };
        assert_eq!(
            serde_json::to_value(&controlled.ctes[0]).unwrap(),
            serde_json::to_value(&expected.ctes[0]).unwrap()
        );
        let (output, _) = complete(&mut input(Arc::new(source), &native, &work, &token, &token));
        assert_eq!(
            serde_json::to_value(&output.0).unwrap(),
            serde_json::to_value(&expected.ctes[0].search).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&*output.1).unwrap(),
            serde_json::to_value(expected.ctes[0].cycle.as_ref().unwrap()).unwrap()
        );
        assert_eq!(&*output.3, &[1]);
        assert_eq!(&*output.4, &[2]);
        drop(controlled);
        assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_cte_metadata_bounds_wide_names_and_retires_each_suspended_prefix() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut source = fixture();
    source.cycle.as_mut().unwrap().mark_value = query(1);
    source.cycle.as_mut().unwrap().mark_default = query(2);
    let source = Arc::new(source);
    let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(value);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("57014")
                );
                assert_eq!(
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("XX001")
                );
            }
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            assert_eq!((native.used(), work.used()), (37, 0));
        }
    }
    drop(sibling);
    for count in [0, 2048] {
        let mut source = fixture();
        source.cycle.as_mut().unwrap().columns =
            (0..count).map(|i| format!("column_{i}")).collect();
        source.search.as_mut().unwrap().columns = source.cycle.as_ref().unwrap().columns.clone();
        source.cycle.as_mut().unwrap().path_column = "경로".repeat(4096);
        let (value, polls) = complete(&mut input(Arc::new(source), &native, &work, &token, &token));
        assert_eq!(value.1.columns.len(), count);
        assert_eq!(value.1.path_column, "경로".repeat(4096));
        assert!(polls >= count * 2);
        assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    for bytes in [0, 1, 128, 512, 1024] {
        let denied = MemoryBudget::new(bytes);
        let error = input(source.clone(), &denied, &work, &token, &token)
            .read(&mut std::io::empty())
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn retained_cte_metadata_retires_deep_marks_after_late_default_and_text_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for failure in ["query", "path"] {
                let mut source = fixture();
                let cycle = source.cycle.as_mut().unwrap();
                let mut deep = Expr::Literal(Value::Str("leaf".into()));
                for _ in 0..8192 {
                    deep = Expr::Not(Box::new(deep));
                }
                cycle.mark_value = deep;
                if failure == "query" {
                    cycle.mark_default = query(99);
                } else {
                    cycle.path_column = "fail_path".into();
                }
                let native = MemoryBudget::new(32 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let sibling = native.reserve(37).unwrap();
                let error = input(Arc::new(source), &native, &work, &token, &token)
                    .read(&mut std::io::empty())
                    .err()
                    .unwrap();
                assert_eq!(
                    error.to_string(),
                    format!("internal error: rejected cycle {failure}")
                );
                assert_eq!((native.used(), work.used()), (37, 0));
                drop(sibling);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_cte_metadata_retires_deep_marks_after_body_construction_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = fixture();
            let mut deep = Expr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                deep = Expr::Not(Box::new(deep));
            }
            source.cycle.as_mut().unwrap().mark_value = deep;
            let Statement::Select(mut body) =
                crate::compile("SELECT row_number() OVER w WINDOW w AS (ORDER BY 1)")
                    .unwrap()
                    .remove(0)
            else {
                unreachable!()
            };
            body.windows[0].inherited = Some(0);
            source.body = crate::ast::CteBody::Query(body);
            let native = MemoryBudget::new(32 << 20);
            let token = CancellationToken::new();
            let sibling = native.reserve(37).unwrap();
            {
                let control = ProductionControl::new(&native, &token, &token);
                let mut copying = Copying::for_rewrite(control).unwrap();
                let error = copying
                    .lower_cte(&source, &crate::plan::NoRegisteredAggregates)
                    .unwrap_err();
                assert!(error.to_string().contains("earlier named"), "{error}");
            }
            assert_eq!(native.used(), 37);
            drop(sibling);
        })
        .unwrap()
        .join()
        .unwrap();
}
