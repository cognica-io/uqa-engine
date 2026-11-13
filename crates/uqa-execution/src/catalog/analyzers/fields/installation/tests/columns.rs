//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::AnalyzerPhase;

#[test]
fn field_installation_checks_declared_types_before_names_and_preserves_unnamed_defaults() {
    let resources = Arc::new(AnalyzerResources::default());
    let plain = binding(&resources);
    for named in [false, true] {
        for ordinary in [false, true] {
            let mut table = table(&plain);
            table.columns = TableDefinition::new(&vec![
                ColumnDef::nullable("a", uqa_sql::ast::ColumnType::Text),
                ColumnDef::nullable("b", uqa_sql::ast::ColumnType::Integer),
            ])
            .unwrap()
            .shared();
            let log = table.log.clone();
            let second = if named {
                plain.assigned(
                    "missing_type_validation_name",
                    plain.index.compiled.clone(),
                    AnalyzerPhase::Both,
                    AnalyzerBindingOwner::Field,
                )
            } else {
                plain.clone()
            };
            let saved = saved([("a", plain.clone()), ("b", second)]);
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(16 << 20);
            let names = uqa_storage::analyzer_catalog::AnalyzerCatalog::new(&mutations);
            let result = if ordinary {
                let state = Catalog {
                    fields: Arc::new(Mutex::new(FieldAnalyzerCatalog::new(&mutations))),
                    named: names,
                };
                restore(
                    tables(table),
                    state.clone(),
                    saved,
                    &GinAnalyzerOwners::new(),
                    false,
                )
                .map(|_| state.fields())
            } else {
                let saved = retained(saved, resources.clone(), &control);
                let mut input = FieldAnalyzerInstallation::new(
                    tables(table),
                    saved,
                    names,
                    &mutations,
                    &control,
                )
                .unwrap();
                finish(&mut input)
            };
            match result {
                Ok(fields) => {
                    assert!(!named);
                    assert_eq!(fields.len(), 2);
                }
                Err(error) => {
                    assert!(named);
                    assert_eq!(
                        error.to_string(),
                        "set_table_analyzer: column `public.docs`.`b` must be TEXT, got Integer"
                    );
                }
            }
            assert_eq!(log.lock().len(), if named { 1 } else { 2 });
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
    }
}
