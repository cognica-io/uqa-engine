//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptions retain exact object/subobject addresses independently of current object names.

use super::{CommentAddress, CommentRegistry};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLError;

pub fn comments(
    values: &CommentRegistry,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(CommentAddress, String)>>, SQLError> {
    let mut entries = ProductionVec::new(*control);
    entries.reserve(values.len())?;
    for (address, description) in values {
        let (description, memory) = control.copy_text(description)?.into_parts();
        entries.push_produced(control.finish((*address, description), memory)?)?;
    }
    Ok(entries.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    use uqa_sql::catalog::dependencies::ObjectAddress;

    #[test]
    fn comment_capture_preserves_addresses_and_exact_text_with_independent_admission() {
        let source = CommentRegistry::from([
            (
                CommentAddress::from(ObjectAddress {
                    class_id: 1259,
                    object_id: 20_001,
                    sub_id: 0,
                }),
                "object".into(),
            ),
            (
                CommentAddress::from(ObjectAddress {
                    class_id: 1259,
                    object_id: 20_001,
                    sub_id: 3,
                }),
                "column 한글\n".into(),
            ),
            (
                CommentAddress::from(ObjectAddress {
                    class_id: 1247,
                    object_id: 20_001,
                    sub_id: 0,
                }),
                "type".into(),
            ),
        ]);
        let memory = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let captured = comments(&source, &control).unwrap();
        assert!(captured.iter().map(|(k, v)| (k, v)).eq(source.iter()));
        drop(source);
        assert_eq!(captured.len(), 3);
        drop(captured);
        assert_eq!(memory.used(), 0);
        let source = CommentRegistry::from([(
            CommentAddress::from(ObjectAddress {
                class_id: 1259,
                object_id: 20_002,
                sub_id: 1,
            }),
            "x".repeat(32768),
        )]);
        assert_eq!(
            comments(&source, &control).unwrap_err().sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), 0);
        token.cancel();
        assert!(matches!(
            comments(&source, &control),
            Err(SQLError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}
