//! A generic `QAbstractListModel` fed from Rust.
//!
//! Roles are declared once per model with [`ListModel::configure`]; rows are
//! any `Serialize` value whose fields match the role names. Services call
//! [`ListModel::sync`] with the complete new list and the model works out the
//! minimal inserts, removes, moves and updates so QML views animate properly.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++Qt" {
        include!(<QtCore/QAbstractListModel>);
        #[qobject]
        type QAbstractListModel;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
        include!("cxx-qt-lib/qmap.h");
        type QMap_QString_QVariant = cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>;
        include!("cxx-qt-lib/qlist.h");
        type QList_QVariant = cxx_qt_lib::QList<cxx_qt_lib::QVariant>;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_uncreatable]
        #[base = QAbstractListModel]
        #[namespace = "blacksheep"]
        #[qproperty(i32, count, READ = count, NOTIFY = count_changed)]
        /// Every row as a JS object. Convenient for filter/map in QML; use
        /// the model itself for views so they get granular updates.
        #[qproperty(QList_QVariant, values, READ = values, NOTIFY = values_changed)]
        type ListModel = super::ListModelRust;

        #[qsignal]
        #[cxx_name = "countChanged"]
        fn count_changed(self: Pin<&mut ListModel>);

        #[qsignal]
        #[cxx_name = "valuesChanged"]
        fn values_changed(self: Pin<&mut ListModel>);

        fn count(self: &ListModel) -> i32;

        fn values(self: &ListModel) -> QList_QVariant;

        /// All roles of one row as a JS object, for use outside delegates.
        #[qinvokable]
        fn get(self: &ListModel, row: i32) -> QMap_QString_QVariant;

        /// Row index of the item whose key role equals `key`, or -1.
        #[qinvokable]
        #[cxx_name = "indexOf"]
        fn index_of(self: &ListModel, key: &QVariant) -> i32;

        #[cxx_override]
        #[rust_name = "row_count"]
        fn rowCount(self: &ListModel, parent: &QModelIndex) -> i32;

        #[cxx_override]
        fn data(self: &ListModel, index: &QModelIndex, role: i32) -> QVariant;

        #[cxx_override]
        #[rust_name = "role_names"]
        fn roleNames(self: &ListModel) -> QHash_i32_QByteArray;

        #[inherit]
        #[qsignal]
        #[rust_name = "data_changed"]
        fn dataChanged(
            self: Pin<&mut ListModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QVector_i32,
        );

        #[inherit]
        fn index(self: &ListModel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;

        #[inherit]
        #[rust_name = "begin_reset_model"]
        fn beginResetModel(self: Pin<&mut ListModel>);
        #[inherit]
        #[rust_name = "end_reset_model"]
        fn endResetModel(self: Pin<&mut ListModel>);
        #[inherit]
        #[rust_name = "begin_insert_rows"]
        fn beginInsertRows(self: Pin<&mut ListModel>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[rust_name = "end_insert_rows"]
        fn endInsertRows(self: Pin<&mut ListModel>);
        #[inherit]
        #[rust_name = "begin_remove_rows"]
        fn beginRemoveRows(self: Pin<&mut ListModel>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[rust_name = "end_remove_rows"]
        fn endRemoveRows(self: Pin<&mut ListModel>);
        #[inherit]
        #[rust_name = "begin_move_rows"]
        fn beginMoveRows(
            self: Pin<&mut ListModel>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            destination_parent: &QModelIndex,
            destination_child: i32,
        ) -> bool;
        #[inherit]
        #[rust_name = "end_move_rows"]
        fn endMoveRows(self: Pin<&mut ListModel>);
    }
}

use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QModelIndex, QString, QVariant};
use qobject::{QHash_i32_QByteArray, QList_QVariant, QMap_QString_QVariant, QVector_i32};
use serde::Serialize;
use serde_json::Value;

use crate::variant::{to_camel, to_value, value_to_variant};

/// Qt::UserRole. Roles handed to views are `USER_ROLE + 1 + index`.
const USER_ROLE: i32 = 0x0100;

struct Role {
    /// Field name on the Rust side (`is_active`).
    field: String,
    /// Name QML sees (`isActive`).
    name: String,
}

struct Row {
    key: Value,
    values: Vec<Value>,
    variants: Vec<QVariant>,
}

#[derive(Default)]
pub struct ListModelRust {
    roles: Vec<Role>,
    key_role: Option<usize>,
    rows: Vec<Row>,
}

impl ListModelRust {
    fn make_row(&self, item: Value) -> Row {
        let empty = serde_json::Map::new();
        let fields = item.as_object().unwrap_or(&empty);
        let values: Vec<Value> = self
            .roles
            .iter()
            .map(|role| fields.get(&role.field).cloned().unwrap_or(Value::Null))
            .collect();
        let key = self
            .key_role
            .and_then(|i| values.get(i).cloned())
            .unwrap_or(Value::Null);
        let variants = values.iter().map(value_to_variant).collect();
        Row {
            key,
            values,
            variants,
        }
    }
}

impl qobject::ListModel {
    /// Declare the roles (Rust field names) and which one identifies a row.
    /// Must run before the model is handed to any view.
    pub fn configure(mut self: Pin<&mut Self>, fields: &[&str], key: Option<&str>) {
        let mut rust = self.as_mut().rust_mut();
        rust.roles = fields
            .iter()
            .map(|field| Role {
                field: field.to_string(),
                name: to_camel(field),
            })
            .collect();
        rust.key_role = key.and_then(|key| fields.iter().position(|f| *f == key));
    }

    /// Replace every row without diffing. Views lose their state.
    pub fn reset<T: Serialize>(mut self: Pin<&mut Self>, items: impl IntoIterator<Item = T>) {
        let rows: Vec<Row> = {
            let rust = self.rust();
            items
                .into_iter()
                .map(|item| rust.make_row(to_value(&item)))
                .collect()
        };
        let before = self.as_ref().rust().rows.len();
        self.as_mut().begin_reset_model();
        self.as_mut().rust_mut().rows = rows;
        self.as_mut().end_reset_model();
        if before != self.as_ref().rust().rows.len() {
            self.as_mut().count_changed();
        }
        self.as_mut().values_changed();
    }

    /// Bring the model in line with `items`, emitting granular change
    /// notifications keyed on the configured key role.
    pub fn sync<T: Serialize>(mut self: Pin<&mut Self>, items: impl IntoIterator<Item = T>) {
        if self.as_ref().rust().key_role.is_none() {
            return self.reset(items);
        }
        let new_rows: Vec<Row> = {
            let rust = self.rust();
            items
                .into_iter()
                .map(|item| rust.make_row(to_value(&item)))
                .collect()
        };
        let parent = QModelIndex::default();
        let before = self.as_ref().rust().rows.len();
        let mut changed = false;

        // 1. Drop rows that no longer exist.
        for i in (0..before).rev() {
            let stale = !new_rows
                .iter()
                .any(|row| row.key == self.as_ref().rust().rows[i].key);
            if stale {
                self.as_mut().begin_remove_rows(&parent, i as i32, i as i32);
                self.as_mut().rust_mut().rows.remove(i);
                self.as_mut().end_remove_rows();
                changed = true;
            }
        }

        // 2. Walk the target order, moving, inserting or updating in place.
        for (i, new) in new_rows.into_iter().enumerate() {
            let found = self
                .as_ref()
                .rust()
                .rows
                .iter()
                .position(|row| row.key == new.key);
            match found {
                Some(j) if j == i => {}
                Some(j) => {
                    // j > i: everything before i is already settled.
                    if self
                        .as_mut()
                        .begin_move_rows(&parent, j as i32, j as i32, &parent, i as i32)
                    {
                        let row = self.as_mut().rust_mut().rows.remove(j);
                        self.as_mut().rust_mut().rows.insert(i, row);
                        self.as_mut().end_move_rows();
                        changed = true;
                    }
                }
                None => {
                    self.as_mut().begin_insert_rows(&parent, i as i32, i as i32);
                    self.as_mut().rust_mut().rows.insert(i, new);
                    self.as_mut().end_insert_rows();
                    changed = true;
                    continue;
                }
            }

            // Existing row at i: update the roles that changed.
            let roles_changed: Vec<i32> = {
                let old = &self.rust().rows[i];
                old.values
                    .iter()
                    .zip(&new.values)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(r, _)| USER_ROLE + 1 + r as i32)
                    .collect()
            };
            if !roles_changed.is_empty() {
                self.as_mut().rust_mut().rows[i] = new;
                let mut roles = QVector_i32::default();
                for role in roles_changed {
                    roles.append(role);
                }
                let index = self.as_ref().index(i as i32, 0, &parent);
                self.as_mut().data_changed(&index, &index, &roles);
                changed = true;
            }
        }

        if before != self.as_ref().rust().rows.len() {
            self.as_mut().count_changed();
        }
        if changed {
            self.as_mut().values_changed();
        }
    }

    fn count(&self) -> i32 {
        self.rows.len() as i32
    }

    fn values(&self) -> QList_QVariant {
        let mut list = QList_QVariant::default();
        list.reserve(self.rows.len() as isize);
        for row in 0..self.rows.len() {
            list.append(QVariant::from(&self.get(row as i32)));
        }
        list
    }

    fn get(&self, row: i32) -> QMap_QString_QVariant {
        let mut map = QMap_QString_QVariant::default();
        if let Some(row) = usize::try_from(row).ok().and_then(|r| self.rows.get(r)) {
            for (role, variant) in self.roles.iter().zip(&row.variants) {
                map.insert(QString::from(role.name.as_str()), variant.clone());
            }
        }
        map
    }

    fn index_of(&self, key: &QVariant) -> i32 {
        let key = variant_key(key);
        self.rows
            .iter()
            .position(|row| row.key == key)
            .map_or(-1, |i| i as i32)
    }

    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rows.len() as i32
    }

    fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let role = (role - USER_ROLE - 1) as usize;
        self.rows
            .get(index.row() as usize)
            .and_then(|row| row.variants.get(role))
            .cloned()
            .unwrap_or_default()
    }

    fn role_names(&self) -> QHash_i32_QByteArray {
        let mut hash = QHash_i32_QByteArray::default();
        for (i, role) in self.roles.iter().enumerate() {
            hash.insert(
                USER_ROLE + 1 + i as i32,
                QByteArray::from(role.name.as_str()),
            );
        }
        hash
    }
}

/// Best-effort mapping of a QML value back to the JSON key space.
fn variant_key(key: &QVariant) -> Value {
    if let Some(i) = key.value::<i64>() {
        Value::from(i)
    } else if let Some(u) = key.value::<u64>() {
        Value::from(u)
    } else if let Some(f) = key.value::<f64>() {
        Value::from(f)
    } else if let Some(s) = key.value::<QString>() {
        Value::from(s.to_string())
    } else {
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use cxx_qt::CxxQtType;
    use serde::Serialize;
    use serde_json::Value;

    use crate::construct::new_list_model;

    #[derive(Serialize)]
    struct Item {
        id: u64,
        name: &'static str,
        is_active: bool,
    }

    fn item(id: u64, name: &'static str, is_active: bool) -> Item {
        Item {
            id,
            name,
            is_active,
        }
    }

    fn keys(model: &super::qobject::ListModel) -> Vec<u64> {
        model
            .rows
            .iter()
            .map(|row| row.key.as_u64().unwrap())
            .collect()
    }

    #[test]
    fn sync_inserts_removes_moves_and_updates() {
        let mut model = new_list_model();
        model
            .pin_mut()
            .configure(&["id", "name", "is_active"], Some("id"));
        assert_eq!(model.rust().roles[2].name, "isActive");

        model
            .pin_mut()
            .sync([item(1, "a", true), item(2, "b", false), item(3, "c", false)]);
        assert_eq!(keys(&model), [1, 2, 3]);
        assert_eq!(model.count(), 3);

        // Remove 2, move 3 before 1, add 4, flip 1's flag.
        model
            .pin_mut()
            .sync([item(3, "c", true), item(1, "a", false), item(4, "d", false)]);
        assert_eq!(keys(&model), [3, 1, 4]);
        assert_eq!(model.rust().rows[1].values[2], Value::Bool(false));
        assert_eq!(model.index_of(&cxx_qt_lib::QVariant::from(&4i64)), 2);
        assert_eq!(model.index_of(&cxx_qt_lib::QVariant::from(&9i64)), -1);

        let row = model.get(0);
        assert_eq!(
            row.get(&cxx_qt_lib::QString::from("name"))
                .and_then(|v| v.value::<cxx_qt_lib::QString>())
                .map(|s| s.to_string()),
            Some("c".to_string())
        );
    }

    #[test]
    fn camel_case_roles() {
        assert_eq!(super::to_camel("active_window_id"), "activeWindowId");
        assert_eq!(super::to_camel("id"), "id");
    }
}
