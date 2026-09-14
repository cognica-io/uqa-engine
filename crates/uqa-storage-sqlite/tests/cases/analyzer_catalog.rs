//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Equivalent descriptor/label replacement and atomicity in both catalog providers.

use std::sync::Arc;
use uqa_storage::{
    AnalyzerBindingOwner, AnalyzerPhase, CatalogFacade, FieldAnalyzerBinding, KeyValueCatalog,
    MemoryKeyValueStore,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection};

fn binding() -> String {
    let revision = uqa_analysis::keyword_analyzer().compile().unwrap();
    FieldAnalyzerBinding::unassigned(revision.clone(), revision.clone())
        .assigned(
            "whole",
            revision,
            AnalyzerPhase::Both,
            AnalyzerBindingOwner::Field,
        )
        .to_json()
        .unwrap()
}

#[test]
fn catalog_replacement_and_legacy_writes_keep_descriptors_and_labels_coherent() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalogs: Vec<Box<dyn CatalogFacade>> = vec![
        Box::new(Catalog::open(connection).unwrap()),
        Box::new(KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()))),
    ];
    let revision = uqa_analysis::keyword_analyzer().compile().unwrap();
    let descriptor = revision.descriptor().canonical_json();
    let config = serde_json::to_string(&revision.descriptor().configuration().unwrap()).unwrap();
    let binding = binding();
    for catalog in catalogs {
        catalog
            .save_analyzer_revision("whole", &config, descriptor)
            .unwrap();
        assert_eq!(
            catalog.load_analyzer_descriptors().unwrap(),
            [("whole".into(), descriptor.into())]
        );
        catalog
            .replace_table_field_analyzer_binding("public.docs", "body", "both", "whole", &binding)
            .unwrap();
        catalog
            .save_table_field_analyzer("public.docs", "body", "search", "standard")
            .unwrap();
        assert!(catalog
            .load_table_field_analyzer_bindings()
            .unwrap()
            .is_empty());
        assert_eq!(catalog.load_table_field_analyzers().unwrap().len(), 2);
        catalog
            .replace_table_field_analyzer_binding("public.docs", "body", "both", "whole", &binding)
            .unwrap();
        assert_eq!(
            catalog.load_table_field_analyzer_bindings().unwrap(),
            [("public.docs".into(), "body".into(), binding.clone())]
        );
        assert_eq!(
            catalog.load_table_field_analyzers().unwrap(),
            [(
                "public.docs".into(),
                "body".into(),
                "both".into(),
                "whole".into()
            )]
        );
        catalog.save_analyzer("whole", &config).unwrap();
        assert!(catalog.load_analyzer_descriptors().unwrap().is_empty());
        catalog
            .save_analyzer_revision("whole", &config, descriptor)
            .unwrap();
        catalog.drop_analyzer("whole").unwrap();
        assert!(catalog.load_analyzer_descriptors().unwrap().is_empty());
        assert!(catalog.load_analyzers().unwrap().is_empty());
        catalog
            .drop_table_field_analyzer_field("public.docs", "body")
            .unwrap();
        assert!(catalog
            .load_table_field_analyzer_bindings()
            .unwrap()
            .is_empty());
        assert!(catalog.load_table_field_analyzers().unwrap().is_empty());
    }
}

#[test]
fn failed_legacy_write_does_not_clear_a_durable_sqlite_binding() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .replace_table_field_analyzer_binding("public.docs", "body", "both", "whole", &binding())
        .unwrap();
    let before = catalog.load_table_field_analyzer_bindings().unwrap();
    connection.with(|db| {
        db.execute_batch("CREATE TRIGGER reject_analyzer_label BEFORE INSERT ON _table_field_analyzers BEGIN SELECT RAISE(ABORT, 'rejected label'); END;")?;
        Ok(())
    }).unwrap();
    assert!(catalog
        .save_table_field_analyzer("public.docs", "body", "search", "standard")
        .is_err());
    assert_eq!(
        catalog.load_table_field_analyzer_bindings().unwrap(),
        before
    );
    assert_eq!(catalog.load_table_field_analyzers().unwrap().len(), 1);
}
