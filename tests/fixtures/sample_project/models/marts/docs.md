{% docs order_facts_doc %}
One row per order, enriched with the customer's country.

Grain: `order_id`.
{% enddocs %}

{% docs primary_key_doc %}Surrogate primary key (unique, not null).{% enddocs %}

{%- docs raw_orders_doc -%}
Orders exactly as exported by the operational app.
{%- enddocs -%}

{% docs finance_note_doc %}Sourced from order facts.{% enddocs %}
