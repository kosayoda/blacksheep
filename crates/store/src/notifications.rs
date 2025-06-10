//! Notification history: what was shown, newest first, with a read flag
//! for the unread badge. A row updated by a repeat moves to the front, so
//! order follows `notified_at`, not insertion.

use rusqlite::{Connection, params};
use serde::Serialize;

/// Rows kept; older ones are pruned on insert.
const KEEP: i64 = 500;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Record {
    pub id: i64,
    pub notified_at: i64,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub desktop_entry: String,
    pub urgency: u8,
    pub read: bool,
    /// Identical notifications stacked into this row.
    pub count: u32,
}

pub struct NewRecord<'a> {
    pub notified_at: i64,
    pub app_name: &'a str,
    pub app_icon: &'a str,
    pub summary: &'a str,
    pub body: &'a str,
    pub desktop_entry: &'a str,
    pub urgency: u8,
    pub count: u32,
}

/// Insert and return the row id.
pub fn insert(connection: &Connection, record: &NewRecord<'_>) -> rusqlite::Result<i64> {
    connection.execute(
        "INSERT INTO notifications (notified_at, app_name, app_icon, summary, body, desktop_entry, urgency, count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            record.notified_at,
            record.app_name,
            record.app_icon,
            record.summary,
            record.body,
            record.desktop_entry,
            record.urgency,
            record.count
        ],
    )?;
    let id = connection.last_insert_rowid();
    connection.execute(
        "DELETE FROM notifications WHERE id NOT IN (SELECT id FROM notifications ORDER BY id DESC LIMIT ?1)",
        params![KEEP],
    )?;
    Ok(id)
}

/// A replacement for an existing notification updates its row in place.
pub fn update(connection: &Connection, id: i64, record: &NewRecord<'_>) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE notifications SET notified_at = ?2, app_name = ?3, app_icon = ?4, summary = ?5,
                body = ?6, desktop_entry = ?7, urgency = ?8, count = ?9, read = 0 WHERE id = ?1",
        params![
            id,
            record.notified_at,
            record.app_name,
            record.app_icon,
            record.summary,
            record.body,
            record.desktop_entry,
            record.urgency,
            record.count
        ],
    )?;
    Ok(())
}

pub fn recent(connection: &Connection, limit: i64) -> rusqlite::Result<Vec<Record>> {
    let mut statement = connection.prepare(
        "SELECT id, notified_at, app_name, app_icon, summary, body, desktop_entry, urgency, read, count
         FROM notifications ORDER BY notified_at DESC, id DESC LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit], |row| {
        Ok(Record {
            id: row.get(0)?,
            notified_at: row.get(1)?,
            app_name: row.get(2)?,
            app_icon: row.get(3)?,
            summary: row.get(4)?,
            body: row.get(5)?,
            desktop_entry: row.get(6)?,
            urgency: row.get::<_, i64>(7)? as u8,
            read: row.get::<_, i64>(8)? != 0,
            count: row.get::<_, i64>(9)? as u32,
        })
    })?;
    rows.collect()
}

pub fn mark_all_read(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute("UPDATE notifications SET read = 1 WHERE read = 0", [])?;
    Ok(())
}

pub fn remove(connection: &Connection, id: i64) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM notifications WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn clear(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM notifications", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record<'a>(summary: &'a str, at: i64) -> NewRecord<'a> {
        NewRecord {
            notified_at: at,
            app_name: "test",
            app_icon: "",
            summary,
            body: "",
            desktop_entry: "",
            urgency: 1,
            count: 1,
        }
    }

    #[test]
    fn newest_first_and_read_flag() {
        let store = crate::Store::open_in_memory().unwrap();
        store
            .with(|c| {
                let first = insert(c, &record("one", 10))?;
                insert(c, &record("two", 20))?;
                let rows = recent(c, 10)?;
                assert_eq!(rows.iter().map(|r| r.summary.as_str()).collect::<Vec<_>>(), ["two", "one"]);
                assert!(rows.iter().all(|r| !r.read));
                mark_all_read(c)?;
                assert!(recent(c, 10)?.iter().all(|r| r.read));
                update(c, first, &NewRecord { count: 3, ..record("one again", 30) })?;
                let rows = recent(c, 10)?;
                assert_eq!(rows.iter().filter(|r| !r.read).count(), 1);
                assert_eq!(rows.iter().find(|r| r.id == first).unwrap().count, 3);
                remove(c, first)?;
                assert_eq!(recent(c, 10)?.len(), 1);
                clear(c)?;
                assert!(recent(c, 10)?.is_empty());
                Ok(())
            })
            .unwrap();
    }
}
