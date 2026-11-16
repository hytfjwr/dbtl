"""Late-delivery risk scored off the shipment fact — a python model.

`fct_delivery_scores` refs this model from SQL; the cross-language ref
must resolve exactly like a `.sql` parent's.
"""

import math


def _shipments(dbt):
    # A multiline `dbt.ref` with a trailing comment — the AST sees the
    # same Call as the one-line spelling.
    return dbt.ref(
        "fct_shipments"  # the shipment fact
    )


def model(dbt, session):
    # In-file config kwargs: the highest-precedence channel — `meta` and
    # the incremental materialization here beat schema.yml and the tree.
    dbt.config(
        materialized="incremental",
        unique_key="shipment_id",
        incremental_strategy="delete+insert",
        meta={"owner": "ml-eng", "sla": {"freshness_hours": 6}},
    )
    shipments = _shipments(dbt)
    lanes = dbt.source("carrier_feed", "rate_cards")
    _ = math.floor(len(f"{shipments}"))
    return shipments.join(lanes, "carrier_id")
