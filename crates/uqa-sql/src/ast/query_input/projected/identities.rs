//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    finish, frame, Fields, Input, JsonDiagnostic, JsonPosition, Parts, Projected, Read, Result,
};
use crate::ast::{InternalRelationId, ObjectAclEntry};
use uqa_core::{catalog_identity::CatalogObjectIdentity, catalog_role::RoleIdentity};

projected_struct!(RoleIdentity {
    oid: required,
    object_id: required
});
projected_struct!(ObjectAclEntry {
    role: required,
    grantor: required,
    grant_option: required
});

impl Projected for InternalRelationId {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let raw = input.read::<u64>(work).await?.into_parts().0;
            input.scalar(Self::from_raw(raw))
        })
    }
}

impl Projected for [u8; 16] {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            let Some(mut values) = input.value.elements() else {
                return Err(input.type_error("an array of length 16", work).await?);
            };
            let length = values.len();
            let mut result = [0; 16];
            for (index, output) in result.iter_mut().enumerate() {
                let value = values.next().ok_or_else(|| {
                    JsonDiagnostic::invalid_length(
                        index,
                        "an array of length 16",
                        JsonPosition::unlocated(),
                    )
                })?;
                *output = input.child(value).read::<u8>(work).await?.into_parts().0;
            }
            if values.next().is_some() {
                return Err(JsonDiagnostic::invalid_length(
                    length,
                    "fewer elements in array",
                    JsonPosition::unlocated(),
                )
                .into());
            }
            input.scalar(result)
        })
    }
}

impl Projected for CatalogObjectIdentity {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            const NAMES: &[&str] = &["object_id", "oid"];
            let mut cursor = Fields::new(
                input,
                NAMES,
                [3, 3],
                "struct CatalogObjectIdentity",
                false,
                work,
            )
            .await?;
            let (mut object_id, mut oid) = (None, None);
            while let Some((index, value)) = cursor.next(work).await? {
                match index {
                    Some(0) => object_id = Some(input.child(value).read(work).await?),
                    Some(1) => oid = Some(input.child(value).read(work).await?),
                    Some(_) => unreachable!("catalog identity field"),
                    None => {
                        return Err(JsonDiagnostic::unknown_field(
                            input
                                .text(cursor.name().expect("unknown map field"), work)
                                .await?,
                            NAMES,
                            JsonPosition::unlocated(),
                        )
                        .into())
                    }
                }
            }
            let mut parts = Parts::new(input.control);
            let value = Self {
                object_id: parts.take(object_id.expect("complete identity")),
                oid: parts.take(oid.expect("complete identity")),
            };
            finish(parts, value)
        })
    }
}
