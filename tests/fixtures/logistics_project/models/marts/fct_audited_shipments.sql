{#- The whole SELECT below comes from the shipkit package macro: the
    generated columns and CTE only exist in compiled SQL. -#}
{{ config(materialized='view') }}

{{ shipkit.audited_select(ref('stg_shipments')) }}
