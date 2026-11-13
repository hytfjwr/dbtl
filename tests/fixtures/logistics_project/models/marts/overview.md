{% docs carrier_perf_doc %}
Per-carrier shipment rollup. `weight_band` and `utilization_pct` come from
project macros; the optional `empty_leg_share` column is gated by the
`perf_track_empty_legs` var and only appears when it is enabled.
{% enddocs %}
