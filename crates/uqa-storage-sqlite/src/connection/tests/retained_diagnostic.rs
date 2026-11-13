//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    json::{JsonDiagnostic, JsonPosition},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_storage::{StorageBackendError, TransactionError};

#[test]
fn retained_native_diagnostics_preserve_their_owner_across_provider_and_transaction_boundaries() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let text = "stored 한🙂 name".repeat(1024);
    let diagnostic = JsonDiagnostic::unknown_field(
        control.copy_text(&text).unwrap(),
        &["owner"],
        JsonPosition::unlocated(),
    );
    let cause: Arc<dyn std::error::Error + Send + Sync> = Arc::new(diagnostic);
    let expected = cause.to_string();
    let weak = Arc::downgrade(&cause);
    let error = SQLiteError::from(StorageBackendError::RetainedDiagnostic(cause));
    assert_eq!(error.to_string(), expected);
    let TransactionError::Storage(error) = TransactionError::from(error) else {
        panic!("retained storage cause lost");
    };
    let StorageBackendError::RetainedDiagnostic(cause) = &error else {
        panic!("retained native diagnostic lost");
    };
    assert!(Arc::ptr_eq(cause, &weak.upgrade().unwrap()));
    assert!(memory.used() >= text.len());
    let roundtrip = StorageBackendError::from(SQLiteError::from(error));
    assert_eq!(roundtrip.to_string(), expected);
    drop(roundtrip);
    assert!(weak.upgrade().is_none());
    assert_eq!(memory.used(), 0);
}
