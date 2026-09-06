-- Stateful PostgreSQL 18.4 CHECK inheritance reference.
-- Statements run in order in an isolated schema on both engines.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case create_column_parent ok
CREATE TABLE parent(a integer CONSTRAINT positive CHECK(a>0));
-- @end

-- @case create_inherited ok
CREATE TABLE inherited() INHERITS(parent);
-- @end

-- @case create_local_column ok
CREATE TABLE local_column(a integer CONSTRAINT positive CHECK(local_column.a>0)) INHERITS(parent);
-- @end

-- @case create_local_table ok
CREATE TABLE local_table(a integer,CONSTRAINT positive CHECK(a>0)) INHERITS(parent);
-- @end

-- @case creation_origins rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('parent'::regclass,'inherited'::regclass,'local_column'::regclass,'local_table'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case reject_local_expression_conflict error
CREATE TABLE bad(a integer,CONSTRAINT positive CHECK(a>=0)) INHERITS(parent);
-- @end

-- @case reject_local_no_inherit error
CREATE TABLE bad(a integer CONSTRAINT positive CHECK(a>0) NO INHERIT) INHERITS(parent);
-- @end

-- @case reject_local_not_enforced error
CREATE TABLE bad(a integer CONSTRAINT positive CHECK(a>0) NOT ENFORCED) INHERITS(parent);
-- @end

-- @case enforce_inherited error
INSERT INTO inherited VALUES(-1);
-- @end

-- @case save_identity_table ok
CREATE TABLE identities(relname text, id oid);
-- @end

-- @case save_identity ok
INSERT INTO identities SELECT r.relname,c.oid FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('parent'::regclass,'inherited'::regclass,'local_column'::regclass,'local_table'::regclass);
-- @end

-- @case direct_inherited_drop error
ALTER TABLE inherited DROP CONSTRAINT positive;
-- @end

-- @case direct_local_inherited_drop error
ALTER TABLE local_column DROP CONSTRAINT positive;
-- @end

-- @case direct_inherited_rename error
ALTER TABLE inherited RENAME CONSTRAINT positive TO renamed;
-- @end

-- @case only_parent_rename error
ALTER TABLE ONLY parent RENAME CONSTRAINT positive TO renamed;
-- @end

-- @case parent_recursive_rename ok
ALTER TABLE parent RENAME CONSTRAINT positive TO renamed;
-- @end

-- @case renamed_origins rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('parent'::regclass,'inherited'::regclass,'local_column'::regclass,'local_table'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case renamed_identities rows
SELECT r.relname,c.oid=i.id AS same_identity FROM identities i JOIN pg_class r ON r.relname=i.relname JOIN pg_constraint c ON c.conrelid=r.oid WHERE c.contype='c' ORDER BY r.relname;
-- @end

-- @case localize_inherited ok
ALTER TABLE inherited ADD CONSTRAINT renamed CHECK(a>0);
-- @end

-- @case localized_origin rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('inherited'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case duplicate_local_add error
ALTER TABLE inherited ADD CONSTRAINT renamed CHECK(a>0);
-- @end

-- @case drop_parent_preserving_locals ok
ALTER TABLE parent DROP CONSTRAINT renamed;
-- @end

-- @case locals_after_parent_drop rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('parent'::regclass,'inherited'::regclass,'local_column'::regclass,'local_table'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case left_parent ok
CREATE TABLE left_parent(a integer,CONSTRAINT shared CHECK(a>0));
-- @end

-- @case right_parent ok
CREATE TABLE right_parent(a integer CONSTRAINT shared CHECK(a>0));
-- @end

-- @case multiple_parents ok
CREATE TABLE shared_child() INHERITS(left_parent,right_parent);
-- @end

-- @case multiple_parent_count rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('shared_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case rename_one_of_two_parents error
ALTER TABLE left_parent RENAME CONSTRAINT shared TO renamed;
-- @end

-- @case failed_rename_is_atomic rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('left_parent'::regclass,'right_parent'::regclass,'shared_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case drop_first_parent_constraint ok
ALTER TABLE left_parent DROP CONSTRAINT shared;
-- @end

-- @case one_parent_remains rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('shared_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case drop_last_parent_constraint ok
ALTER TABLE right_parent DROP CONSTRAINT shared;
-- @end

-- @case inherited_constraint_removed rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('shared_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case only_parent ok
CREATE TABLE only_parent(a integer CONSTRAINT positive CHECK(a>0));
-- @end

-- @case only_child ok
CREATE TABLE only_child() INHERITS(only_parent);
-- @end

-- @case only_drop ok
ALTER TABLE ONLY only_parent DROP CONSTRAINT positive;
-- @end

-- @case only_drop_localizes_child rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('only_parent'::regclass,'only_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case invalid_parent ok
CREATE TABLE invalid_parent(a integer);
-- @end

-- @case invalid_child ok
CREATE TABLE invalid_child() INHERITS(invalid_parent);
-- @end

-- @case invalid_row ok
INSERT INTO invalid_child VALUES(-1);
-- @end

-- @case unvalidated_child ok
ALTER TABLE invalid_child ADD CONSTRAINT positive CHECK(a>0) NOT VALID;
-- @end

-- @case validated_recursive_merge_conflict error
ALTER TABLE invalid_parent ADD CONSTRAINT positive CHECK(a>0);
-- @end

-- @case validation_conflict_rollback rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('invalid_parent'::regclass,'invalid_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case unvalidated_recursive_merge ok
ALTER TABLE invalid_parent ADD CONSTRAINT positive CHECK(a>0) NOT VALID;
-- @end

-- @case new_child_from_unvalidated ok
CREATE TABLE born_valid() INHERITS(invalid_parent);
-- @end

-- @case new_empty_child_is_valid rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('invalid_parent'::regclass,'invalid_child'::regclass,'born_valid'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case failed_recursive_validation error
ALTER TABLE invalid_parent VALIDATE CONSTRAINT positive;
-- @end

-- @case failed_validation_rollback rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('invalid_parent'::regclass,'invalid_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case repair_invalid_row ok
UPDATE invalid_child SET a=1;
-- @end

-- @case only_validation_requires_children error
ALTER TABLE ONLY invalid_parent VALIDATE CONSTRAINT positive;
-- @end

-- @case recursive_validation ok
ALTER TABLE invalid_parent VALIDATE CONSTRAINT positive;
-- @end

-- @case recursive_validated_flags rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('invalid_parent'::regclass,'invalid_child'::regclass,'born_valid'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case nv_parent ok
CREATE TABLE nv_parent(a integer);
-- @end

-- @case nv_child ok
CREATE TABLE nv_child() INHERITS(nv_parent);
-- @end

-- @case nv_legacy_row ok
INSERT INTO nv_child VALUES(-1);
-- @end

-- @case nv_add_parent ok
ALTER TABLE nv_parent ADD CONSTRAINT positive CHECK(a>0) NOT VALID;
-- @end

-- @case nv_local_validated_conflict error
ALTER TABLE nv_child ADD CONSTRAINT positive CHECK(a>0);
-- @end

-- @case nv_localize ok
ALTER TABLE nv_child ADD CONSTRAINT positive CHECK(a>0) NOT VALID;
-- @end

-- @case nv_localized_flags rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('nv_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case not_enforced_parent ok
CREATE TABLE weak_parent(a integer,CONSTRAINT positive CHECK(a>0) NOT ENFORCED);
-- @end

-- @case not_enforced_child ok
CREATE TABLE weak_child() INHERITS(weak_parent);
-- @end

-- @case enforced_local_create ok
CREATE TABLE strong_child(a integer CONSTRAINT positive CHECK(a>0)) INHERITS(weak_parent);
-- @end

-- @case directional_enforcement rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('weak_parent'::regclass,'weak_child'::regclass,'strong_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case strengthen_local_constraint ok
ALTER TABLE weak_child ADD CONSTRAINT positive CHECK(a>0);
-- @end

-- @case strengthened_local_flags rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('weak_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case plain_child ok
CREATE TABLE attached(a integer,CONSTRAINT positive CHECK(a>0));
-- @end

-- @case ordinary_attach ok
ALTER TABLE attached INHERIT weak_parent;
-- @end

-- @case ordinary_attach_keeps_local rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('attached'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case ordinary_remove_edge ok
ALTER TABLE attached NO INHERIT weak_parent;
-- @end

-- @case removed_edge_origin rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('attached'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case ordinary_readd_edge ok
ALTER TABLE attached INHERIT weak_parent;
-- @end

-- @case readded_edge_origin rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('attached'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case partition_parent ok
CREATE TABLE partition_parent(a integer,CONSTRAINT positive CHECK(a>0)) PARTITION BY RANGE(a);
-- @end

-- @case born_partition ok
CREATE TABLE born_partition PARTITION OF partition_parent FOR VALUES FROM(0) TO(10);
-- @end

-- @case standalone_partition ok
CREATE TABLE attached_partition(a integer,CONSTRAINT positive CHECK(a>0));
-- @end

-- @case attach_partition ok
ALTER TABLE partition_parent ATTACH PARTITION attached_partition FOR VALUES FROM(10) TO(20);
-- @end

-- @case partition_origins rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('partition_parent'::regclass,'born_partition'::regclass,'attached_partition'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case partition_duplicate_local error
ALTER TABLE born_partition ADD CONSTRAINT positive CHECK(a>0);
-- @end

-- @case detach_partition ok
ALTER TABLE partition_parent DETACH PARTITION attached_partition;
-- @end

-- @case detached_partition_origin rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('attached_partition'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case partition_rename ok
ALTER TABLE partition_parent RENAME CONSTRAINT positive TO renamed;
-- @end

-- @case partition_renamed_origin rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('partition_parent'::regclass,'born_partition'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case partition_drop ok
ALTER TABLE partition_parent DROP CONSTRAINT renamed;
-- @end

-- @case partition_dropped rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('partition_parent'::regclass,'born_partition'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case column_no_inherit_parent ok
CREATE TABLE local_only(a integer CONSTRAINT positive CHECK(a>0) NO INHERIT);
-- @end

-- @case no_inherit_child ok
CREATE TABLE local_only_child() INHERITS(local_only);
-- @end

-- @case no_inherit_insert ok
INSERT INTO local_only_child VALUES(-1);
-- @end

-- @case no_inherit_absent rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('local_only'::regclass,'local_only_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case diamond_root ok
CREATE TABLE root(a integer CONSTRAINT positive CHECK(a>0));
-- @end

-- @case diamond_left ok
CREATE TABLE branch_left() INHERITS(root);
-- @end

-- @case diamond_right ok
CREATE TABLE branch_right() INHERITS(root);
-- @end

-- @case diamond_leaf ok
CREATE TABLE leaf() INHERITS(branch_left,branch_right);
-- @end

-- @case diamond_rename ok
ALTER TABLE root RENAME CONSTRAINT positive TO renamed;
-- @end

-- @case diamond_rename_origins rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('root'::regclass,'branch_left'::regclass,'branch_right'::regclass,'leaf'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case diamond_drop ok
ALTER TABLE root DROP CONSTRAINT renamed;
-- @end

-- @case diamond_dropped rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('root'::regclass,'branch_left'::regclass,'branch_right'::regclass,'leaf'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case owner_parent_role ok
CREATE ROLE __UQA_ROLE_PARENT__;
-- @end

-- @case owner_child_role ok
CREATE ROLE __UQA_ROLE_MEMBER__;
-- @end

-- @case owner_schema_usage ok
GRANT USAGE ON SCHEMA __UQA_STATEFUL_SCHEMA__ TO __UQA_ROLE_PARENT__,__UQA_ROLE_MEMBER__;
-- @end

-- @case owned_parent ok
CREATE TABLE owned_parent(a integer CONSTRAINT positive CHECK(a>0));
-- @end

-- @case owned_child ok
CREATE TABLE owned_child() INHERITS(owned_parent);
-- @end

-- @case change_parent_owner ok
ALTER TABLE owned_parent OWNER TO __UQA_ROLE_PARENT__;
-- @end

-- @case change_child_owner ok
ALTER TABLE owned_child OWNER TO __UQA_ROLE_MEMBER__;
-- @end

-- @case rename_other_owner_child error
SET ROLE __UQA_ROLE_PARENT__;
ALTER TABLE owned_parent RENAME CONSTRAINT positive TO renamed;
-- @end

-- @case drop_other_owner_child error
SET ROLE __UQA_ROLE_PARENT__;
ALTER TABLE owned_parent DROP CONSTRAINT positive;
-- @end

-- @case owner_failures_are_atomic rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('owned_parent'::regclass,'owned_child'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case column_add_parent ok
CREATE TABLE add_parent(a integer);
-- @end

-- @case column_add_child ok
CREATE TABLE add_child(b integer CONSTRAINT child_positive CHECK(b>0)) INHERITS(add_parent);
-- @end

-- @case column_add_grandchild ok
CREATE TABLE add_grandchild() INHERITS(add_child);
-- @end

-- @case recursive_column_check ok
ALTER TABLE add_parent ADD COLUMN b integer CONSTRAINT parent_positive CHECK(b>=0);
-- @end

-- @case column_checks_have_independent_origins rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('add_parent'::regclass,'add_child'::regclass,'add_grandchild'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case add_no_inherit_column_check ok
ALTER TABLE add_parent ADD COLUMN c integer CONSTRAINT local_positive CHECK(c>0) NO INHERIT;
-- @end

-- @case added_no_inherit_check_boundary rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('add_parent'::regclass,'add_child'::regclass,'add_grandchild'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case no_inherit_child_value ok
INSERT INTO add_child VALUES(1,1,-1);
-- @end

-- @case equivalent_cast_parent ok
CREATE TABLE cast_parent(a integer CONSTRAINT positive CHECK(a>0));
-- @end

-- @case equivalent_cast_child ok
CREATE TABLE cast_child(a integer CONSTRAINT positive CHECK(a::integer>0)) INHERITS(cast_parent);
-- @end

-- @case equivalent_literal_child ok
CREATE TABLE literal_child(a integer CONSTRAINT positive CHECK(a>'0'::integer)) INHERITS(cast_parent);
-- @end

-- @case repeated_local_name error
CREATE TABLE duplicate_local(a integer CONSTRAINT positive CHECK(a>0),CONSTRAINT positive CHECK(a>0)) INHERITS(cast_parent);
-- @end

-- @case partition_local_parent ok
CREATE TABLE explicit_partition_parent(a integer CONSTRAINT positive CHECK(a>0)) PARTITION BY RANGE(a);
-- @end

-- @case partition_local_definition ok
CREATE TABLE explicit_partition PARTITION OF explicit_partition_parent(CONSTRAINT positive CHECK(a>0)) FOR VALUES FROM(0) TO(10);
-- @end

-- @case explicit_partition_origin rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('explicit_partition'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case only_partition_constraint_drop ok
ALTER TABLE ONLY explicit_partition_parent DROP CONSTRAINT positive;
-- @end

-- @case only_partition_drop_origin rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('explicit_partition_parent'::regclass,'explicit_partition'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case attach_invalid_child ok
CREATE TABLE attach_invalid(a integer);
-- @end

-- @case attach_invalid_check ok
ALTER TABLE attach_invalid ADD CONSTRAINT positive CHECK(a>0) NOT VALID;
-- @end

-- @case attach_validation_conflict error
ALTER TABLE attach_invalid INHERIT cast_parent;
-- @end

-- @case mixed_enforcement_parent ok
CREATE TABLE weak_second(a integer CONSTRAINT positive CHECK(a>0) NOT ENFORCED);
-- @end

-- @case mixed_enforcement_child ok
CREATE TABLE mixed_enforcement() INHERITS(cast_parent,weak_second);
-- @end

-- @case mixed_enforcement_merge rows
SELECT r.relname,c.conname,c.conislocal,c.coninhcount,c.conenforced,c.convalidated,c.connoinherit FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid WHERE c.contype='c' AND c.conrelid IN('mixed_enforcement'::regclass) ORDER BY r.relname,c.conname;
-- @end

-- @case separate_constraint_identities rows
SELECT count(*)=count(DISTINCT oid) AS distinct_identities FROM pg_constraint WHERE contype='c' AND conrelid IN('cast_parent'::regclass,'cast_child'::regclass,'mixed_enforcement'::regclass);
-- @end

-- @case qualified_add_parent ok
CREATE TABLE qualified_parent(a integer);
-- @end

-- @case qualified_add_child ok
CREATE TABLE qualified_child() INHERITS(qualified_parent);
-- @end

-- @case qualified_recursive_check error
ALTER TABLE qualified_parent ADD CONSTRAINT positive CHECK(qualified_parent.a>0);
-- @end

-- @case qualified_child_enforcement ok
INSERT INTO qualified_child VALUES(-1);
-- @end

-- @case qualified_child_origin rows
SELECT conname,conislocal,coninhcount FROM pg_constraint WHERE conrelid='qualified_child'::regclass AND contype='c';
-- @end

-- @case owned_parent_inheritance_ancestor ok
CREATE TABLE owned_ancestor(a integer CONSTRAINT positive CHECK(a>0));
-- @end

-- @case inherited_owned_parent ok
ALTER TABLE owned_parent INHERIT owned_ancestor;
-- @end

-- @case rename_child_owner_precedes_root_inheritance error
SET ROLE __UQA_ROLE_PARENT__;
ALTER TABLE owned_parent RENAME CONSTRAINT positive TO renamed;
-- @end
