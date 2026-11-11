use super::*;

// ---- YAML → serde_json::Value, with REAL dbt's scalar semantics ----
//
// Every dbt YAML file (dbt_project.yml, schema.yml) is loaded by PyYAML,
// whose YAML-1.1 implicit resolvers make a PLAIN `yes`/`no`/`on`/`off` a
// bool, `017` octal (15), `1:30` sexagesimal (90), `1_000` a thousand —
// while a QUOTED spelling stays a string and `0o17`/`08`/`y`/`n`/`1e5` are
// strings either way (all verified against real dbt 1.12.6). The serde_yaml
// family of `Value` types (serde_norway included) erases the plain-vs-quoted
// scalar style before callers can see it, so the parse below works off
// yaml-rust2's event stream — `Event::Scalar` carries `TScalarStyle` — and
// applies PyYAML's resolution rules itself. Anchors, aliases, `<<` merge
// keys, explicit `!!` tags, and duplicate keys (last wins) all resolve like
// PyYAML. Known residual gaps: YAML timestamps keep their text (a `date`
// meta value is vanishingly rare) and non-finite floats become `null` (JSON
// has no representation).

/// The `(event, source mark)` pairs of one YAML stream — collected so the
/// tree build can recurse freely (marks ride along for error messages).
pub(super) struct EventSink(Vec<(Event, Marker)>);
impl MarkedEventReceiver for EventSink {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        self.0.push((ev, mark));
    }
}

/// Parse a single-document YAML stream into a [`serde_json::Value`] (the wire
/// type `RawNode.meta` / `RawSource.meta` / tree fields already carry).
/// Malformed YAML and multi-document streams are errors — like PyYAML's
/// `safe_load`, which dbt's own files must satisfy.
pub(super) fn yaml_to_json(
    text: &str,
    warnings: &mut Vec<String>,
) -> Result<serde_json::Value, String> {
    let mut sink = EventSink(Vec::new());
    yaml_rust2::parser::Parser::new(text.chars())
        .load(&mut sink, false)
        .map_err(|e| e.to_string())?;
    let mut b = YamlBuilder {
        evs: &sink.0,
        pos: 0,
        anchors: HashMap::new(),
        warnings,
    };
    // Walk to the first content event of the first document.
    let doc = loop {
        match b.evs.get(b.pos).map(|(e, _)| e) {
            None | Some(Event::StreamEnd) => return Ok(serde_json::Value::Null),
            Some(Event::StreamStart | Event::DocumentStart | Event::Nothing) => b.pos += 1,
            Some(Event::DocumentEnd) => return Ok(serde_json::Value::Null),
            Some(_) => break b.node(),
        }
    };
    // Anything past DocumentEnd is a second document — a hard error, like
    // `yaml.safe_load` ("expected a single document").
    for (ev, _) in &b.evs[b.pos..] {
        if matches!(ev, Event::DocumentStart) {
            return Err("expected a single document in the stream".to_string());
        }
    }
    Ok(doc)
}

/// Fold the collected event stream into [`serde_json::Value`]s. Anchored
/// nodes register by anchor id as they complete (a self-referencing alias —
/// which PyYAML resolves recursively — lands `null` + warning instead).
pub(super) struct YamlBuilder<'a> {
    pub(super) evs: &'a [(Event, Marker)],
    pub(super) pos: usize,
    pub(super) anchors: HashMap<usize, serde_json::Value>,
    pub(super) warnings: &'a mut Vec<String>,
}

impl YamlBuilder<'_> {
    pub(super) fn node(&mut self) -> serde_json::Value {
        let (ev, _) = &self.evs[self.pos];
        match ev {
            Event::Scalar(v, style, aid, tag) => {
                self.pos += 1;
                let out = yaml_scalar_to_json(v, *style, tag.as_ref());
                self.anchor(*aid, &out);
                out
            }
            Event::SequenceStart(aid, _tag) => {
                self.pos += 1;
                let mut seq = Vec::new();
                while !matches!(
                    self.evs.get(self.pos).map(|(e, _)| e),
                    None | Some(Event::SequenceEnd)
                ) {
                    seq.push(self.node());
                }
                self.pos += 1;
                let out: serde_json::Value = seq.into();
                self.anchor(*aid, &out);
                out
            }
            Event::MappingStart(aid, _tag) => {
                self.pos += 1;
                let mut map = serde_json::Map::new();
                while !matches!(
                    self.evs.get(self.pos).map(|(e, _)| e),
                    None | Some(Event::MappingEnd)
                ) {
                    // `<<` merge key: entries merge UNDER the explicit keys
                    // (earlier merge sources win over later ones — the YAML
                    // merge spec PyYAML implements).
                    let is_merge = matches!(
                        &self.evs.get(self.pos).map(|(e, _)| e),
                        Some(Event::Scalar(k, TScalarStyle::Plain, _, tag))
                            if k == "<<" && tag.is_none()
                    );
                    if is_merge {
                        self.pos += 1;
                        let v = self.node();
                        merge_into(&mut map, v, self.warnings);
                        continue;
                    }
                    let key = match self.node() {
                        serde_json::Value::String(s) => s,
                        // A `yes:`/`5:`/`~:` key resolves like the value —
                        // PyYAML's dict key serializes as "true"/"5"/"null".
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Null => "null".to_string(),
                        other => other.to_string(), // seq/map key: compact JSON
                    };
                    let v = self.node();
                    if map.insert(key.clone(), v).is_some() {
                        self.warnings.push(format!(
                            "duplicate mapping key '{key}' — the last value wins"
                        ));
                    }
                }
                self.pos += 1;
                let out: serde_json::Value = map.into();
                self.anchor(*aid, &out);
                out
            }
            Event::Alias(id) => {
                self.pos += 1;
                match self.anchors.get(id) {
                    Some(v) => v.clone(),
                    None => {
                        self.warnings
                            .push("dangling YAML alias — resolved as null".to_string());
                        serde_json::Value::Null
                    }
                }
            }
            // DocumentEnd/StreamEnd/Nothing mid-stream: treat as absent.
            _ => {
                self.pos += 1;
                serde_json::Value::Null
            }
        }
    }

    pub(super) fn anchor(&mut self, aid: usize, v: &serde_json::Value) {
        if aid > 0 {
            self.anchors.insert(aid, v.clone());
        }
    }
}

/// Merge a `<<` key's value into `map`: a mapping contributes its entries
/// (existing — explicit — keys always win); a sequence merges each mapping
/// member in order, earlier sources winning (the YAML merge spec).
pub(super) fn merge_into(
    map: &mut serde_json::Map<String, serde_json::Value>,
    v: serde_json::Value,
    warnings: &mut Vec<String>,
) {
    let merge_map = |map: &mut serde_json::Map<String, serde_json::Value>,
                     src: serde_json::Map<String, serde_json::Value>| {
        for (k, val) in src {
            map.entry(k).or_insert(val);
        }
    };
    match v {
        serde_json::Value::Object(src) => merge_map(map, src),
        serde_json::Value::Array(srcs) => {
            for src in srcs {
                match src {
                    serde_json::Value::Object(src) => merge_map(map, src),
                    _ => warnings
                        .push("ignoring a non-mapping entry in a `<<` merge sequence".to_string()),
                }
            }
        }
        serde_json::Value::Null => {} // `<<:` with no value — nothing to merge
        _ => warnings.push("ignoring a non-mapping `<<` merge value".to_string()),
    }
}

/// The [`serde_json::Value`] one YAML scalar holds under PyYAML. `style` is
/// the detail serde `Value`s erased: only a PLAIN scalar resolves by content
/// (a quoted `'yes'`/`"017"` or block literal is always a string). Explicit
/// `!!`-tags use PyYAML's tagged constructors; unknown local tags (`!foo`)
/// unwrap to their content, the serde_yaml `!Tagged` behaviour.
pub(super) fn yaml_scalar_to_json(
    v: &str,
    style: TScalarStyle,
    tag: Option<&Tag>,
) -> serde_json::Value {
    if let Some(t) = tag {
        if t.handle == "tag:yaml.org,2002:" {
            match t.suffix.as_str() {
                "str" => return v.into(),
                "null" => return serde_json::Value::Null,
                "bool" => return pyyaml_tagged_bool(v).map_or_else(|| v.into(), Into::into),
                "int" => return pyyaml_int(v).map_or_else(|| v.into(), Into::into),
                "float" => {
                    return pyyaml_float(v).map_or(serde_json::Value::Null, |f| {
                        serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into)
                    });
                }
                _ => {}
            }
        }
    }
    if style != TScalarStyle::Plain {
        return v.into();
    }
    match v {
        // PyYAML's null spellings, incl. the empty plain scalar (`key:`).
        "" | "~" | "null" | "Null" | "NULL" => return serde_json::Value::Null,
        _ => {}
    }
    if let Some(b) = yaml11_bool(v) {
        return b.into();
    }
    if let Some(i) = pyyaml_int(v) {
        return i.into();
    }
    if let Some(f) = pyyaml_float(v) {
        return serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into);
    }
    v.into()
}

/// The bool a PLAIN scalar holds under PyYAML's implicit YAML-1.1 resolver,
/// or `None` when it's a string. `y`/`n` are NOT bools even under PyYAML
/// (verified against real dbt: `y_key: y` lands in the manifest as the
/// string `"y"`); `true`/`false` spellings are here too so every bool lands
/// by one rule.
pub(super) fn yaml11_bool(s: &str) -> Option<bool> {
    match s {
        "yes" | "Yes" | "YES" | "on" | "On" | "ON" | "true" | "True" | "TRUE" => Some(true),
        "no" | "No" | "NO" | "off" | "Off" | "OFF" | "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// The bool an EXPLICIT `!!bool`-tagged scalar holds under PyYAML — the
/// tagged constructor's `bool_values` table is broader than the implicit
/// resolver and DOES accept `y`/`n`.
pub(super) fn pyyaml_tagged_bool(s: &str) -> Option<bool> {
    match s {
        "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            Some(true)
        }
        "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            Some(false)
        }
        _ => None,
    }
}

/// An integer under PyYAML's YAML-1.1 implicit resolver: binary (`0b101`),
/// octal (`017` → 15 — `0o17`/`08` stay strings!), decimal with `_`
/// separators (`1_000`), hex (`0x1F`), and sexagesimal (`1:30` → 90).
/// Overflow beyond `i64` degrades to "not an int" (a string) rather than a
/// wrong number.
pub(super) fn pyyaml_int(v: &str) -> Option<i64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^([-+]?)(?:0b([0-1_]+)|0([0-7_]+)|(0|[1-9][0-9_]*)|0x([0-9a-fA-F_]+)|([1-9][0-9_]*(?::[0-5]?[0-9])+))$",
        )
        .unwrap()
    });
    let m = re.captures(v)?;
    let neg = m.get(1).is_some_and(|s| s.as_str() == "-");
    let val: i64 = if let Some(d) = m.get(2) {
        i64::from_str_radix(&d.as_str().replace('_', ""), 2).ok()?
    } else if let Some(d) = m.get(3) {
        i64::from_str_radix(&format!("0{}", d.as_str().replace('_', "")), 8).ok()?
    } else if let Some(d) = m.get(4) {
        d.as_str().replace('_', "").parse::<i64>().ok()?
    } else if let Some(d) = m.get(5) {
        i64::from_str_radix(&d.as_str().replace('_', ""), 16).ok()?
    } else {
        let d = m.get(6)?;
        let mut acc: i64 = 0;
        for part in d.as_str().replace('_', "").split(':') {
            acc = acc
                .checked_mul(60)?
                .checked_add(part.parse::<i64>().ok()?)?;
        }
        acc
    };
    Some(if neg { -val } else { val })
}

/// A float under PyYAML's YAML-1.1 implicit resolver: `1.5`, `5.`, `.5`,
/// `1.0e+5` (a sign-LESS exponent — `1e5`, `1.0e5` — stays a string!),
/// sexagesimal floats (`1:20.5` → 80.5), and `.inf`/`.nan` spellings.
pub(super) fn pyyaml_float(v: &str) -> Option<f64> {
    match v {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return Some(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => return Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"^[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?$|^\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?$|^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*$",
        )
        .unwrap()
    });
    if !re.is_match(v) {
        return None;
    }
    let v = v.replace('_', "");
    if v.contains(':') {
        // Sexagesimal float: fold int segments base-60, add the fraction.
        let (neg, v) = match v.strip_prefix('-') {
            Some(r) => (-1.0, r.to_string()),
            None => (1.0, v.strip_prefix('+').unwrap_or(&v).to_string()),
        };
        let mut acc = 0.0f64;
        for part in v.split(':') {
            acc = acc * 60.0 + part.parse::<f64>().ok()?;
        }
        Some(neg * acc)
    } else {
        v.parse::<f64>().ok()
    }
}

// ---- serde glue: the string coercions serde_norway used to apply ----
//
// The structs below deserialize via `serde_json::from_value`, which is
// STRICT where serde_norway coerced scalars (`description: 5` → `"5"`).
// The `de_*` helpers reproduce that coercion per field so a stray numeric
// description can't fail the whole file.

/// `5` → `"5"`, `true` → `"true"`, a string → itself; non-scalars → `None`.
pub(super) fn scalar_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(super) fn de_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<String, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    scalar_to_string(&v).ok_or_else(|| serde::de::Error::custom("expected a string scalar"))
}

pub(super) fn de_opt_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => scalar_to_string(&v)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected a string scalar")),
    }
}

pub(super) fn de_string_vec<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<String>, D::Error> {
    // `tags:`/`depends_on:` left empty (`key:` → null) is an empty list, not
    // a type error.
    let v = Option::<Vec<serde_json::Value>>::deserialize(d)?.unwrap_or_default();
    v.iter()
        .map(scalar_to_string)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| serde::de::Error::custom("expected a list of string scalars"))
}

pub(super) fn de_opt_string_vec<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Vec<String>>, D::Error> {
    let v = Option::<Vec<serde_json::Value>>::deserialize(d)?;
    v.map(|elems| {
        elems
            .iter()
            .map(scalar_to_string)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| serde::de::Error::custom("expected a list of string scalars"))
    })
    .transpose()
}

/// The three-state `Option<Option<T>>` recipe: paired with
/// `#[serde(default)]` it distinguishes ABSENT (`None`) from an explicit
/// `null` (`Some(None)`) — the difference between "inherits the higher
/// level's value" and "clears it" that dbt's `freshness:`/`loaded_at_field:`
/// precedence requires.
pub(super) fn de_opt_some<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// The string-scalar flavour of [`de_opt_some`]: absent → `None` (inherit),
/// explicit `null` → `Some(None)` (clear), a scalar → `Some(Some(s))` with
/// the same numeric/bool coercion `de_opt_string` applies.
pub(super) fn de_opt_opt_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Null => Ok(Some(None)),
        other => scalar_to_string(&other)
            .map(|s| Some(Some(s)))
            .ok_or_else(|| serde::de::Error::custom("expected a string scalar")),
    }
}
