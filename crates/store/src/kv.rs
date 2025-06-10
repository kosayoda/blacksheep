//! Small key-value state for the configuration: JSON values by key.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::Value;

pub fn get(connection: &Connection, key: &str) -> rusqlite::Result<Option<Value>> {
    let text: Option<String> = connection
        .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |row| row.get(0))
        .optional()?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
}

pub fn set(connection: &Connection, key: &str, value: &Value) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value.to_string()],
    )?;
    Ok(())
}

pub fn remove(connection: &Connection, key: &str) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        let store = crate::Store::open_in_memory().unwrap();
        store
            .with(|c| {
                super::set(c, "bar.visible", &serde_json::json!(false))?;
                super::set(c, "bar.visible", &serde_json::json!(true))?;
                assert_eq!(super::get(c, "bar.visible")?, Some(serde_json::json!(true)));
                assert_eq!(super::get(c, "missing")?, None);
                super::remove(c, "bar.visible")?;
                assert_eq!(super::get(c, "bar.visible")?, None);
                Ok(())
            })
            .unwrap();
    }
}
