//! `Store` singleton: small persistent key-value state for the configuration.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[namespace = "blacksheep"]
        /// Whether a database is open; without one, get() returns the default.
        #[qproperty(bool, available, READ, CONSTANT)]
        type Store = super::StoreRust;

        /// The value stored under `key`, or `fallback` when there is none.
        /// Values are JSON: booleans, numbers, strings, arrays and objects.
        #[qinvokable]
        fn get(self: &Store, key: &QString, fallback: &QVariant) -> QVariant;
        /// Store a value under `key`; it survives reloads and restarts.
        #[qinvokable]
        fn set(self: &Store, key: &QString, value: &QVariant);
        #[qinvokable]
        fn remove(self: &Store, key: &QString);
    }
}

use cxx_qt_lib::{QString, QVariant};

use crate::variant::{value_to_variant, variant_to_value};

pub struct StoreRust {
    available: bool,
}

impl Default for StoreRust {
    fn default() -> Self {
        Self {
            available: blacksheep_store::global().is_some(),
        }
    }
}

impl qobject::Store {
    fn get(&self, key: &QString, fallback: &QVariant) -> QVariant {
        let Some(store) = blacksheep_store::global() else {
            return fallback.clone();
        };
        let key = key.to_string();
        match store.with(|c| blacksheep_store::kv::get(c, &key)) {
            Ok(Some(value)) => value_to_variant(&value),
            Ok(None) => fallback.clone(),
            Err(err) => {
                tracing::warn!("store: get {key:?}: {err}");
                fallback.clone()
            }
        }
    }

    fn set(&self, key: &QString, value: &QVariant) {
        let Some(store) = blacksheep_store::global() else {
            return;
        };
        let key = key.to_string();
        let value = variant_to_value(value);
        if let Err(err) = store.with(|c| blacksheep_store::kv::set(c, &key, &value)) {
            tracing::warn!("store: set {key:?}: {err}");
        }
    }

    fn remove(&self, key: &QString) {
        let Some(store) = blacksheep_store::global() else {
            return;
        };
        let key = key.to_string();
        if let Err(err) = store.with(|c| blacksheep_store::kv::remove(c, &key)) {
            tracing::warn!("store: remove {key:?}: {err}");
        }
    }
}
