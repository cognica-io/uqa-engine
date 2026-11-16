//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::SelectStmt, plan::MutableQueryPlan};

type Output = Prefix<MutableQueryPlan>;
type Input = ImageDecoder<Output, SQLError>;

fn input(
    source: Arc<SelectStmt>,
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
        let output = driver.work().submit(move |work| async move {
            let mut copying = Copying::for_rewrite(control).unwrap();
            copying.plan_retirement = Some(Context::new(&control)?);
            let query = build(
                construct::Source::Select(source),
                &mut copying,
                work.clone(),
            )?
            .await?;
            let memory = copying.into_rewrite_memory();
            query
                .map(|query| MutableQueryPlan::from_rewrite_parts(query, Some(memory)))
                .normalize_window_definitions_from(&control, work)?
                .await
        })?;
        while driver.advance().await? {
            channel.checkpoint().await?;
        }
        construction::receive(output).await
    })
    .unwrap()
}

fn advance(reader: &mut Input) -> Result<Option<Output>> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(reader: &mut Input) -> (Output, usize) {
    for polls in 1..1_000_000 {
        if let Some(output) = advance(reader).unwrap() {
            return (output, polls);
        }
    }
    panic!("query normalization stopped progressing")
}

#[test]
fn retained_query_normalization_preserves_canonical_scopes_and_native_owners() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "SELECT row_number() OVER w WINDOW w AS (ORDER BY 1)",
        "SELECT sum(v) OVER child, row_number() OVER base FROM (VALUES(1)) t(v) WINDOW base AS (ORDER BY (SELECT 7)), child AS (base ROWS CURRENT ROW)",
        "WITH q AS (SELECT row_number() OVER w n WINDOW w AS (ORDER BY 2)) SELECT row_number() OVER w, (SELECT row_number() OVER w WINDOW w AS (ORDER BY 3)) FROM q WINDOW w AS (ORDER BY 1)",
        "WITH q AS (UPDATE t SET a=(SELECT row_number() OVER w WINDOW w AS (ORDER BY 4)) RETURNING a) SELECT row_number() OVER w FROM q WINDOW w AS (ORDER BY 1)",
        "SELECT row_number() OVER w FROM (SELECT row_number() OVER w n WINDOW w AS (ORDER BY 2)) a JOIN (SELECT 1) b ON true WINDOW w AS (ORDER BY 1)",
        "SELECT row_number() OVER w WINDOW w AS (ORDER BY 1) UNION ALL SELECT row_number() OVER w WINDOW w AS (ORDER BY 2)",
    ] {
        let source = blocks::query(sql);
        let expected = UnifiedPlan::lower(Statement::Select(Box::new(source.clone())));
        let (actual, polls) = complete(&mut input(Arc::new(source), &native, &work, &token, &token));
        let UnifiedPlan::Query(expected) = expected else { panic!("query"); };
        assert_eq!(serde_json::to_value(&**actual).unwrap(), serde_json::to_value(&*expected).unwrap(), "{sql}");
        assert!(polls > 1);
        assert_eq!(work.used(), 0);
        let allocations = allocation_counter::measure(|| drop(actual));
        assert_eq!(allocations.count_total, 0, "{sql}: {allocations:?}");
        assert_eq!(native.used(), 0, "{sql}");
    }
}

#[test]
fn retained_query_normalization_releases_every_interrupted_prefix() {
    let source = Arc::new(blocks::query("SELECT row_number() OVER w, (SELECT row_number() OVER w WINDOW w AS (ORDER BY 2)) WINDOW w AS (ORDER BY 1)"));
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (output, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancel != 0 {
                if cancel == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            }
            drop(reader);
            assert_eq!(
                (native.used(), work.used()),
                (37, 0),
                "prefix={prefix}, cancellation={cancel}"
            );
        }
    }
    drop(sibling);
    for bytes in [0, 1, 512, 4096, 8192, 32768] {
        let denied = MemoryBudget::new(bytes);
        let error = input(source.clone(), &denied, &work, &token, &token)
            .read(&mut std::io::empty())
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn retained_query_normalization_bounds_structural_polling_and_cleanup_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(128 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut source = blocks::query("SELECT row_number() OVER w WINDOW w AS (ORDER BY 1)");
            for _ in 0..512 {
                let mut parent = blocks::query("SELECT 1");
                parent.projections[0].expr = Expr::ScalarSubquery(Box::new(source));
                source = parent;
            }
            let output = input(Arc::new(source), &native, &work, &token, &token)
                .read(&mut std::io::empty())
                .unwrap();
            let allocations = allocation_counter::measure(|| drop(output));
            assert_eq!(allocations.count_total, 0, "{allocations:?}");
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
