-- Consumed ONLY by a package model (audit_tools.audit_rollups) in the manifest:
-- a no-downstream model on the source side / root scope, non-orphan under
-- --include-packages.
select * from {{ source('raw', 'orders') }}
