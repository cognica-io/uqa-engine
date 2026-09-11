//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::{Cell, RefCell};
use uqa_ml::TrainingExample;
use uqa_storage::StorageBackendError;

struct Fixture {
    events: RefCell<Vec<&'static str>>,
    retained: Cell<bool>,
    documents: BTreeMap<DocId, Document>,
    failure: Option<&'static str>,
    saved: RefCell<Vec<DeepModel>>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
            retained: Cell::new(false),
            documents: BTreeMap::from([
                (
                    1,
                    BTreeMap::from([
                        ("features".into(), Value::List(vec![Value::Float(2.0)])),
                        ("label".into(), Value::Int(0)),
                    ]),
                ),
                (
                    2,
                    BTreeMap::from([
                        ("features".into(), Value::List(vec![Value::Int(7)])),
                        ("label".into(), Value::Float(1.0)),
                    ]),
                ),
            ]),
            failure: None,
            saved: RefCell::new(Vec::new()),
        }
    }

    fn context(&self) -> ModelTrainingContext<'_> {
        ModelTrainingContext {
            tables: self,
            models: self,
        }
    }
}

struct Table<'a>(&'a Fixture);

impl Drop for Table<'_> {
    fn drop(&mut self) {
        assert!(self.0.retained.replace(false));
        self.0.events.borrow_mut().push("release");
    }
}

impl TrainingTable for Table<'_> {
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        assert!(self.0.retained.get());
        self.0.events.borrow_mut().push("scan");
        if self.0.failure == Some("scan") {
            return Err(StorageBackendError::Other("scan failure".into()));
        }
        Ok(vec![2, 1])
    }
}

impl TrainingTables for Fixture {
    fn training_table(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<Box<dyn TrainingTable + '_>>> {
        self.events.borrow_mut().push("resolve");
        if self.failure == Some("resolve") {
            return Err(StorageBackendError::Other("lookup failure".into()));
        }
        if name != "training" {
            return Ok(None);
        }
        assert!(!self.retained.replace(true));
        Ok(Some(Box::new(Table(self))))
    }

    fn training_documents(
        &self,
        table: &str,
        doc_ids: &[DocId],
        projection: &[String],
    ) -> Result<BTreeMap<DocId, Document>, SQLError> {
        assert!(self.retained.get());
        assert_eq!(table, "training");
        assert_eq!(doc_ids, &[2, 1]);
        assert_eq!(projection, &["features", "label"]);
        self.events.borrow_mut().push("project");
        if self.failure == Some("project") {
            return Err(SQLError::Internal("projection failure".into()));
        }
        Ok(self.documents.clone())
    }
}

impl TrainedModels for Fixture {
    fn save_model(&self, name: &str, model: &DeepModel) -> Result<(), SQLError> {
        assert!(
            !self.retained.get(),
            "the source table handle ends before model publication"
        );
        assert_eq!(name, "trained");
        self.events.borrow_mut().push("save");
        if self.failure == Some("save") {
            return Err(SQLError::Internal("model persistence failed".into()));
        }
        self.saved.borrow_mut().push(model.clone());
        Ok(())
    }
}

fn training_set() -> TrainingSet {
    TrainingSet {
        examples: vec![
            TrainingExample {
                features: vec![2.0],
                label: 0,
            },
            TrainingExample {
                features: vec![7.0],
                label: 1,
            },
        ],
        class_count: None,
    }
}

#[test]
fn table_training_retains_the_source_until_materialization_and_publishes_the_trained_model() {
    let fixture = Fixture::new();
    let output = fixture
        .context()
        .train_table("trained", "training", &LearnOptions::default())
        .unwrap();
    let expected = uqa_ml::deep_learn(&training_set(), &LearnOptions::default()).unwrap();
    assert_eq!(output, expected);
    assert_eq!(&*fixture.saved.borrow(), &[expected.model]);
    assert_eq!(
        &*fixture.events.borrow(),
        &["resolve", "scan", "project", "release", "save"]
    );
}

#[test]
fn table_training_errors_keep_lookup_scan_projection_and_row_validation_order() {
    for (failure, expected) in [
        ("resolve", "resolve table `training`"),
        ("scan", "scan deep_learn table `training`"),
        ("project", "projection failure"),
    ] {
        let mut fixture = Fixture::new();
        fixture.failure = Some(failure);
        let error = fixture
            .context()
            .train_table("trained", "training", &LearnOptions::default())
            .unwrap_err();
        assert!(matches!(error, SQLError::Internal(message) if message.contains(expected)));
        assert!(!fixture.retained.get());
        assert!(fixture.saved.borrow().is_empty());
        assert!(!fixture.events.borrow().contains(&"save"));
    }
    let fixture = Fixture::new();
    let error = fixture
        .context()
        .train_table("trained", "absent", &LearnOptions::default())
        .unwrap_err();
    assert!(matches!(error, SQLError::UnknownTable(name) if name == "absent"));
    assert_eq!(&*fixture.events.borrow(), &["resolve"]);
    let mut fixture = Fixture::new();
    fixture.documents.insert(1, Document::new());
    for (field, value, expected) in [
        (None, Value::Null, "deep_learn table \"training\" row 1 is missing `features`"),
        (Some("features"), Value::Null, "deep_learn table \"training\" row 1 is missing `label`"),
        (Some("label"), Value::Int(0), "deep_learn table \"training\" row 1 `features`: expected feature array, got Null"),
        (Some("features"), Value::List(vec![Value::Int(2)]), ""),
        (Some("label"), Value::Int(-1), "deep_learn table \"training\" row 1 `label`: expected non-negative integer label, got Int(-1)"),
    ] {
        if let Some(field) = field {
            fixture.documents.get_mut(&1).unwrap().insert(field.into(), value);
        }
        if expected.is_empty() { continue; }
        let error = fixture.context().train_table("trained", "training", &LearnOptions::default()).unwrap_err();
        assert!(matches!(error, SQLError::TypeMismatch(message) if message == expected));
        assert!(!fixture.retained.get());
        assert!(fixture.saved.borrow().is_empty());
    }
}

#[test]
fn invalid_json_and_ml_training_fail_before_any_table_read_or_model_save() {
    let fixture = Fixture::new();
    let error = fixture
        .context()
        .train_json("trained", "{", &LearnOptions::default())
        .unwrap_err();
    assert!(
        matches!(error, SQLError::TypeMismatch(message) if message.starts_with("invalid deep_learn training JSON: "))
    );
    let error = fixture
        .context()
        .train_json("trained", r#"{"examples":[]}"#, &LearnOptions::default())
        .unwrap_err();
    assert!(matches!(error, SQLError::Unsupported(message) if message.starts_with("deep_learn: ")));
    assert!(fixture.events.borrow().is_empty());
    assert!(fixture.saved.borrow().is_empty());
}

#[test]
fn json_training_returns_model_persistence_failure_without_a_success_report() {
    let mut fixture = Fixture::new();
    fixture.failure = Some("save");
    let json = serde_json::to_string(&training_set()).unwrap();
    let error = fixture
        .context()
        .train_json("trained", &json, &LearnOptions::default())
        .unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "model persistence failed"));
    assert_eq!(&*fixture.events.borrow(), &["save"]);
    assert!(fixture.saved.borrow().is_empty());
}
