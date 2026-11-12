-- Governance through the middle rung: the schema.yml top-level `access:`
-- property supplies the private access; its `config:` supplies the group
-- membership and the enforced contract. The untyped `status` column is the
-- `contract_columns_without_type` case.
select 1 as id, 'x' as status
