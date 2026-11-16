{% snapshot shipment_status_snap %}
{{ config(unique_key='shipment_id', strategy='check', check_cols='all', tags=['snap_in']) }}

select
    shipment_id,
    status
from {{ ref('fct_shipments') }}

{% endsnapshot %}
