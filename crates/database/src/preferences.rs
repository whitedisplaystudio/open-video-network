//! The tag preference vector. **Local only.**
//!
//! Same rule as [`crate::watch`]: [`TagWeight`] has no `Serialize`, so a
//! preference vector cannot be put into a protocol message. Section 27's model
//! lives in `ovn-recommendation`; this module only stores the numbers.

use rusqlite::{params, OptionalExtension};

use crate::{now_secs, Database, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct TagWeight {
    pub tag: String,
    pub weight: f64,
    pub updated_at: i64,
}

impl Database {
    pub fn set_tag_weight(&self, tag: &str, weight: f64) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO preferences (tag, weight, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(tag) DO UPDATE SET weight = ?2, updated_at = ?3",
            params![tag, weight, now_secs()],
        )?;
        Ok(())
    }

    pub fn tag_weight(&self, tag: &str) -> Result<Option<f64>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT weight FROM preferences WHERE tag = ?1",
                params![tag],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The whole preference vector, strongest first.
    pub fn tag_weights(&self) -> Result<Vec<TagWeight>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT tag, weight, updated_at FROM preferences ORDER BY weight DESC, tag")?;
        let rows = stmt.query_map([], |row| {
            Ok(TagWeight {
                tag: row.get(0)?,
                weight: row.get(1)?,
                updated_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replace the whole vector in one transaction, so a reader never sees a
    /// half-rebuilt model.
    pub fn replace_tag_weights(&self, weights: &[(String, f64)]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM preferences", [])?;
        {
            let now = now_secs();
            let mut stmt = tx
                .prepare("INSERT INTO preferences (tag, weight, updated_at) VALUES (?1, ?2, ?3)")?;
            for (tag, weight) in weights {
                stmt.execute(params![tag, weight, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_preferences(&self) -> Result<usize> {
        Ok(self.conn()?.execute("DELETE FROM preferences", [])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_are_stored_and_updated() {
        let db = Database::open_in_memory().unwrap();
        db.set_tag_weight("gaming", 0.5).unwrap();
        assert_eq!(db.tag_weight("gaming").unwrap(), Some(0.5));
        db.set_tag_weight("gaming", 0.91).unwrap();
        assert_eq!(db.tag_weight("gaming").unwrap(), Some(0.91));
        assert_eq!(db.tag_weight("music").unwrap(), None);
    }

    #[test]
    fn the_vector_comes_back_strongest_first() {
        let db = Database::open_in_memory().unwrap();
        db.set_tag_weight("music", 0.31).unwrap();
        db.set_tag_weight("gaming", 0.91).unwrap();
        db.set_tag_weight("indie", 0.76).unwrap();
        let tags: Vec<_> = db
            .tag_weights()
            .unwrap()
            .into_iter()
            .map(|t| t.tag)
            .collect();
        assert_eq!(tags, vec!["gaming", "indie", "music"]);
    }

    #[test]
    fn replacing_the_vector_is_atomic_and_total() {
        let db = Database::open_in_memory().unwrap();
        db.set_tag_weight("stale", 1.0).unwrap();
        db.replace_tag_weights(&[("gaming".into(), 0.8), ("rust".into(), 0.4)])
            .unwrap();
        let tags: Vec<_> = db
            .tag_weights()
            .unwrap()
            .into_iter()
            .map(|t| t.tag)
            .collect();
        assert_eq!(tags, vec!["gaming", "rust"]);
        assert_eq!(db.tag_weight("stale").unwrap(), None);
    }

    #[test]
    fn the_user_can_erase_the_model() {
        let db = Database::open_in_memory().unwrap();
        db.set_tag_weight("gaming", 0.5).unwrap();
        assert_eq!(db.clear_preferences().unwrap(), 1);
        assert!(db.tag_weights().unwrap().is_empty());
    }
}
