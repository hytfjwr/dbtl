-- Bound `source()` argument: the table name is decided at the call site
-- and bound into the macro body's `source('carrier_feed', table_name)`.
{{ carrier_feed('rate_cards') }}
