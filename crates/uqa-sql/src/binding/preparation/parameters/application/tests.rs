//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::FunctionBinding,
    plan::{ProjectionAliasUpdates, ProjectionPlan, TableFunctionPlan},
    ScalarExpr,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

mod installation;

enum Fixture {
    Aliases(Vec<ProjectionPlan>),
    Source(Box<SourcePlan>),
}

#[derive(Debug)]
enum Output {
    Aliases(ProjectionAliasUpdates),
    Source(SourceMetadataUpdate),
}

fn aliases(names: impl IntoIterator<Item = String>) -> Arc<Fixture> {
    Arc::new(Fixture::Aliases(
        names
            .into_iter()
            .map(|name| ProjectionPlan {
                expr: ScalarExpr::Column(name.into()),
                alias: None,
            })
            .collect(),
    ))
}

fn functions(count: usize) -> Arc<Fixture> {
    Arc::new(Fixture::Source(Box::new(SourcePlan::FunctionGroup {
        functions: (0..count)
            .map(|_| TableFunctionPlan {
                name: "same_name".into(),
                binding: None,
                output_name: "same_name".into(),
                relations: None,
                args: Vec::new(),
                column_aliases: Vec::new(),
                column_types: Vec::new(),
            })
            .collect(),
        alias: None,
        column_aliases: Vec::new(),
        ordinality: true,
    })))
}

fn observe(fixture: &Fixture, control: &ProductionControl<'_>) -> InputConstants {
    let mut constants = InputConstants::new(control);
    let mut include = |index: usize, binding: &Option<FunctionBinding>| {
        if index.is_multiple_of(2) {
            let selected = FunctionBinding::from_stored_json_with_control(
                &format!(r#"{{"name":"selected_{index}","argument_types":["integer"]}}"#),
                control,
            )
            .unwrap();
            constants
                .7
                .insert(AnalysisIdentity::from(binding), selected)
                .unwrap();
        }
    };
    match fixture {
        Fixture::Aliases(projections) => {
            for projection in projections {
                if !matches!(&projection.expr, ScalarExpr::Column(name) if name == "unselected") {
                    constants
                        .11
                        .insert(AnalysisIdentity::from(&projection.expr), 0)
                        .unwrap();
                }
            }
        }
        Fixture::Source(source) => {
            let source = source.as_ref();
            match source {
                SourcePlan::Function { binding, .. } => include(0, binding),
                SourcePlan::FunctionGroup { functions, .. } => {
                    for (index, function) in functions.iter().enumerate() {
                        include(index, &function.binding);
                    }
                }
                _ => (),
            }
            constants
                .8
                .sources
                .insert(AnalysisIdentity::from(source), crate::RowSchema::default())
                .unwrap();
        }
    }
    constants
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

type Decoder = ImageDecoder<Output, SQLError>;

fn decoder(
    fixture: Arc<Fixture>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    completed: Arc<AtomicUsize>,
    deny: bool,
) -> Decoder {
    let mut constants = observe(
        &fixture,
        &ProductionControl::new(native, original, invoking),
    );
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let cancellation = input.cancellation;
        let control = ProductionControl::new(&native, &original, cancellation);
        let mut work = WorkWindows {
            input: &mut input,
            completed,
        };
        let _held = deny.then(|| native.reserve(native.available()).unwrap());
        match &*fixture {
            Fixture::Aliases(projections) => {
                let before = constants.11.values().len();
                let output = constants
                    .projection_aliases_from(projections, &control, &mut work)
                    .await?;
                assert_eq!(constants.11.values().len(), before);
                Ok(Output::Aliases(output))
            }
            Fixture::Source(source) => {
                let output = constants
                    .source_metadata_from(source, &control, &mut work)
                    .await?;
                assert!(constants.7.is_empty());
                assert!(constants.8.sources.is_empty());
                Ok(Output::Source(output))
            }
        }
    })
    .unwrap()
}

fn step(decoder: &mut Decoder) -> Result<Option<Output>, SQLError> {
    decoder.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn selected(output: &Output) -> Vec<(usize, String)> {
    match output {
        Output::Aliases(aliases) => aliases
            .iter()
            .map(|(index, name)| (*index, (**name).clone()))
            .collect(),
        Output::Source(bindings) => bindings
            .iter()
            .map(|(index, binding)| (*index, binding.name.clone()))
            .collect(),
    }
}

#[test]
fn metadata_selection_preserves_names_binding_slots_and_observation_ownership() {
    let crate::plan::UnifiedPlan::Query(mut query) = crate::plan::UnifiedPlan::lower(
        crate::compile("SELECT * FROM generate_series(1, 2)")
            .unwrap()
            .remove(0),
    ) else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let single = Arc::new(Fixture::Source(Box::new(block.from.take().unwrap())));
    let mut labels = vec![
        ProjectionPlan {
            expr: ScalarExpr::Column("n".into()),
            alias: None,
        },
        ProjectionPlan {
            expr: ScalarExpr::QualifiedColumn {
                qualifier: "f".into(),
                column: "n".into(),
            },
            alias: None,
        },
        ProjectionPlan {
            expr: ScalarExpr::Column("n".into()),
            alias: Some("explicit".into()),
        },
        ProjectionPlan {
            expr: ScalarExpr::Param(0),
            alias: None,
        },
        ProjectionPlan {
            expr: ScalarExpr::Column("unselected".into()),
            alias: None,
        },
    ];
    labels.push(ProjectionPlan {
        expr: ScalarExpr::Column("日本語".repeat(2000).into()),
        alias: None,
    });
    let fixtures = [
        (
            Arc::new(Fixture::Aliases(labels)),
            vec![(0, "n".into()), (1, "n".into()), (5, "日本語".repeat(2000))],
        ),
        (
            functions(5),
            vec![
                (0, "selected_0".into()),
                (2, "selected_2".into()),
                (4, "selected_4".into()),
            ],
        ),
        (single, vec![(0, "selected_0".into())]),
        (functions(0), vec![]),
    ];
    for (fixture, expected) in fixtures {
        let native = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let mut constants = observe(&fixture, &control);
        let ordinary = match &*fixture {
            Fixture::Aliases(projections) => {
                Output::Aliases(constants.projection_aliases(projections, &control).unwrap())
            }
            Fixture::Source(source) => {
                Output::Source(constants.source_metadata(source, &control).unwrap())
            }
        };
        let actual = decoder(
            fixture,
            &native,
            &work,
            &token,
            &token,
            Arc::default(),
            false,
        )
        .read(&mut std::io::empty())
        .unwrap();
        assert_eq!(selected(&ordinary), expected);
        assert_eq!(selected(&actual), expected);
        if !expected.is_empty() {
            assert!(native.used() > 0);
        }
        drop((actual, ordinary, constants));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn metadata_selection_retires_every_pending_prefix_and_both_cancellation_owners() {
    for fixture in [aliases(["n".into(), "日本語".repeat(2000)]), functions(6)] {
        let native = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut complete = decoder(
            fixture.clone(),
            &native,
            &work,
            &token,
            &token,
            Arc::default(),
            false,
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
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut input = decoder(
                    fixture.clone(),
                    &native,
                    &work,
                    &original,
                    &invoking,
                    Arc::default(),
                    false,
                );
                for _ in 0..prefix {
                    assert!(step(&mut input).unwrap().is_none());
                }
                match cancellation {
                    1 => original.cancel(),
                    2 => invoking.cancel(),
                    _ => (),
                }
                if cancellation != 0 {
                    assert_eq!(step(&mut input).unwrap_err().sqlstate(), Some("57014"));
                }
                assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
                assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
            }
        }
    }
}

#[test]
fn metadata_selection_rejects_unfunded_names_and_reuses_released_binding_storage() {
    for (fixture, expected) in [
        (aliases(["n".into()]), None),
        (
            functions(3),
            Some(vec![(0, "selected_0".into()), (2, "selected_2".into())]),
        ),
    ] {
        let native = MemoryBudget::new(8 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let sibling = native.reserve(37).unwrap();
        let input = decoder(
            fixture,
            &native,
            &work,
            &token,
            &token,
            Arc::default(),
            true,
        );
        let result = input.read(&mut std::io::empty());
        match expected {
            Some(expected) => {
                let output = result.unwrap();
                assert_eq!(selected(&output), expected);
                drop(output);
            }
            None => assert_eq!(result.unwrap_err().sqlstate(), Some("53200")),
        }
        assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
    }
}

#[test]
fn metadata_selection_bounds_wide_sources_and_long_names_on_a_small_stack() {
    let fixtures = [
        aliases((0..4096).map(|i| format!("field_{i}"))),
        functions(4096),
        aliases(["日本語".repeat(8192)]),
    ];
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(move || {
            for fixture in fixtures {
                let native = MemoryBudget::new(32 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let completed = Arc::new(AtomicUsize::new(0));
                let mut input = decoder(
                    fixture,
                    &native,
                    &work,
                    &token,
                    &token,
                    completed.clone(),
                    false,
                );
                let mut polls = 0;
                loop {
                    polls += 1;
                    let before = completed.load(Ordering::Relaxed);
                    let output = step(&mut input).unwrap();
                    assert!(completed.load(Ordering::Relaxed) - before <= 1);
                    if let Some(output) = output {
                        assert!(!selected(&output).is_empty());
                        drop(output);
                        break;
                    }
                    assert!(polls < 20000);
                }
                assert!(polls > 16);
                drop(input);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
