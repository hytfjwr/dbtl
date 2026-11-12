-- Model-level generic tests declare their column through `column_name:` /
-- `arguments:` kwargs — never a column-level `node.column_name`. Both
-- spellings must normalize so `has_pk_guarantee` sees unique+not_null on id.
select 1 as id, 'note' as notes
