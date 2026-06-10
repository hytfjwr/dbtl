-- staging model for orders
{# a Jinja-commented ref is invisible to dbt and must be ignored: {{ ref('ghost_model') }} #}
{{ config(materialized='view') }}

select *
from {{ source('raw', 'orders') }}
