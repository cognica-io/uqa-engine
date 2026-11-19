//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{JoinExecutionStrategy, RelationalPlan, UnifiedPlan};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

fn leaf() -> SourcePlan {
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(crate::compile("SELECT 1 FROM t").unwrap().remove(0))
    else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    block.from.take().unwrap()
}

fn join(left: SourcePlan, right: SourcePlan) -> SourcePlan {
    SourcePlan::Join {
        comma: false,
        left: Box::new(left),
        right: Box::new(right),
        kind: crate::ast::JoinKind::Inner,
        on: None,
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral: false,
        strategy: JoinExecutionStrategy::Auto,
    }
}

fn observe(
    source: &SourcePlan,
    missing: Option<usize>,
    control: &ProductionControl<'_>,
) -> (QuerySchemas, Vec<RowSchema>) {
    let mut schemas = QuerySchemas::new(control);
    let mut retained = Vec::new();
    let mut pending = vec![source];
    while let Some(source) = pending.pop() {
        let index = retained.len();
        let name = format!("source_{index}");
        let schema = RowSchema::with_column_metadata_with_control(
            None,
            [(name.as_str(), Some(&crate::ColumnType::Integer), None)],
            control,
        )
        .unwrap();
        if missing != Some(index) {
            schemas
                .sources
                .insert(AnalysisIdentity::from(source), schema.clone())
                .unwrap();
        }
        retained.push(schema);
        if let SourcePlan::Join { left, right, .. } = source {
            pending.extend([right.as_ref(), left.as_ref()]);
        }
    }
    (schemas, retained)
}

fn flattened(source: &AnalyzedSource, expected: &[RowSchema]) -> Vec<String> {
    let mut pending = vec![source];
    let mut output = Vec::new();
    while let Some(source) = pending.pop() {
        let name = source.schema.columns()[0].clone();
        let index: usize = name.strip_prefix("source_").unwrap().parse().unwrap();
        assert!(source.schema.shares_index(&expected[index]));
        output.push(name);
        if let Some(children) = &source.children {
            pending.extend([&children.1, &children.0]);
        }
    }
    output
}

struct WorkWindows<'a, 'b> {
    input: &'a mut AsyncImageInput<'b>,
    completed: Arc<AtomicUsize>,
}
impl Work for WorkWindows<'_, '_> {
    async fn checkpoint(&mut self) -> Result<(), ImageIoError> {
        self.input.checkpoint().await?;
        self.completed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        crate::catalog::work::copy_text(self, source, control).await
    }
}

type Output = (
    Option<Produced<AnalyzedSource>>,
    QuerySchemas,
    Vec<RowSchema>,
);
type Decoder = ImageDecoder<Output, SQLError>;

fn input(
    source: Arc<SourcePlan>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    missing: Option<usize>,
    completed: Arc<AtomicUsize>,
) -> Decoder {
    let (mut schemas, retained) = observe(
        &source,
        missing,
        &ProductionControl::new(native, original, invoking),
    );
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        let mut work = WorkWindows {
            input: &mut channel,
            completed,
        };
        let output = schemas
            .take_source_from(&source, &control, &mut work)
            .await?;
        Ok((output, schemas, retained))
    })
    .unwrap()
}

fn step(input: &mut Decoder) -> Result<Option<Output>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn source_metadata_assembly_preserves_preorder_selection_and_original_schema_owners() {
    let source = Arc::new(join(join(leaf(), leaf()), join(leaf(), leaf())));
    for missing in std::iter::once(None).chain((0..7).map(Some)) {
        let native = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        for controlled in [false, true] {
            let control = if controlled {
                ProductionControl::new(&native, &token, &token)
            } else {
                ProductionControl::uncontrolled()
            };
            let (mut schemas, retained) = observe(&source, missing, &control);
            let output = schemas.take_source_with_control(&source, &control).unwrap();
            check(output.as_deref(), &schemas, &retained, missing);
            drop((output, schemas, retained));
        }
        let (output, schemas, retained) = input(
            source.clone(),
            &native,
            &work,
            &token,
            &token,
            missing,
            Arc::default(),
        )
        .read(&mut std::io::empty())
        .unwrap();
        check(output.as_deref(), &schemas, &retained, missing);
        drop((output, schemas, retained));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

fn check(
    output: Option<&AnalyzedSource>,
    schemas: &QuerySchemas,
    retained: &[RowSchema],
    missing: Option<usize>,
) {
    if let Some(missing) = missing {
        assert!(output.is_none());
        let mut remaining = schemas
            .sources
            .values()
            .map(|schema| schema.columns()[0].clone())
            .collect::<Vec<_>>();
        remaining.sort();
        assert_eq!(
            remaining,
            ((missing + 1)..7)
                .map(|i| format!("source_{i}"))
                .collect::<Vec<_>>()
        );
    } else {
        assert_eq!(
            flattened(output.unwrap(), retained),
            (0..7).map(|i| format!("source_{i}")).collect::<Vec<_>>()
        );
        assert!(schemas.sources.is_empty());
    }
}

#[test]
fn source_metadata_assembly_retires_every_interruption_and_denied_parent_allocation() {
    let source = Arc::new(join(join(leaf(), leaf()), join(leaf(), leaf())));
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for missing in [None, Some(6)] {
        let mut complete = input(
            source.clone(),
            &native,
            &work,
            &token,
            &token,
            missing,
            Arc::default(),
        );
        let mut polls = 0;
        loop {
            polls += 1;
            if let Some(output) = step(&mut complete).unwrap() {
                drop(output);
                break;
            }
            assert!(polls < 1000);
        }
        drop(complete);
        let sibling = native.reserve(37).unwrap();
        for prefix in 0..polls {
            for cancel in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut decoder = input(
                    source.clone(),
                    &native,
                    &work,
                    &original,
                    &invoking,
                    missing,
                    Arc::default(),
                );
                for _ in 0..prefix {
                    assert!(step(&mut decoder).unwrap().is_none());
                }
                match cancel {
                    1 => original.cancel(),
                    2 => invoking.cancel(),
                    _ => (),
                }
                if cancel != 0 {
                    assert!(
                        matches!(step(&mut decoder), Err(error) if error.sqlstate() == Some("57014"))
                    );
                }
                assert_eq!(allocation_counter::measure(|| drop(decoder)).count_total, 0);
                assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
            }
        }
        if missing.is_none() {
            let mut decoder = input(
                source.clone(),
                &native,
                &work,
                &token,
                &token,
                None,
                Arc::default(),
            );
            for _ in 0..polls - 1 {
                assert!(step(&mut decoder).unwrap().is_none());
            }
            let held = native.reserve(native.available()).unwrap();
            assert!(matches!(step(&mut decoder), Err(error) if error.sqlstate() == Some("53200")));
            drop((decoder, held));
            assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
        }
    }
}

#[test]
fn source_metadata_assembly_bounds_deep_work_and_retirement_on_a_small_stack() {
    let mut source = leaf();
    for _ in 0..512 {
        source = join(source, leaf());
    }
    let source = Arc::new(source);
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let completed = Arc::new(AtomicUsize::new(0));
    // The source syntax remains separately owned while metadata construction runs.
    let mut decoder = input(
        source.clone(),
        &native,
        &work,
        &token,
        &token,
        None,
        completed.clone(),
    );
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(move || {
            let mut polls = 0;
            loop {
                polls += 1;
                let before = completed.load(Ordering::Relaxed);
                if let Some((output, schemas, retained)) = step(&mut decoder).unwrap() {
                    assert!(completed.load(Ordering::Relaxed) - before <= 1);
                    assert_eq!(flattened(output.as_deref().unwrap(), &retained).len(), 1025);
                    assert!(schemas.sources.is_empty());
                    assert_eq!(
                        allocation_counter::measure(|| drop((output, schemas, retained)))
                            .count_total,
                        0
                    );
                    break;
                }
                assert!(completed.load(Ordering::Relaxed) - before <= 1);
                assert!(polls < 10_000);
            }
            assert!(polls > 1025);
            drop(decoder);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
    drop(source);
}
