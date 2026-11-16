def model(dbt, session):
    # The folder's `+enabled: var('enable_experimental', false)` gates it —
    # off under the default vars, on under the vars manifest.
    return dbt.ref("fct_shipments")
