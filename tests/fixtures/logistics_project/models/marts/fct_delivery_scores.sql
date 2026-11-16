-- The SQL consumer of `fct_py_delivery_risk`: a ref() naming a python
-- model resolves exactly like a `.sql` parent (source mode used to miss
-- the `.py` node entirely — the regression this fixture pins).

select
    r.shipment_id,
    r.risk_band
from {{ ref('fct_py_delivery_risk') }} r
