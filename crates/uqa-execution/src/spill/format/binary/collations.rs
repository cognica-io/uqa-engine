//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional, bounded schema collation records in typed batch spills.

use super::decode::BinaryReader;
use super::encode::{add_identity_size, add_size, write_identity, write_u64};
use crate::batch::{ColumnIdentity, RowSchema};
use crate::physical::ExecResult;
use crate::spill::format::spill_error;
use std::collections::HashSet;
use std::io::Write;
use uqa_sql::ast::InternalColumnRef;
use uqa_sql::schema::CollationBinding;

pub(super) const FLAG: u8 = 2;

pub(super) fn encoded_size(schema: &RowSchema) -> ExecResult<usize> {
    let mut bytes = 0;
    for (binding, _) in schema.collation_overrides() {
        if bytes == 0 {
            bytes = 8;
        }
        add_size(&mut bytes, 16, "schema collation tag and identity")?;
        match binding {
            CollationBinding::Column(_) => add_size(&mut bytes, 8, "collation column")?,
            CollationBinding::Alias(identity) => {
                add_identity_size(&mut bytes, identity, "collation alias")?;
            }
            CollationBinding::Internal(_) => {
                add_size(&mut bytes, 16, "collation internal attribute")?;
            }
        }
    }
    Ok(bytes)
}

pub(super) fn encode(writer: &mut impl Write, schema: &RowSchema) -> ExecResult<()> {
    write_u64(writer, schema.collation_overrides().count())?;
    for (binding, oid) in schema.collation_overrides() {
        match binding {
            CollationBinding::Column(position) => {
                write_u64(writer, 0)?;
                write_u64(writer, position)?;
            }
            CollationBinding::Alias(identity) => {
                write_u64(writer, 1)?;
                write_identity(writer, identity)?;
            }
            CollationBinding::Internal(column) => {
                write_u64(writer, 2)?;
                writer
                    .write_all(&column.relation().raw().to_le_bytes())
                    .map_err(|error| spill_error(format!("write collation relation: {error}")))?;
                write_u64(writer, column.attribute())?;
            }
        }
        write_u64(
            writer,
            usize::try_from(oid).expect("collation OID fits usize"),
        )?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Binding {
    Column(usize),
    Alias(ColumnIdentity),
    Internal(InternalColumnRef),
}

pub(super) fn decode(
    reader: &mut BinaryReader<'_>,
    mut schema: RowSchema,
    binding_count: usize,
) -> ExecResult<RowSchema> {
    let count = reader.read_count("schema collation count", 24)?;
    if count > binding_count {
        return Err(spill_error(
            "schema collation count exceeds the declared bindings",
        ));
    }
    let mut seen = HashSet::new();
    seen.try_reserve(count)
        .map_err(|error| spill_error(format!("cannot allocate collation bindings: {error}")))?;
    for _ in 0..count {
        let binding = match reader.read_u64("schema collation binding tag")? {
            0 => Binding::Column(
                usize::try_from(reader.read_u64("schema collation column")?)
                    .map_err(|_| spill_error("collation column exceeds address space"))?,
            ),
            1 => Binding::Alias(reader.read_identity("schema collation alias")?),
            2 => {
                let relation = reader.read_u64("schema collation relation")?;
                let attribute = u32::try_from(reader.read_u64("schema collation attribute")?)
                    .map_err(|_| spill_error("collation attribute exceeds u32"))?;
                Binding::Internal(InternalColumnRef::from_raw(relation, attribute))
            }
            tag => {
                return Err(spill_error(format!(
                    "invalid schema collation binding tag {tag}"
                )))
            }
        };
        if !seen.insert(binding.clone()) {
            return Err(spill_error("duplicate schema collation binding"));
        }
        let oid = u32::try_from(reader.read_u64("schema collation OID")?)
            .map_err(|_| spill_error("collation OID exceeds u32"))?;
        let binding = match &binding {
            Binding::Column(position) => CollationBinding::Column(*position),
            Binding::Alias(identity) => CollationBinding::Alias(identity),
            Binding::Internal(column) => CollationBinding::Internal(*column),
        };
        schema = schema
            .with_collation_override(binding, oid)
            .map_err(|error| spill_error(format!("invalid spill collation: {error}")))?;
    }
    Ok(schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_sql::ast::ColumnType;

    #[test]
    fn malformed_collation_records_fail_before_extending_the_schema() {
        let schema = RowSchema::with_types(
            vec!["a".into(), "b".into()],
            vec![Some(ColumnType::Text); 2],
        );
        for words in [
            vec![3, 0, 0, 950, 0, 1, 951, 0, 0, 811],
            vec![2, 0, 0, 950, 0, 0, 951],
            vec![1, 0, 4, 950],
            vec![1, 9, 0, 950],
            vec![1, 0, 0, u64::MAX],
            vec![1, 2, 0, 123, 950],
        ] {
            let bytes = words
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>();
            assert!(
                decode(&mut BinaryReader::new(&bytes), schema.clone(), 2).is_err(),
                "{words:?}"
            );
        }
    }
}
