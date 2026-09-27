//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field movement preserves `DiskANN` canonical ownership and ordinary raw-vector backfill.

use super::{
    columns_json_references, string, Family, NativeRecordOwner, NativeSnapshot, RelationIdentity,
    Result, VersionError,
};

const FAMILIES: [Family; 5] = [
    Family::Vectors,
    Family::VectorOrigins,
    Family::VectorChanges,
    Family::VectorPopulations,
    Family::VectorPopulationWitnesses,
];

impl NativeSnapshot {
    pub(super) fn reject_diskann_field_merge(
        &self,
        owner: NativeRecordOwner,
        table: &str,
        from: &str,
        to: &str,
    ) -> Result<()> {
        if !self.has_vector_field_records(owner, to, &FAMILIES)? {
            return Ok(());
        }
        let mut diskann = self.has_vector_field_records(owner, from, &FAMILIES[1..])?
            || self.has_vector_field_records(owner, to, &FAMILIES[1..])?;
        self.visit_rows(
            Family::CatalogIndexes,
            Some(NativeRecordOwner::Database(self.database)),
            &[],
            |row| {
                if string(row[3])?.eq_ignore_ascii_case("diskann")
                    && RelationIdentity::new(string(row[4])?, string(row[5])?).qualified_name()
                        == table
                {
                    let columns = string(row[6])?;
                    diskann |= columns_json_references(&columns, from)?
                        || columns_json_references(&columns, to)?;
                }
                Ok(())
            },
        )?;
        if diskann {
            return Err(VersionError::InvalidEncoding(
                "column rename would merge DiskANN canonical fields",
            )
            .into());
        }
        Ok(())
    }

    fn has_vector_field_records(
        &self,
        owner: NativeRecordOwner,
        field: &str,
        families: &[Family],
    ) -> Result<bool> {
        for &family in families {
            let mut occupied = false;
            self.visit_field_keys(family, owner, 1, field, None, |_| {
                occupied = true;
                Ok(false)
            })?;
            if occupied {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
