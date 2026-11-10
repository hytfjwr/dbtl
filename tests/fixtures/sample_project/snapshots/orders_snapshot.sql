{% snapshot orders_snapshot %}
{{
    config(
      target_schema='snapshots',
      unique_key='id',
      strategy='check',
      check_cols='all',
      meta={'owner': 'snap-file'}
    )
}}

select * from {{ ref('stg_orders') }}

{% endsnapshot %}
