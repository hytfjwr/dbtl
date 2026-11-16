def model(dbt, session):
    # Enabled through the dbt_project.yml tree — the same `+enabled`
    # expression that gates `sbx_probe` — so the schema `config:`
    # materialization (`view`) is what beats the python `table` default.
    staged = dbt.ref("stg_shipments")
    return staged.limit(5)
