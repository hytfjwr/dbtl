-- Bound `ref()` argument: the model name is decided at the call site and
-- bound into the macro body's `ref(model_name)` — `stg_rate_cards` gains
-- a real consumer, not a lint-visible orphan.
{{ staging_model('stg_rate_cards') }}
