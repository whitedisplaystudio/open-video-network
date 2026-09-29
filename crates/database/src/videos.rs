//! Discovered videos, creator profiles, and the local full-text index.

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use ovn_protocol::{ContentId, ProfileUpdate, VideoAnnouncement};

use crate::{now_secs, Database, DatabaseError, Result};

/// Public metadata about a video we have heard of.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoRecord {
    pub cid: String,
    pub creator: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub duration_secs: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_cid: Option<String>,
    pub created_at: i64,
    pub discovered_at: i64,
    /// Published by this node.
    pub is_local: bool,
    /// The manifest block is held locally.
    pub have_manifest: bool,
    /// Every chunk is held locally.
    pub have_content: bool,
}

/// What to store for a newly seen announcement.
pub struct VideoUpsert<'a> {
    pub announcement: &'a VideoAnnouncement,
    /// The exact CBOR bytes we received, kept so the claim stays re-verifiable.
    pub announcement_bytes: &'a [u8],
    pub is_local: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatorRecord {
    pub public_key: String,
    pub display_name: String,
    pub bio: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_cid: Option<String>,
    pub updated_at: i64,
    pub first_seen: i64,
    pub following: bool,
    pub video_count: i64,
}

fn row_to_video(row: &Row<'_>) -> rusqlite::Result<VideoRecord> {
    let tags: String = row.get("tags")?;
    Ok(VideoRecord {
        cid: row.get("cid")?,
        creator: row.get("creator_public_key")?,
        title: row.get("title")?,
        description: row.get("description")?,
        tags: tags.split_whitespace().map(str::to_string).collect(),
        duration_secs: row.get("duration_secs")?,
        thumbnail_cid: row.get("thumbnail_cid")?,
        created_at: row.get("created_at")?,
        discovered_at: row.get("discovered_at")?,
        is_local: row.get::<_, i64>("is_local")? != 0,
        have_manifest: row.get::<_, i64>("have_manifest")? != 0,
        have_content: row.get::<_, i64>("have_content")? != 0,
    })
}

const VIDEO_COLUMNS: &str = "rowid, cid, creator_public_key, title, description, tags, \
     duration_secs, thumbnail_cid, created_at, discovered_at, is_local, have_manifest, have_content";

impl Database {
    /// Store a verified announcement. Returns `true` if this video is new to
    /// us.
    ///
    /// The caller must have called [`VideoAnnouncement::verify`] first: this
    /// function stores what it is given.
    pub fn upsert_video(&self, upsert: VideoUpsert<'_>) -> Result<bool> {
        let announcement = upsert.announcement;
        let cid = announcement.video_cid.to_string();
        let creator = announcement.creator()?.to_hex();
        let tags = announcement.tags.join(" ");
        let now = now_secs();
        let conn = self.conn()?;

        let existing_created_at: Option<i64> = conn
            .query_row(
                "SELECT created_at FROM known_videos WHERE cid = ?1",
                params![&cid],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(previous) = existing_created_at {
            // Same content id means the same bytes, so metadata should not
            // really change. Accept a newer signed announcement anyway (a
            // creator may fix a typo), but never an older one.
            if announcement.created_at as i64 <= previous {
                if upsert.is_local {
                    conn.execute(
                        "UPDATE known_videos SET is_local = 1 WHERE cid = ?1",
                        params![&cid],
                    )?;
                }
                return Ok(false);
            }
            conn.execute(
                "UPDATE known_videos SET
                     creator_public_key = ?2, title = ?3, description = ?4, tags = ?5,
                     duration_secs = ?6, thumbnail_cid = ?7, created_at = ?8,
                     announcement = ?9, is_local = MAX(is_local, ?10)
                 WHERE cid = ?1",
                params![
                    &cid,
                    &creator,
                    &announcement.title,
                    &announcement.description,
                    &tags,
                    announcement.duration_secs as i64,
                    announcement.thumbnail_cid.map(|c| c.to_string()),
                    announcement.created_at as i64,
                    upsert.announcement_bytes,
                    i64::from(upsert.is_local),
                ],
            )?;
            return Ok(false);
        }

        conn.execute(
            "INSERT INTO known_videos
                 (cid, creator_public_key, title, description, tags, duration_secs,
                  thumbnail_cid, created_at, discovered_at, announcement, is_local,
                  have_manifest, have_content)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 0)",
            params![
                &cid,
                &creator,
                &announcement.title,
                &announcement.description,
                &tags,
                announcement.duration_secs as i64,
                announcement.thumbnail_cid.map(|c| c.to_string()),
                announcement.created_at as i64,
                now,
                upsert.announcement_bytes,
                i64::from(upsert.is_local),
            ],
        )?;
        Ok(true)
    }

    pub fn video(&self, cid: &ContentId) -> Result<Option<VideoRecord>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                &format!("SELECT {VIDEO_COLUMNS} FROM known_videos WHERE cid = ?1"),
                params![cid.to_string()],
                row_to_video,
            )
            .optional()?)
    }

    /// The original signed announcement bytes, for re-serving or re-checking.
    pub fn announcement_bytes(&self, cid: &ContentId) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT announcement FROM known_videos WHERE cid = ?1",
                params![cid.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Recently created videos, newest first, excluding blocked content.
    pub fn videos(&self, limit: usize, offset: usize) -> Result<Vec<VideoRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {VIDEO_COLUMNS} FROM known_videos v
              WHERE v.cid NOT IN (SELECT cid FROM blocked_cids)
                AND v.creator_public_key NOT IN (SELECT public_key FROM blocked_creators)
              ORDER BY created_at DESC, cid ASC
              LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], row_to_video)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Videos attributed to one creator, blocked or not.
    ///
    /// Unlike [`Database::videos`] this does not filter blocked content: it
    /// is used *because* a creator was just blocked.
    pub fn videos_by_creator(&self, public_key_hex: &str) -> Result<Vec<VideoRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {VIDEO_COLUMNS} FROM known_videos WHERE creator_public_key = ?1"
        ))?;
        let rows = stmt.query_map(params![public_key_hex], row_to_video)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Videos whose manifest we hold, so their chunk lists can be consulted.
    pub fn videos_with_manifest(&self, limit: usize) -> Result<Vec<VideoRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {VIDEO_COLUMNS} FROM known_videos WHERE have_manifest = 1 LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_video)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn local_videos(&self) -> Result<Vec<VideoRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {VIDEO_COLUMNS} FROM known_videos WHERE is_local = 1 ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([], row_to_video)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn video_count(&self) -> Result<i64> {
        Ok(self
            .conn()?
            .query_row("SELECT COUNT(*) FROM known_videos", [], |row| row.get(0))?)
    }

    /// Local full-text search (section 26).
    ///
    /// The query never leaves this process. User input is turned into quoted
    /// FTS5 phrase tokens rather than interpolated, so a query containing FTS
    /// operators is treated as text, not as syntax.
    pub fn search_videos(&self, query: &str, limit: usize) -> Result<Vec<VideoRecord>> {
        let fts_query = to_fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM known_videos v
               JOIN videos_fts f ON f.rowid = v.rowid
              WHERE videos_fts MATCH ?1
                AND v.cid NOT IN (SELECT cid FROM blocked_cids)
                AND v.creator_public_key NOT IN (SELECT public_key FROM blocked_creators)
              ORDER BY bm25(videos_fts, 8.0, 1.0, 4.0), v.created_at DESC
              LIMIT ?2",
            VIDEO_COLUMNS
                .split(", ")
                .map(|c| format!("v.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![fts_query, limit as i64], row_to_video)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_have_manifest(&self, cid: &ContentId, have: bool) -> Result<()> {
        self.conn()?.execute(
            "UPDATE known_videos SET have_manifest = ?2 WHERE cid = ?1",
            params![cid.to_string(), i64::from(have)],
        )?;
        Ok(())
    }

    pub fn set_have_content(&self, cid: &ContentId, have: bool) -> Result<()> {
        self.conn()?.execute(
            "UPDATE known_videos SET have_content = ?2 WHERE cid = ?1",
            params![cid.to_string(), i64::from(have)],
        )?;
        Ok(())
    }

    pub fn delete_video(&self, cid: &ContentId) -> Result<bool> {
        let changed = self.conn()?.execute(
            "DELETE FROM known_videos WHERE cid = ?1",
            params![cid.to_string()],
        )?;
        Ok(changed > 0)
    }

    // ------------------------------------------------------------ creators

    /// Store a verified profile update, ignoring one older than what we hold.
    pub fn upsert_creator(&self, profile: &ProfileUpdate) -> Result<bool> {
        let public_key = ovn_identity::PublicKey::from_bytes(&profile.public_key)
            .map_err(|e| DatabaseError::Malformed(e.to_string()))?
            .to_hex();
        let now = now_secs();
        let changed = self.conn()?.execute(
            "INSERT INTO creators (public_key, display_name, bio, avatar_cid, updated_at, first_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(public_key) DO UPDATE SET
                 display_name = excluded.display_name,
                 bio          = excluded.bio,
                 avatar_cid   = excluded.avatar_cid,
                 updated_at   = excluded.updated_at
             WHERE excluded.updated_at > creators.updated_at",
            params![
                public_key,
                profile.display_name,
                profile.bio,
                profile.avatar_cid.map(|c| c.to_string()),
                profile.updated_at as i64,
                now,
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn creator(&self, public_key_hex: &str) -> Result<Option<CreatorRecord>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT c.public_key, c.display_name, c.bio, c.avatar_cid, c.updated_at,
                        c.first_seen,
                        EXISTS(SELECT 1 FROM following f WHERE f.public_key = c.public_key),
                        (SELECT COUNT(*) FROM known_videos v
                          WHERE v.creator_public_key = c.public_key)
                   FROM creators c WHERE c.public_key = ?1",
                params![public_key_hex],
                |row| {
                    Ok(CreatorRecord {
                        public_key: row.get(0)?,
                        display_name: row.get(1)?,
                        bio: row.get(2)?,
                        avatar_cid: row.get(3)?,
                        updated_at: row.get(4)?,
                        first_seen: row.get(5)?,
                        following: row.get::<_, i64>(6)? != 0,
                        video_count: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    // ----------------------------------------------------------- following

    pub fn set_following(&self, public_key: &ovn_identity::PublicKey, follow: bool) -> Result<()> {
        let conn = self.conn()?;
        if follow {
            conn.execute(
                "INSERT OR IGNORE INTO following (public_key, since) VALUES (?1, ?2)",
                params![public_key.to_hex(), now_secs()],
            )?;
        } else {
            conn.execute(
                "DELETE FROM following WHERE public_key = ?1",
                params![public_key.to_hex()],
            )?;
        }
        Ok(())
    }

    pub fn following(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT public_key FROM following ORDER BY since DESC")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn is_following(&self, public_key_hex: &str) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM following WHERE public_key = ?1)",
            params![public_key_hex],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }
}

/// Turn free user text into a safe FTS5 MATCH expression.
///
/// Every token becomes a quoted phrase, so `NEAR`, `*`, `"` and friends are
/// searched for rather than executed. The last token gets a prefix `*` so that
/// typing grows results as you go.
fn to_fts_query(query: &str) -> String {
    let tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && !is_word_char(c))
        .map(|t| t.trim().replace('"', ""))
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    let last = tokens.len() - 1;
    tokens
        .iter()
        .enumerate()
        .map(|(i, token)| {
            if i == last {
                format!("\"{token}\"*")
            } else {
                format!("\"{token}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Keep CJK and other non-ASCII word characters together as tokens; FTS5's
/// unicode61 tokenizer will split them further if it needs to.
fn is_word_char(c: char) -> bool {
    !c.is_ascii() && !c.is_whitespace() && !c.is_ascii_punctuation()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_identity::Identity;
    use ovn_protocol::{to_cbor_vec, NewVideo};

    fn announce(identity: &Identity, title: &str, tags: &[&str], body: &[u8]) -> VideoAnnouncement {
        VideoAnnouncement::sign(
            NewVideo {
                video_cid: Some(ContentId::from_dag_cbor(body)),
                title: title.to_string(),
                description: "a description".to_string(),
                tags: tags.iter().map(|t| t.to_string()).collect(),
                duration_secs: 120,
                thumbnail_cid: None,
            },
            identity,
        )
        .unwrap()
    }

    fn store(db: &Database, announcement: &VideoAnnouncement, is_local: bool) -> bool {
        let bytes = to_cbor_vec(announcement).unwrap();
        db.upsert_video(VideoUpsert {
            announcement,
            announcement_bytes: &bytes,
            is_local,
        })
        .unwrap()
    }

    #[test]
    fn a_video_is_stored_and_read_back() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let ann = announce(&id, "Rust speedrun", &["Gaming", "rust"], b"one");
        assert!(store(&db, &ann, true));

        let record = db.video(&ann.video_cid).unwrap().unwrap();
        assert_eq!(record.title, "Rust speedrun");
        assert_eq!(record.tags, vec!["gaming", "rust"]);
        assert_eq!(record.creator, id.public_key().to_hex());
        assert!(record.is_local);
        assert!(!record.have_content);
        assert_eq!(db.video_count().unwrap(), 1);
    }

    #[test]
    fn the_same_announcement_twice_is_not_a_new_video() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let ann = announce(&id, "Once", &[], b"one");
        assert!(store(&db, &ann, false));
        assert!(!store(&db, &ann, false));
        assert_eq!(db.video_count().unwrap(), 1);
    }

    #[test]
    fn an_older_announcement_cannot_overwrite_a_newer_one() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let mut newer = announce(&id, "Correct title", &[], b"one");
        newer.created_at += 100;
        store(&db, &newer, false);

        let mut older = newer.clone();
        older.title = "Stale title".into();
        older.created_at -= 200;
        store(&db, &older, false);

        assert_eq!(
            db.video(&newer.video_cid).unwrap().unwrap().title,
            "Correct title"
        );
    }

    #[test]
    fn the_original_announcement_bytes_are_kept_verbatim() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let ann = announce(&id, "Keep me", &[], b"one");
        let bytes = to_cbor_vec(&ann).unwrap();
        store(&db, &ann, false);
        assert_eq!(
            db.announcement_bytes(&ann.video_cid).unwrap().unwrap(),
            bytes
        );
    }

    #[test]
    fn local_search_finds_by_title_description_and_tag() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        store(
            &db,
            &announce(&id, "Kingdom speedrun", &["gaming"], b"a"),
            false,
        );
        store(&db, &announce(&id, "Ambient set", &["music"], b"b"), false);

        assert_eq!(db.search_videos("kingdom", 10).unwrap().len(), 1);
        assert_eq!(db.search_videos("music", 10).unwrap().len(), 1);
        assert_eq!(db.search_videos("description", 10).unwrap().len(), 2);
        assert_eq!(db.search_videos("nonexistent", 10).unwrap().len(), 0);
    }

    #[test]
    fn search_matches_prefixes_so_typing_narrows_results() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        store(&db, &announce(&id, "Kingdom speedrun", &[], b"a"), false);
        assert_eq!(db.search_videos("king", 10).unwrap().len(), 1);
        assert_eq!(db.search_videos("kingdoms", 10).unwrap().len(), 0);
    }

    #[test]
    fn fts_operators_in_user_input_are_treated_as_text() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        store(&db, &announce(&id, "Safe title", &[], b"a"), false);
        // None of these may error or inject.
        for query in ["\" OR 1=1 --", "NEAR(a b)", "*", "^title", "title AND", ""] {
            assert!(db.search_videos(query, 10).is_ok(), "query {query:?}");
        }
    }

    #[test]
    fn search_reflects_deletions() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let ann = announce(&id, "Ephemeral", &[], b"a");
        store(&db, &ann, false);
        assert_eq!(db.search_videos("ephemeral", 10).unwrap().len(), 1);
        assert!(db.delete_video(&ann.video_cid).unwrap());
        assert_eq!(db.search_videos("ephemeral", 10).unwrap().len(), 0);
    }

    #[test]
    fn blocked_content_disappears_from_listings_and_search() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let ann = announce(&id, "Unwanted", &[], b"a");
        store(&db, &ann, false);
        assert_eq!(db.videos(10, 0).unwrap().len(), 1);

        db.block_cid(&ann.video_cid, "spam").unwrap();
        assert_eq!(db.videos(10, 0).unwrap().len(), 0);
        assert_eq!(db.search_videos("unwanted", 10).unwrap().len(), 0);

        db.unblock_cid(&ann.video_cid).unwrap();
        db.block_creator(&id.public_key(), "spam").unwrap();
        assert_eq!(db.videos(10, 0).unwrap().len(), 0);
    }

    #[test]
    fn listings_are_newest_first_and_paginate() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        for i in 0..5 {
            let mut ann = announce(&id, &format!("Video {i}"), &[], format!("{i}").as_bytes());
            ann.created_at = 1_000 + i as u64;
            store(&db, &ann, false);
        }
        let page = db.videos(2, 0).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].title, "Video 4");
        assert_eq!(db.videos(2, 2).unwrap()[0].title, "Video 2");
    }

    #[test]
    fn videos_can_be_listed_by_creator_even_when_blocked() {
        let db = Database::open_in_memory().unwrap();
        let one = Identity::generate();
        let two = Identity::generate();
        let first = announce(&one, "First", &[], b"a");
        store(&db, &first, false);
        store(&db, &announce(&one, "Second", &[], b"b"), false);
        store(&db, &announce(&two, "Elsewhere", &[], b"c"), false);

        let mine = db.videos_by_creator(&one.public_key().to_hex()).unwrap();
        assert_eq!(mine.len(), 2);

        // Still listed after a block: that is the point of the call.
        db.block_creator(&one.public_key(), "spam").unwrap();
        assert_eq!(
            db.videos_by_creator(&one.public_key().to_hex())
                .unwrap()
                .len(),
            2
        );
        assert!(db
            .videos(10, 0)
            .unwrap()
            .iter()
            .all(|v| v.creator != one.public_key().to_hex()));
        assert!(db.videos_by_creator("nobody").unwrap().is_empty());

        // `videos_with_manifest` only reports what we actually hold.
        assert!(db.videos_with_manifest(10).unwrap().is_empty());
        db.set_have_manifest(&first.video_cid, true).unwrap();
        assert_eq!(db.videos_with_manifest(10).unwrap().len(), 1);
    }

    #[test]
    fn have_flags_track_what_we_hold() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let ann = announce(&id, "Fetched", &[], b"a");
        store(&db, &ann, false);
        db.set_have_manifest(&ann.video_cid, true).unwrap();
        db.set_have_content(&ann.video_cid, true).unwrap();
        let record = db.video(&ann.video_cid).unwrap().unwrap();
        assert!(record.have_manifest && record.have_content);
    }

    #[test]
    fn profiles_only_move_forward_in_time() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        let mut newer = ProfileUpdate::sign("Current".into(), "".into(), &id).unwrap();
        newer.updated_at += 50;
        assert!(db.upsert_creator(&newer).unwrap());

        let mut older = ProfileUpdate::sign("Stale".into(), "".into(), &id).unwrap();
        older.updated_at = newer.updated_at - 10;
        assert!(!db.upsert_creator(&older).unwrap());

        let creator = db.creator(&id.public_key().to_hex()).unwrap().unwrap();
        assert_eq!(creator.display_name, "Current");
    }

    #[test]
    fn creator_records_report_following_and_video_counts() {
        let db = Database::open_in_memory().unwrap();
        let id = Identity::generate();
        db.upsert_creator(&ProfileUpdate::sign("Author".into(), "hi".into(), &id).unwrap())
            .unwrap();
        store(&db, &announce(&id, "One", &[], b"a"), false);
        store(&db, &announce(&id, "Two", &[], b"b"), false);

        let hex = id.public_key().to_hex();
        let creator = db.creator(&hex).unwrap().unwrap();
        assert_eq!(creator.video_count, 2);
        assert!(!creator.following);

        db.set_following(&id.public_key(), true).unwrap();
        assert!(db.is_following(&hex).unwrap());
        assert!(db.creator(&hex).unwrap().unwrap().following);

        db.set_following(&id.public_key(), false).unwrap();
        assert!(!db.is_following(&hex).unwrap());
    }
}
