-- A provably-true branch's `enabled=false` parks the model — it lands
-- under `disabled` in the real manifest exactly like an unconditional
-- `config(enabled=false)`.
{% if true %}{{ config(enabled=false) }}{% endif %}
select * from {{ ref('stg_orders') }}
