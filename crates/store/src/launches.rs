//! Launch history and the frecency score derived from it.
//!
//! Each launch is a timestamp. A score is the sum over launches of
//! `0.5 ^ (age / HALF_LIFE)`, so ten launches today beat ten launches last
//! month, and an app used daily stays on top without ever being pinned.

use std::collections::HashMap;

use rusqlite::{Connection, params};

/// Days for a launch to lose half its weight.
pub const HALF_LIFE_DAYS: f64 = 7.0;
/// Launches older than this are pruned; they would weigh under 0.02% anyway.
const KEEP_DAYS: i64 = 90;
/// Per app, so a heavily used app does not grow the table without bound.
const KEEP_PER_APP: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Score {
    pub frecency: f64,
    pub launches: u32,
    /// Unix seconds of the most recent launch.
    pub last: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Note a launch of `app_id` at the current time, pruning old rows.
pub fn record(connection: &Connection, app_id: &str) -> rusqlite::Result<()> {
    record_at(connection, app_id, now())
}

pub fn record_at(connection: &Connection, app_id: &str, at: i64) -> rusqlite::Result<()> {
    connection.execute("INSERT INTO launches (app_id, at) VALUES (?1, ?2)", params![app_id, at])?;
    connection.execute(
        "DELETE FROM launches WHERE at < ?1",
        params![at - KEEP_DAYS * 86_400],
    )?;
    connection.execute(
        "DELETE FROM launches WHERE app_id = ?1 AND at NOT IN (
            SELECT at FROM launches WHERE app_id = ?1 ORDER BY at DESC LIMIT ?2
         )",
        params![app_id, KEEP_PER_APP as i64],
    )?;
    Ok(())
}

/// Scores for every app with history.
pub fn scores(connection: &Connection) -> rusqlite::Result<HashMap<String, Score>> {
    scores_at(connection, now())
}

pub fn scores_at(connection: &Connection, now: i64) -> rusqlite::Result<HashMap<String, Score>> {
    let mut statement = connection.prepare("SELECT app_id, at FROM launches")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    let mut scores: HashMap<String, Score> = HashMap::new();
    for row in rows {
        let (app_id, at) = row?;
        let age_days = (now - at).max(0) as f64 / 86_400.0;
        let weight = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
        let score = scores.entry(app_id).or_default();
        score.frecency += weight;
        score.launches += 1;
        score.last = score.last.max(at);
    }
    Ok(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_use_outranks_old_use() {
        let store = crate::Store::open_in_memory().unwrap();
        store
            .with(|c| {
                let day = 86_400;
                let now = 100 * day;
                // "old": ten launches, 60 days ago. "recent": three launches this week.
                for i in 0..10 {
                    record_at(c, "old", now - 60 * day - i)?;
                }
                for i in 0..3 {
                    record_at(c, "recent", now - i * day)?;
                }
                let scores = scores_at(c, now)?;
                assert!(scores["recent"].frecency > scores["old"].frecency);
                assert_eq!(scores["old"].launches, 10);
                assert_eq!(scores["recent"].last, now);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn prunes_old_and_excess_rows() {
        let store = crate::Store::open_in_memory().unwrap();
        store
            .with(|c| {
                let day = 86_400;
                let now = 400 * day;
                record_at(c, "a", now - 200 * day)?; // pruned by age on the next record
                for i in 0..80 {
                    record_at(c, "a", now - i)?;
                }
                let count: i64 = c.query_row("SELECT COUNT(*) FROM launches", [], |r| r.get(0))?;
                assert_eq!(count as usize, KEEP_PER_APP);
                Ok(())
            })
            .unwrap();
    }
}
