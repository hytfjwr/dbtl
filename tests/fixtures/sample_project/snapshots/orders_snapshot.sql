{% snapshot orders_snapshot %}
{{
    config(
      target_schema='snapshots',
      unique_key='id',
      strategy='check',
      check_cols='all'
    )
}}

select * from {{ ref('stg_orders') }}

{% endsnapshot %}
