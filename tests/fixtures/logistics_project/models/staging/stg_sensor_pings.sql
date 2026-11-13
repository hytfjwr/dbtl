-- Nested macros: `feeder_chain` forwards its bound `table_name` into
-- `carrier_feed`, so the `sensor_pings` edge comes through two hops.
{{ feeder_chain('sensor_pings') }}
