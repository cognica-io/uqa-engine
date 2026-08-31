-- Stateful PostgreSQL 18.4 role-membership parity fixture.
-- The runner replaces schema and cluster-global role placeholders with unique names.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case create_parent ok
CREATE ROLE __UQA_ROLE_PARENT__;
-- @end

-- @case create_member ok
CREATE ROLE __UQA_ROLE_MEMBER__ INHERIT;
-- @end

-- @case create_noinherit ok
CREATE ROLE __UQA_ROLE_NOINHERIT__ NOINHERIT;
-- @end

-- @case create_admin ok
CREATE ROLE __UQA_ROLE_ADMIN__;
-- @end

-- @case create_delegate ok
CREATE ROLE __UQA_ROLE_DELEGATE__;
-- @end

-- @case create_initial_member ok
CREATE ROLE __UQA_ROLE_INITIAL_MEMBER__;
-- @end

-- @case create_initial_admin ok
CREATE ROLE __UQA_ROLE_INITIAL_ADMIN__;
-- @end

-- @case create_alter_member ok
CREATE ROLE __UQA_ROLE_ALTER_MEMBER__;
-- @end

-- @case create_rollback_member ok
CREATE ROLE __UQA_ROLE_ROLLBACK_MEMBER__;
-- @end

-- @case create_droppable ok
CREATE ROLE __UQA_ROLE_DROPPABLE__;
-- @end

-- @case create_middle ok
CREATE ROLE __UQA_ROLE_MIDDLE__;
-- @end

-- @case create_leaf ok
CREATE ROLE __UQA_ROLE_LEAF__;
-- @end

-- @case create_limited_creator ok
CREATE ROLE __UQA_ROLE_LIMITED_CREATOR__ CREATEROLE;
-- @end

-- @case create_full_creator ok
CREATE ROLE __UQA_ROLE_FULL_CREATOR__ CREATEROLE CREATEDB REPLICATION BYPASSRLS;
-- @end

-- A CREATEROLE user receives ADMIN on roles it creates, but may delegate only the global attributes it already holds.
-- @case limited_creator_creates_managed_role ok
SET ROLE __UQA_ROLE_LIMITED_CREATOR__; CREATE ROLE __UQA_ROLE_MANAGED__ CREATEROLE; RESET ROLE;
-- @end

-- @case creator_receives_managed_admin rows
SELECT membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_MANAGED__' AND member.rolname = '__UQA_ROLE_LIMITED_CREATOR__';
-- @end

-- @case limited_creator_cannot_create_createdb error
SET ROLE __UQA_ROLE_LIMITED_CREATOR__; CREATE ROLE __UQA_ROLE_FORBIDDEN__ CREATEDB;
-- @end

-- @case limited_creator_cannot_create_replication error
SET ROLE __UQA_ROLE_LIMITED_CREATOR__; CREATE ROLE __UQA_ROLE_FORBIDDEN__ REPLICATION;
-- @end

-- @case limited_creator_cannot_create_bypassrls error
SET ROLE __UQA_ROLE_LIMITED_CREATOR__; CREATE ROLE __UQA_ROLE_FORBIDDEN__ BYPASSRLS;
-- @end

-- @case limited_creator_cannot_alter_createdb error
SET ROLE __UQA_ROLE_LIMITED_CREATOR__; ALTER ROLE __UQA_ROLE_MANAGED__ CREATEDB;
-- @end

-- @case limited_creator_cannot_alter_replication error
SET ROLE __UQA_ROLE_LIMITED_CREATOR__; ALTER ROLE __UQA_ROLE_MANAGED__ REPLICATION;
-- @end

-- @case limited_creator_cannot_alter_bypassrls error
SET ROLE __UQA_ROLE_LIMITED_CREATOR__; ALTER ROLE __UQA_ROLE_MANAGED__ BYPASSRLS;
-- @end

-- @case full_creator_delegates_held_attributes ok
SET ROLE __UQA_ROLE_FULL_CREATOR__; CREATE ROLE __UQA_ROLE_FULL_CHILD__ CREATEROLE CREATEDB REPLICATION BYPASSRLS; RESET ROLE;
-- @end

-- @case full_creator_attributes_are_visible rows
SELECT rolcreaterole, rolcreatedb, rolreplication, rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = '__UQA_ROLE_FULL_CHILD__';
-- @end

-- Defaults take INHERIT from the grantee role and enable SET.
-- @case grant_default_memberships ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_MEMBER__, __UQA_ROLE_NOINHERIT__;
-- @end

-- @case default_membership_options rows
SELECT member.rolinherit, membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname IN ('__UQA_ROLE_MEMBER__', '__UQA_ROLE_NOINHERIT__') ORDER BY member.rolinherit DESC;
-- @end

-- A leaf inherits routine privileges and can assume a role through every SET-enabled edge in a chain.
-- @case grant_parent_to_middle ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_MIDDLE__;
-- @end

-- @case grant_middle_to_leaf ok
GRANT __UQA_ROLE_MIDDLE__ TO __UQA_ROLE_LEAF__;
-- @end

-- @case transitive_membership_options rows
SELECT granted.rolname = '__UQA_ROLE_PARENT__' AS parent_edge, membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE (granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_MIDDLE__') OR (granted.rolname = '__UQA_ROLE_MIDDLE__' AND member.rolname = '__UQA_ROLE_LEAF__') ORDER BY parent_edge DESC;
-- @end

-- @case create_transitive_acl_probe ok
CREATE FUNCTION public.__UQA_STATEFUL_SCHEMA__() RETURNS integer LANGUAGE SQL AS 'SELECT 18';
-- @end

-- @case restrict_transitive_acl_probe ok
REVOKE ALL ON FUNCTION public.__UQA_STATEFUL_SCHEMA__() FROM PUBLIC;
-- @end

-- @case grant_transitive_acl_probe ok
GRANT EXECUTE ON FUNCTION public.__UQA_STATEFUL_SCHEMA__() TO __UQA_ROLE_PARENT__;
-- @end

-- @case transitive_role_acl_is_inherited ok
SET ROLE __UQA_ROLE_LEAF__; DO $$ BEGIN PERFORM public.__UQA_STATEFUL_SCHEMA__(); END $$; RESET ROLE;
-- @end

-- Routine ownership follows inherited role privileges, while a SET-only membership does not confer owner privileges until the role is assumed.
-- @case transfer_probe_to_role_owner ok
ALTER FUNCTION public.__UQA_STATEFUL_SCHEMA__() OWNER TO __UQA_ROLE_PARENT__;
-- @end

-- @case inherited_owner_manages_probe ok
SET ROLE __UQA_ROLE_LEAF__; ALTER FUNCTION public.__UQA_STATEFUL_SCHEMA__() IMMUTABLE; GRANT EXECUTE ON FUNCTION public.__UQA_STATEFUL_SCHEMA__() TO __UQA_ROLE_INITIAL_MEMBER__; RESET ROLE;
-- @end

-- @case inherited_owner_changes_are_visible rows
SELECT public.__UQA_STATEFUL_SCHEMA__() AS result, procedure.provolatile, owner.rolname = '__UQA_ROLE_PARENT__' AS owner_matches FROM pg_catalog.pg_proc AS procedure JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = procedure.pronamespace JOIN pg_catalog.pg_roles AS owner ON owner.oid = procedure.proowner WHERE namespace.nspname = 'public' AND procedure.proname = '__UQA_STATEFUL_SCHEMA__';
-- @end

-- @case granted_execute_role_invokes_probe ok
SET ROLE __UQA_ROLE_INITIAL_MEMBER__; DO $$ BEGIN PERFORM public.__UQA_STATEFUL_SCHEMA__(); END $$; RESET ROLE;
-- @end

-- @case noinherit_member_cannot_alter_owned_probe error
SET ROLE __UQA_ROLE_NOINHERIT__; ALTER FUNCTION public.__UQA_STATEFUL_SCHEMA__() STABLE;
-- @end

-- @case noinherit_member_cannot_replace_owned_probe error
SET ROLE __UQA_ROLE_NOINHERIT__; CREATE OR REPLACE FUNCTION public.__UQA_STATEFUL_SCHEMA__() RETURNS integer LANGUAGE SQL AS 'SELECT 20';
-- @end

-- @case noinherit_member_cannot_drop_owned_probe error
SET ROLE __UQA_ROLE_NOINHERIT__; DROP FUNCTION public.__UQA_STATEFUL_SCHEMA__();
-- @end

-- A successful SECURITY INVOKER body may change the session role, but the same operation is forbidden anywhere inside a SECURITY DEFINER call.
-- @case create_invoker_set_role_probe ok
CREATE FUNCTION public.__UQA_STATEFUL_SCHEMA__(boolean) RETURNS text LANGUAGE plpgsql SECURITY INVOKER AS $$ BEGIN EXECUTE 'SET ROLE __UQA_ROLE_MEMBER__'; RETURN current_user; END $$;
-- @end

-- @case invoker_set_role_survives_return ok
DO $$ BEGIN PERFORM public.__UQA_STATEFUL_SCHEMA__(true); IF current_user <> '__UQA_ROLE_MEMBER__' THEN RAISE EXCEPTION 'SET ROLE did not survive SECURITY INVOKER return'; END IF; END $$; RESET ROLE;
-- @end

-- @case create_definer_set_role_probe ok
CREATE FUNCTION public.__UQA_STATEFUL_SCHEMA__(text) RETURNS text LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN EXECUTE 'SET ROLE __UQA_ROLE_MEMBER__'; RETURN current_user; END $$;
-- @end

-- @case transfer_definer_set_role_probe ok
ALTER FUNCTION public.__UQA_STATEFUL_SCHEMA__(text) OWNER TO __UQA_ROLE_PARENT__;
-- @end

-- @case definer_set_role_is_rejected error
SELECT public.__UQA_STATEFUL_SCHEMA__('blocked');
-- @end

-- @case grant_explicit_admin_options ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_ADMIN__ WITH ADMIN OPTION, INHERIT FALSE, SET FALSE;
-- @end

-- @case explicit_admin_options rows
SELECT membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_ADMIN__';
-- @end

-- @case delegated_grant ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_DELEGATE__ GRANTED BY __UQA_ROLE_ADMIN__;
-- @end

-- @case delegated_grantor rows
SELECT grantor.rolname = '__UQA_ROLE_ADMIN__' AS delegated, membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member JOIN pg_catalog.pg_roles AS grantor ON grantor.oid = membership.grantor WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_DELEGATE__';
-- @end

-- @case membership_cycle_rejected error
GRANT __UQA_ROLE_MEMBER__ TO __UQA_ROLE_PARENT__;
-- @end

-- @case self_membership_rejected error
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_PARENT__;
-- @end

-- ADMIN revocation follows the grants made by that member.
-- @case dependent_admin_revoke_restrict error
REVOKE ADMIN OPTION FOR __UQA_ROLE_PARENT__ FROM __UQA_ROLE_ADMIN__ RESTRICT;
-- @end

-- @case dependent_admin_revoke_cascade ok
REVOKE ADMIN OPTION FOR __UQA_ROLE_PARENT__ FROM __UQA_ROLE_ADMIN__ CASCADE;
-- @end

-- @case cascade_removes_delegated_grant rows
SELECT membership.admin_option, (SELECT count(*) FROM pg_catalog.pg_auth_members AS delegated JOIN pg_catalog.pg_roles AS delegated_member ON delegated_member.oid = delegated.member WHERE delegated.roleid = membership.roleid AND delegated_member.rolname = '__UQA_ROLE_DELEGATE__') AS delegated_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_ADMIN__';
-- @end

-- One role/member pair may have independent grants from multiple grantors.
-- @case restore_admin_option ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_ADMIN__ WITH ADMIN TRUE;
-- @end

-- @case delegated_grant_again ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_DELEGATE__ GRANTED BY __UQA_ROLE_ADMIN__;
-- @end

-- @case direct_grant_same_pair ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_DELEGATE__;
-- @end

-- @case multiple_grantors rows
SELECT count(*) AS grant_count, count(*) FILTER (WHERE grantor.rolname = '__UQA_ROLE_ADMIN__') AS delegated_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member JOIN pg_catalog.pg_roles AS grantor ON grantor.oid = membership.grantor WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_DELEGATE__';
-- @end

-- @case revoke_one_grantor ok
REVOKE __UQA_ROLE_PARENT__ FROM __UQA_ROLE_DELEGATE__ GRANTED BY __UQA_ROLE_ADMIN__;
-- @end

-- @case one_grantor_remains rows
SELECT count(*) AS grant_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_DELEGATE__';
-- @end

-- Re-grant changes only explicitly named options; option-only REVOKE retains the row.
-- @case regrant_inherit_and_set ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_NOINHERIT__ WITH INHERIT TRUE, SET FALSE;
-- @end

-- @case regrant_named_options rows
SELECT membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_NOINHERIT__';
-- @end

-- @case regrant_set_only ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_NOINHERIT__ WITH SET TRUE;
-- @end

-- @case unspecified_option_is_retained rows
SELECT membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_NOINHERIT__';
-- @end

-- @case revoke_inherit_option ok
REVOKE INHERIT OPTION FOR __UQA_ROLE_PARENT__ FROM __UQA_ROLE_NOINHERIT__;
-- @end

-- @case revoke_set_option ok
REVOKE SET OPTION FOR __UQA_ROLE_PARENT__ FROM __UQA_ROLE_NOINHERIT__;
-- @end

-- @case option_revoke_retains_membership rows
SELECT count(*) AS grant_count, bool_or(membership.inherit_option) AS any_inherit, bool_or(membership.set_option) AS any_set FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_NOINHERIT__';
-- @end

-- @case revoke_complete_membership ok
REVOKE __UQA_ROLE_PARENT__ FROM __UQA_ROLE_NOINHERIT__;
-- @end

-- @case complete_revoke_removes_row rows
SELECT count(*) AS grant_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_NOINHERIT__';
-- @end

-- CREATE ROLE membership clauses publish all edges atomically.
-- @case create_role_with_memberships ok
CREATE ROLE __UQA_ROLE_CREATED__ NOINHERIT IN ROLE __UQA_ROLE_PARENT__ ROLE __UQA_ROLE_INITIAL_MEMBER__ ADMIN __UQA_ROLE_INITIAL_ADMIN__;
-- @end

-- @case create_role_membership_options rows
SELECT CASE WHEN granted.rolname = '__UQA_ROLE_PARENT__' THEN 'in_role' WHEN member.rolname = '__UQA_ROLE_INITIAL_ADMIN__' THEN 'admin' ELSE 'role_member' END AS edge, membership.admin_option, membership.inherit_option, membership.set_option FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE (granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_CREATED__') OR (granted.rolname = '__UQA_ROLE_CREATED__' AND member.rolname IN ('__UQA_ROLE_INITIAL_MEMBER__', '__UQA_ROLE_INITIAL_ADMIN__')) ORDER BY edge;
-- @end

-- @case alter_group_add_member ok
ALTER GROUP __UQA_ROLE_PARENT__ ADD USER __UQA_ROLE_ALTER_MEMBER__;
-- @end

-- @case alter_group_added rows
SELECT count(*) AS grant_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_ALTER_MEMBER__';
-- @end

-- @case alter_group_drop_member ok
ALTER GROUP __UQA_ROLE_PARENT__ DROP USER __UQA_ROLE_ALTER_MEMBER__;
-- @end

-- @case alter_group_removed rows
SELECT count(*) AS grant_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_ALTER_MEMBER__';
-- @end

-- @case membership_transaction_rollback ok
BEGIN; GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_ROLLBACK_MEMBER__; ROLLBACK;
-- @end

-- @case rolled_back_membership_absent rows
SELECT count(*) AS grant_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_ROLLBACK_MEMBER__';
-- @end

-- Dropping a member removes its memberships; dropping a grantor with surviving grants is blocked.
-- @case grant_droppable_member ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_DROPPABLE__;
-- @end

-- @case drop_member_cascades_memberships ok
DROP ROLE __UQA_ROLE_DROPPABLE__;
-- @end

-- @case no_dangling_member_oid rows
SELECT count(*) AS dangling_count FROM pg_catalog.pg_auth_members AS membership LEFT JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE member.oid IS NULL;
-- @end

-- @case delegated_grant_before_grantor_drop ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_DELEGATE__ GRANTED BY __UQA_ROLE_ADMIN__;
-- @end

-- GRANT with ADMIN FALSE updates the edge without implicitly revoking grants previously made by that administrator.
-- @case clear_admin_without_cascade ok
GRANT __UQA_ROLE_PARENT__ TO __UQA_ROLE_ADMIN__ WITH ADMIN FALSE;
-- @end

-- @case cleared_admin_keeps_delegated_grant rows
SELECT membership.admin_option, (SELECT count(*) FROM pg_catalog.pg_auth_members AS delegated JOIN pg_catalog.pg_roles AS delegated_member ON delegated_member.oid = delegated.member JOIN pg_catalog.pg_roles AS delegated_grantor ON delegated_grantor.oid = delegated.grantor WHERE delegated.roleid = membership.roleid AND delegated_member.rolname = '__UQA_ROLE_DELEGATE__' AND delegated_grantor.rolname = '__UQA_ROLE_ADMIN__') AS delegated_count FROM pg_catalog.pg_auth_members AS membership JOIN pg_catalog.pg_roles AS granted ON granted.oid = membership.roleid JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member WHERE granted.rolname = '__UQA_ROLE_PARENT__' AND member.rolname = '__UQA_ROLE_ADMIN__';
-- @end

-- @case grantor_drop_dependency error
DROP ROLE __UQA_ROLE_ADMIN__;
-- @end

-- @case revoke_grantor_dependency ok
REVOKE __UQA_ROLE_PARENT__ FROM __UQA_ROLE_DELEGATE__ GRANTED BY __UQA_ROLE_ADMIN__;
-- @end

-- @case drop_grantor_after_revoke ok
DROP ROLE __UQA_ROLE_ADMIN__;
-- @end

-- @case no_dangling_grantor_oid rows
SELECT count(*) AS dangling_count FROM pg_catalog.pg_auth_members AS membership LEFT JOIN pg_catalog.pg_roles AS grantor ON grantor.oid = membership.grantor WHERE grantor.oid IS NULL;
-- @end

-- @case inherited_owner_drops_probe ok
SET ROLE __UQA_ROLE_LEAF__; DROP FUNCTION public.__UQA_STATEFUL_SCHEMA__(); RESET ROLE;
-- @end


-- @case drop_role_context_probes ok
DROP FUNCTION public.__UQA_STATEFUL_SCHEMA__(boolean), public.__UQA_STATEFUL_SCHEMA__(text);
-- @end
