//! The existing TraceBundle hashes use Python's JSON spelling, including spaces.
//! Keep that wire contract while moving the implementation to Rust.
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use time::OffsetDateTime;

pub(super) fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) fn fingerprint(value: &Value) -> String {
    digest(python_json(value, true).as_bytes())
}

pub(super) fn python_json(value: &Value, sorted: bool) -> String {
    match value {
        Value::Object(map) => {
            let mut pairs: Vec<_> = map.iter().collect();
            if sorted {
                pairs.sort_by_key(|(k, _)| *k);
            }
            format!(
                "{{{}}}",
                pairs
                    .into_iter()
                    .map(|(k, v)| format!(
                        "{}: {}",
                        serde_json::to_string(k).unwrap(),
                        python_json(v, sorted)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|v| python_json(v, sorted))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(n) if n.is_f64() => {
            let value = n.as_f64().unwrap();
            let raw = n.to_string();
            // Python uses scientific notation below 1e-4 and from 1e16.
            let scientific = value != 0.0 && (value.abs() < 1e-4 || value.abs() >= 1e16);
            if scientific {
                let raw = if raw.contains('e') {
                    raw
                } else {
                    format!("{value:e}")
                };
                let (mantissa, exponent) = raw.split_once('e').unwrap();
                let exponent: i32 = exponent.parse().unwrap();
                format!(
                    "{}e{exponent:+03}",
                    mantissa.strip_suffix(".0").unwrap_or(mantissa)
                )
            } else if raw.contains('e') {
                format!("{value}")
            } else {
                raw
            }
        }
        _ => value.to_string(),
    }
}

fn datetime(timestamp: f64) -> Result<String, String> {
    if !timestamp.is_finite() || !(-62_135_596_800.0..253_402_300_800.0).contains(&timestamp) {
        return Err("ValueError".into());
    }
    // Python rounds the fractional seconds before adding the whole seconds;
    // multiplying an epoch timestamp first can lose a microsecond.
    let micros = timestamp.trunc() as i128 * 1_000_000
        + (timestamp.fract() * 1_000_000.0).round_ties_even() as i128;
    let dt = OffsetDateTime::from_unix_timestamp_nanos(micros * 1000).map_err(|_| "ValueError")?;
    if !(1..=9999).contains(&dt.year()) {
        return Err("ValueError".into());
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}+00:00",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        if dt.microsecond() == 0 {
            String::new()
        } else {
            format!(".{:06}", dt.microsecond())
        }
    ))
}

pub(super) fn utc(value: &Value) -> Result<Value, String> {
    let timestamp = if value.is_object() {
        value.get("at").ok_or("KeyError")?
    } else {
        value
    };
    if timestamp.as_i64() == Some(i64::MAX) {
        return Ok(Value::Null);
    }
    let timestamp = timestamp
        .as_f64()
        .or_else(|| timestamp.as_bool().map(|value| u8::from(value) as f64))
        .ok_or("TypeError")?;
    datetime(timestamp).map(Value::String)
}

pub(super) fn now() -> String {
    datetime(OffsetDateTime::now_utc().unix_timestamp_nanos() as f64 / 1e9).unwrap()
}
pub(super) fn file_time(time: std::time::SystemTime) -> Result<Value, String> {
    let seconds = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    };
    datetime(seconds).map(Value::String)
}

static KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)author[iİı]zat[iİı]on|cook[iİı]e|ap[iİı][-_]?key|access[-_]?token|secret|password",
    )
    .unwrap()
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)Bearer[\s\x1c-\x1f]+[^\s\x1c-\x1f\"'<>]+"#).unwrap());
// Python re uses letters/numbers/underscore for \w (not combining marks or
// join controls), four extra ASCII whitespace controls, and İ/ı in (?i)i.
static SK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\A|[^\p{L}\p{N}_])sk-[A-Za-z0-9_-]{11,}(?:[A-Za-z0-9_](\z|[^\p{L}\p{N}_])|-([\p{L}\p{N}_]))").unwrap()
});
static ARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\A|[^\p{L}\p{N}_])ark-[A-Za-z0-9_-]+").unwrap());
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)((?:ap[iİı][_-]?key|author[iİı]zat[iİı]on|cook[iİı]e|password|secret|token)[\s\x1c-\x1f]*[=:][\s\x1c-\x1f]*)[^\s\x1c-\x1f,;]+",
    )
    .unwrap()
});

fn redact_sk(text: &str) -> String {
    // Both word→nonword and hyphen→word endings satisfy Python's \b.
    // Captured boundaries are retained. A second pass finds adjacent tokens
    // sharing the one delimiter consumed by the first pass (no look-around).
    let first = SK.replace_all(text, "${1}[REDACTED]${2}${3}");
    SK.replace_all(&first, "${1}[REDACTED]${2}${3}")
        .into_owned()
}

pub(super) fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if KEY.is_match(k) {
                            Value::String("[REDACTED]".into())
                        } else {
                            redact(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact).collect()),
        Value::String(s) => Value::String(redact_sk(&BEARER.replace_all(s, "Bearer [REDACTED]"))),
        _ => value.clone(),
    }
}
pub(super) fn safe(value: &Value) -> Value {
    match value {
        Value::Object(_) => {
            let Value::Object(map) = redact(value) else {
                unreachable!()
            };
            Value::Object(map.into_iter().map(|(k, v)| (k, safe(&v))).collect())
        }
        Value::Array(values) => Value::Array(values.iter().map(safe).collect()),
        Value::String(s) => {
            if let Ok(parsed @ (Value::Object(_) | Value::Array(_))) =
                serde_json::from_str::<Value>(s)
            {
                return Value::String(python_json(&safe(&parsed), false));
            }
            let redacted = redact(value);
            let s = ARK.replace_all(redacted.as_str().unwrap(), "${1}[REDACTED]");
            redact(&Value::String(
                ASSIGN.replace_all(&s, "${1}[REDACTED]").into_owned(),
            ))
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn python_unicode_redaction() {
        for key in ["APİ_KEY", "apı_key", "paſſword", "cooKie"] {
            assert_eq!(redact(&json!({key:"sensitive"}))[key], "[REDACTED]");
        }
        assert_eq!(
            redact(&json!("Bearer\u{1c}sensitive")),
            json!("Bearer [REDACTED]")
        );
        assert_eq!(
            safe(&json!("APİ_KEY\u{1f}= sensitive")),
            json!("APİ_KEY\u{1f}= [REDACTED]")
        );
        assert_eq!(
            redact(&json!("e\u{301}sk-1234567890abcdef")),
            json!("e\u{301}[REDACTED]")
        );
        assert_eq!(
            redact(&json!("²sk-1234567890abcdef")),
            json!("²sk-1234567890abcdef")
        );
        assert_eq!(
            safe(&json!("\u{200c}ark-sensitive")),
            json!("\u{200c}[REDACTED]")
        );
        assert_eq!(
            redact(&json!("sk-abcdefghijkl sk-mnopqrstuvwx sk-yzabcdefghij")),
            json!("[REDACTED] [REDACTED] [REDACTED]")
        );
        assert_eq!(
            redact(&json!("xsk-aaaaaaaaaaaa-sk-bbbbbbbbbbbb")),
            json!("xsk-aaaaaaaaaaaa-[REDACTED]")
        );
        assert_eq!(
            redact(&json!("sk-aaaaaaaaaaaa-b中")),
            json!("[REDACTED]b中")
        );
        assert_eq!(redact(&json!("sk-aaaaaaaaaaa-中")), json!("[REDACTED]中"));
        assert_eq!(redact(&json!("sk-aaaaaaaaaaa-²")), json!("[REDACTED]²"));
        assert_eq!(
            redact(&json!("sk-aaaaaaaaaaa- ")),
            json!("sk-aaaaaaaaaaa- ")
        );
    }

    #[test]
    fn python_wire_spelling() {
        assert_eq!(
            python_json(&json!({"中文":[1,1.0,1e-6,1e16,-0.0]}), true),
            "{\"中文\": [1, 1.0, 1e-06, 1e+16, -0.0]}"
        );
        assert_eq!(
            utc(&json!({"at":1789917000,"source":"Explicit"})).unwrap(),
            "2026-09-20T15:10:00+00:00"
        );
        assert_eq!(utc(&json!({"at":i64::MAX})).unwrap(), Value::Null);
        assert!(utc(&json!(1e308)).is_err());
        assert!(utc(&json!(-1e308)).is_err());
        assert_eq!(
            utc(&json!(1789917000.9827855)).unwrap(),
            "2026-09-20T15:10:00.982785+00:00"
        );
        assert_eq!(
            utc(&json!(-0.0000015)).unwrap(),
            "1969-12-31T23:59:59.999998+00:00"
        );
        assert_eq!(utc(&json!(true)).unwrap(), "1970-01-01T00:00:01+00:00");
        assert_eq!(
            safe(&json!("{\"api_key\":\"private\"}")),
            json!("{\"api_key\": \"[REDACTED]\"}")
        );
    }
}
