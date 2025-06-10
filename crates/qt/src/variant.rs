//! Conversion from serde-serialisable Rust values to `QVariant`.
//!
//! Goes through `serde_json::Value`, which keeps the code tiny and is far
//! cheaper than anything a shell does per frame. Struct fields become
//! `QVariantMap` keys, sequences become `QVariantList`, `None` becomes an
//! invalid (undefined in QML) variant.

use cxx_qt_lib::{QList, QMap, QMapPair_QString_QVariant, QString, QVariant};
use serde::Serialize;
use serde_json::Value;

pub fn to_value<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|err| {
        tracing::error!("cannot serialise value for QML: {err}");
        Value::Null
    })
}

#[allow(dead_code)]
pub fn to_variant<T: Serialize + ?Sized>(value: &T) -> QVariant {
    value_to_variant(&to_value(value))
}

pub fn value_to_variant(value: &Value) -> QVariant {
    match value {
        Value::Null => QVariant::default(),
        Value::Bool(b) => QVariant::from(b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                QVariant::from(&i)
            } else if let Some(u) = n.as_u64() {
                QVariant::from(&u)
            } else {
                QVariant::from(&n.as_f64().unwrap_or_default())
            }
        }
        Value::String(s) => QVariant::from(&QString::from(s.as_str())),
        Value::Array(items) => {
            let mut list = QList::<QVariant>::default();
            list.reserve(items.len() as isize);
            for item in items {
                list.append(value_to_variant(item));
            }
            QVariant::from(&list)
        }
        Value::Object(fields) => {
            let mut map = QMap::<QMapPair_QString_QVariant>::default();
            for (key, item) in fields {
                map.insert(QString::from(key.as_str()), value_to_variant(item));
            }
            QVariant::from(&map)
        }
    }
}

/// `is_active` -> `isActive`, the convention QML users expect.
pub fn to_camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Best-effort conversion of a value coming back from QML into JSON.
pub fn variant_to_value(variant: &QVariant) -> Value {
    use cxx_qt_lib::{QList, QMap, QMapPair_QString_QVariant};
    // Objects and arrays from QML arrive wrapped; unwrap before matching.
    let flattened;
    let variant = if variant.type_name() == "QJSValue" {
        flattened = crate::ipc::qobject::flatten_js_value(variant);
        &flattened
    } else {
        variant
    };
    match variant.type_name() {
        "bool" => variant
            .value::<bool>()
            .map(Value::from)
            .unwrap_or(Value::Null),
        "int" => variant
            .value::<i32>()
            .map(Value::from)
            .unwrap_or(Value::Null),
        "uint" => variant
            .value::<u32>()
            .map(Value::from)
            .unwrap_or(Value::Null),
        "qlonglong" => variant
            .value::<i64>()
            .map(Value::from)
            .unwrap_or(Value::Null),
        "qulonglong" => variant
            .value::<u64>()
            .map(Value::from)
            .unwrap_or(Value::Null),
        // QML numbers are doubles; print whole ones as integers.
        "double" | "float" => match variant.value::<f64>() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Value::from(f as i64),
            Some(f) => Value::from(f),
            None => Value::Null,
        },
        "QString" => variant
            .value::<QString>()
            .map(|s| Value::from(s.to_string()))
            .unwrap_or(Value::Null),
        "QVariantList" => {
            let list = variant.value::<QList<QVariant>>().unwrap_or_default();
            Value::Array(list.iter().map(variant_to_value).collect())
        }
        "QVariantMap" => {
            let map = variant
                .value::<QMap<QMapPair_QString_QVariant>>()
                .unwrap_or_default();
            Value::Object(
                map.iter()
                    .map(|(k, v)| (k.to_string(), variant_to_value(v)))
                    .collect(),
            )
        }
        _ => variant
            .value::<QString>()
            .map(|s| Value::from(s.to_string()))
            .unwrap_or(Value::Null),
    }
}
