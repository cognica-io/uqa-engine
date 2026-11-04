//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptions retain exact object/subobject addresses independently of current object names.

use super::{CapturedComments, CommentRegistry};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::SQLError;

pub fn comments(
    values: &CommentRegistry,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedComments>, SQLError> {
    values.capture(control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::comments::CommentAddress;
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    use uqa_sql::{ast::RelationPersistence, catalog::dependencies::ObjectAddress};

    #[test]
    fn comment_capture_preserves_addresses_and_exact_text_with_independent_admission() {
        let source = CommentRegistry::try_from_iter(
            [
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
            ]
            .map(|(address, text)| (address, text, RelationPersistence::Permanent)),
        )
        .unwrap();
        let memory = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let captured = comments(&source, &control).unwrap();
        assert!(captured.iter().eq(source.iter()));
        drop(source);
        assert_eq!(captured.len(), 3);
        drop(captured);
        assert_eq!(memory.used(), 0);
        let source = CommentRegistry::try_from_iter(
            [(
                CommentAddress::from(ObjectAddress {
                    class_id: 1259,
                    object_id: 20_002,
                    sub_id: 1,
                }),
                "x".repeat(32768),
            )]
            .map(|(address, text)| (address, text, RelationPersistence::Permanent)),
        )
        .unwrap();
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
