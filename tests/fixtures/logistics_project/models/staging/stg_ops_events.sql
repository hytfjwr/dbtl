-- Literal `source()` inside a zero-argument project macro — the call
-- carries no binding yet still lands `carrier_feed.ops_events` as the
-- parent edge in both modes.
{{ ops_events_feed() }}
