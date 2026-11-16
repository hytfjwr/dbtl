"""Sensor pings scored by a python model.

The staging folder's `+materialized: view` must NOT reach it: a python
model defaults to `table` and the project tree cannot override that
(verified against real dbt — only a schema `config:` or `dbt.config()`
may).
"""


def _pings(dbt):
    # A `dbt.ref` inside a helper is a real dependency — dbt's AST pass
    # walks every scope, not just `model`.
    return dbt.ref("stg_sensor_pings")


def model(dbt, session):
    # `dbt.config` accepts a positional dict as well as kwargs — this one
    # deliberately uses the dict form (and carries no `materialized` key,
    # so the python `table` default stands).
    dbt.config(
        {
            "meta": {"owner": "sensor-eng", "pipeline": "scores"},
        }
    )
    pings = _pings(dbt)
    # An f-string still names a dependency: dbt's AST sees the Call node
    # inside the interpolation.
    _ = f"rates join: {dbt.ref('stg_rate_cards')}"
    return pings
