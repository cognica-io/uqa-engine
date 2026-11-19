//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every granted privilege has an owner-rooted grant-option path.

use super::{invalid, AuthorityCatalog, RoleIdentity, SQLError, Work};
use uqa_core::memory::{BudgetedMap, BudgetedVec};

#[derive(Clone, Copy)]
pub(super) struct Grant {
    pub grantee: Option<RoleIdentity>,
    pub grantor: RoleIdentity,
    pub privileges: u16,
    pub options: u16,
}

struct Link {
    grantee: RoleIdentity,
    options: u16,
    next: Option<usize>,
}

impl AuthorityCatalog<'_> {
    pub(super) async fn grants(
        &self,
        owner: RoleIdentity,
        entries: impl Iterator<Item = Grant> + Clone,
        inherited: impl Iterator<Item = Grant>,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        self.role(owner)?;
        let mut paths = BudgetedMap::new(self.memory);
        for grant in entries.clone() {
            self.checkpoint(work).await?;
            self.role(grant.grantor)?;
            if let Some(grantee) = grant.grantee {
                self.role(grantee)?;
            }
            if grant.privileges == 0
                || grant.options & !grant.privileges != 0
                || grant.grantee.is_none() && grant.options != 0
                || paths.insert((grant.grantee, grant.grantor), ())?.is_some()
            {
                return Err(invalid("invalid or duplicate ACL grant path"));
            }
        }
        let control = self.control();
        crate::catalog::work::retire_entries(paths.into_entries(), &control, work).await?;
        let mut heads = BudgetedMap::new(self.memory);
        let mut links = BudgetedVec::new(self.memory);
        for grant in entries.clone().chain(inherited) {
            self.checkpoint(work).await?;
            if let Some(grantee) = grant.grantee.filter(|_| grant.options != 0) {
                let index = links.len();
                links.push(Link {
                    grantee,
                    options: grant.options,
                    next: heads.get(&grant.grantor).copied(),
                })?;
                heads.insert(grant.grantor, index)?;
            }
        }
        let mut reached = BudgetedMap::new(self.memory);
        reached.insert(owner, u16::MAX)?;
        let mut pending = BudgetedVec::new(self.memory);
        pending.push(owner)?;
        while let Some(grantor) = pending.pop() {
            self.checkpoint(work).await?;
            let options = *reached.get(&grantor).expect("queued grantor");
            let mut next = heads.get(&grantor).copied();
            while let Some(index) = next {
                self.checkpoint(work).await?;
                let link = &links[index];
                let before = reached.get(&link.grantee).copied().unwrap_or(0);
                let after = before | (options & link.options);
                if before != after {
                    reached.insert(link.grantee, after)?;
                    pending.push(link.grantee)?;
                }
                next = link.next;
            }
        }
        for grant in entries {
            self.checkpoint(work).await?;
            if grant.privileges & !reached.get(&grant.grantor).copied().unwrap_or(0) != 0 {
                return Err(invalid("ACL grantor has no owner-rooted grant option"));
            }
        }
        crate::catalog::work::retire_entries(reached.into_entries(), &control, work).await?;
        crate::catalog::work::retire_entries(heads.into_entries(), &control, work).await?;
        Ok(())
    }
}
