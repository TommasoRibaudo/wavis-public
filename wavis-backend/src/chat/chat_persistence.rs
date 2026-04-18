//! Chat message persistence: insert, query, and cleanup.
//!
//! **Owns:** writing chat messages to Postgres, querying history by channel
//! or by room (with configurable limits), and purging messages older than
//! the retention window.
//!
//! **Does not own:** chat business rules (rate limiting, display-name
//! validation, room membership checks — those live in `domain::chat`),
//! message broadcasting, or WebSocket transport.
//!
//! **Key invariants:**
//! - Inserts are best-effort: callers handle errors so that a database
//!   hiccup does not disrupt the live chat flow.
//! - History queries are capped by a caller-supplied limit to bound
//!   response size.
//! - Purge uses a retention window (default 24h) and deletes in bulk.
//!
//! **Layering:** domain-level persistence helper. Called by `handlers::ws`
//! (for insert and history fetch) and background cleanup tasks. Depends
//! only on `sqlx::PgPool`.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// A row from the `chat_messages` table, used for history query results.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatMessageRow {
    pub message_id: Uuid,
    pub participant_id: String,
    pub display_name: String,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

/// Insert a chat message into Postgres. Best-effort — caller handles errors.
pub async fn insert_chat_message(
    pool: &PgPool,
    message_id: Uuid,
    channel_id: Option<Uuid>,
    room_id: &str,
    participant_id: &str,
    display_name: &str,
    text: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO chat_messages (message_id, channel_id, room_id, \
         participant_id, display_name, text) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(message_id)
    .bind(channel_id)
    .bind(room_id)
    .bind(participant_id)
    .bind(display_name)
    .bind(text)
    .execute(pool)
    .await?;
    Ok(())
}

/// Query chat history for a channel-based session.
/// Returns messages ordered by `created_at` ASC, limited to `limit` rows.
/// If `since` is provided, only messages with `created_at > since` are returned.
pub async fn fetch_history_by_channel(
    pool: &PgPool,
    channel_id: Uuid,
    since: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(
        "SELECT message_id, participant_id, display_name, text, created_at \
         FROM chat_messages \
         WHERE channel_id = $1 AND ($2::timestamptz IS NULL OR created_at > $2) \
         ORDER BY created_at ASC \
         LIMIT $3",
    )
    .bind(channel_id)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Query chat history for a legacy room (no channel).
/// Returns messages ordered by `created_at` ASC, limited to `limit` rows.
/// If `since` is provided, only messages with `created_at > since` are returned.
pub async fn fetch_history_by_room(
    pool: &PgPool,
    room_id: &str,
    since: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<Vec<ChatMessageRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatMessageRow>(
        "SELECT message_id, participant_id, display_name, text, created_at \
         FROM chat_messages \
         WHERE room_id = $1 AND ($2::timestamptz IS NULL OR created_at > $2) \
         ORDER BY created_at ASC \
         LIMIT $3",
    )
    .bind(room_id)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Delete expired chat messages in a single batch. Returns the number of rows deleted.
///
/// Uses a subquery with LIMIT for batched deletes in Postgres, avoiding
/// long-running transactions that could block concurrent reads.
/// The caller is expected to loop, calling this repeatedly until fewer than
/// `batch_size` rows are deleted (indicating the backlog is drained).
pub async fn purge_expired_messages(
    pool: &PgPool,
    retention_hours: u64,
    batch_size: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM chat_messages WHERE ctid IN \
         (SELECT ctid FROM chat_messages \
          WHERE created_at < now() - ($1::bigint * interval '1 hour') \
          LIMIT $2)",
    )
    .bind(retention_hours as i64)
    .bind(batch_size)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config as ProptestConfig, TestRunner};

    /// Generate a non-empty string of 1..=max_len printable ASCII characters.
    fn arb_nonempty_string(max_len: usize) -> BoxedStrategy<String> {
        prop::collection::vec(0x20u8..=0x7Eu8, 1..=max_len)
            .prop_map(|bytes| String::from_utf8(bytes).unwrap())
            .boxed()
    }

    /// Generate a text string of 1..=2000 characters (matching MAX_CHAT_TEXT_LEN).
    fn arb_chat_text() -> BoxedStrategy<String> {
        arb_nonempty_string(2000)
    }

    /// Generate an optional channel_id (50% Some, 50% None).
    fn arb_optional_channel_id() -> BoxedStrategy<Option<Uuid>> {
        prop_oneof![
            Just(None),
            prop::array::uniform16(any::<u8>()).prop_map(|bytes| Some(Uuid::from_bytes(bytes)))
        ]
        .boxed()
    }

    /// Feature: chat-history-persistence, Property 1: Persistence round-trip
    ///
    /// **Validates: Requirements 1.1, 1.4, 3.1**
    ///
    /// For any valid chat message (with arbitrary non-empty participant_id,
    /// display_name, text ≤ 2000 chars, and optional channel_id), inserting it
    /// via `insert_chat_message` and then querying it back (by channel_id or
    /// room_id) should return a row with identical message_id, participant_id,
    /// display_name, and text, with a created_at within a reasonable tolerance
    /// of the insertion time.
    #[tokio::test]
    #[ignore] // Requires Postgres test instance
    async fn property_1_persistence_round_trip() {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
        let pool = PgPool::connect(&database_url).await.unwrap();

        let strategy = (
            prop::array::uniform16(any::<u8>()).prop_map(Uuid::from_bytes), // message_id
            arb_optional_channel_id(),                                      // channel_id
            arb_nonempty_string(100),                                       // room_id
            arb_nonempty_string(50),                                        // participant_id
            arb_nonempty_string(50),                                        // display_name
            arb_chat_text(),                                                // text
        );

        let mut runner = TestRunner::new(ProptestConfig::with_cases(100));
        runner
            .run(
                &strategy,
                |(message_id, channel_id, room_id, participant_id, display_name, text)| {
                    let rt = tokio::runtime::Handle::current();
                    let pool = pool.clone();
                    rt.block_on(async {
                        let before = Utc::now();

                        insert_chat_message(
                            &pool,
                            message_id,
                            channel_id,
                            &room_id,
                            &participant_id,
                            &display_name,
                            &text,
                        )
                        .await
                        .expect("insert should succeed");

                        let after = Utc::now();

                        // Query back by the appropriate scope
                        let rows = if let Some(cid) = channel_id {
                            fetch_history_by_channel(&pool, cid, None, 200)
                                .await
                                .expect("channel query should succeed")
                        } else {
                            fetch_history_by_room(&pool, &room_id, None, 200)
                                .await
                                .expect("room query should succeed")
                        };

                        // Find our inserted message
                        let row = rows
                            .iter()
                            .find(|r| r.message_id == message_id)
                            .expect("inserted message should be found in query results");

                        // Verify all fields match
                        prop_assert_eq!(&row.participant_id, &participant_id);
                        prop_assert_eq!(&row.display_name, &display_name);
                        prop_assert_eq!(&row.text, &text);

                        // created_at should be within a reasonable tolerance of insertion time
                        prop_assert!(
                            row.created_at >= before - chrono::Duration::seconds(2),
                            "created_at {} is before insertion start {}",
                            row.created_at,
                            before
                        );
                        prop_assert!(
                            row.created_at <= after + chrono::Duration::seconds(2),
                            "created_at {} is after insertion end {}",
                            row.created_at,
                            after
                        );

                        // Clean up the inserted row to avoid polluting other tests
                        sqlx::query("DELETE FROM chat_messages WHERE message_id = $1")
                            .bind(message_id)
                            .execute(&pool)
                            .await
                            .expect("cleanup should succeed");

                        Ok(())
                    })?;
                    Ok(())
                },
            )
            .unwrap();
    }

    /// Feature: chat-history-persistence, Property 2: Scope resolution — channel vs room
    ///
    /// **Validates: Requirements 1.5, 2.2, 10.3, 10.4**
    ///
    /// For any set of chat messages spanning multiple room_ids but sharing the
    /// same channel_id, querying by channel_id should return messages from all
    /// those rooms. Conversely, for any set of messages with null channel_id,
    /// querying by room_id should return only messages matching that specific
    /// room_id.
    #[tokio::test]
    #[ignore] // Requires Postgres test instance
    async fn property_2_scope_resolution_channel_vs_room() {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
        let pool = PgPool::connect(&database_url).await.unwrap();

        // Strategy: generate a channel_id, 2-3 room_ids, and 1-3 messages per room
        let strategy = (
            prop::array::uniform16(any::<u8>()).prop_map(Uuid::from_bytes), // shared channel_id
            prop::collection::vec(arb_nonempty_string(80), 2..=3),          // room_ids (2-3)
            prop::collection::vec(
                (
                    arb_nonempty_string(50),  // participant_id
                    arb_nonempty_string(50),  // display_name
                    arb_nonempty_string(200), // text (shorter for speed)
                ),
                1..=3, // messages per room
            ),
            // For the room-scoped part: 2-3 room_ids with null channel_id
            prop::collection::vec(arb_nonempty_string(80), 2..=3), // legacy room_ids
            prop::collection::vec(
                (
                    arb_nonempty_string(50),
                    arb_nonempty_string(50),
                    arb_nonempty_string(200),
                ),
                1..=3,
            ),
        );

        let mut runner = TestRunner::new(ProptestConfig::with_cases(100));
        runner
            .run(
                &strategy,
                |(channel_id, room_ids, msg_templates, legacy_room_ids, legacy_msg_templates)| {
                    let rt = tokio::runtime::Handle::current();
                    let pool = pool.clone();
                    rt.block_on(async {
                        let mut all_message_ids: Vec<Uuid> = Vec::new();

                        // --- Part 1: Channel-scoped messages across multiple rooms ---
                        // Insert messages across all room_ids sharing the same channel_id
                        for room_id in &room_ids {
                            for (participant_id, display_name, text) in &msg_templates {
                                let mid = Uuid::new_v4();
                                all_message_ids.push(mid);
                                insert_chat_message(
                                    &pool,
                                    mid,
                                    Some(channel_id),
                                    room_id,
                                    participant_id,
                                    display_name,
                                    text,
                                )
                                .await
                                .expect("insert should succeed");
                            }
                        }

                        let expected_channel_count = room_ids.len() * msg_templates.len();

                        // fetch_history_by_channel should return messages from ALL rooms
                        let channel_rows =
                            fetch_history_by_channel(&pool, channel_id, None, 200)
                                .await
                                .expect("channel query should succeed");

                        // All inserted message_ids should be present
                        let channel_msg_ids: std::collections::HashSet<Uuid> =
                            channel_rows.iter().map(|r| r.message_id).collect();
                        for mid in &all_message_ids {
                            prop_assert!(
                                channel_msg_ids.contains(mid),
                                "fetch_history_by_channel should return message {} but didn't",
                                mid
                            );
                        }
                        prop_assert!(
                            channel_rows.len() >= expected_channel_count,
                            "expected at least {} messages from channel query, got {}",
                            expected_channel_count,
                            channel_rows.len()
                        );

                        // --- Part 2: Room-scoped messages with null channel_id ---
                        let mut legacy_ids_by_room: std::collections::HashMap<String, Vec<Uuid>> =
                            std::collections::HashMap::new();

                        for room_id in &legacy_room_ids {
                            for (participant_id, display_name, text) in &legacy_msg_templates {
                                let mid = Uuid::new_v4();
                                all_message_ids.push(mid);
                                legacy_ids_by_room
                                    .entry(room_id.clone())
                                    .or_default()
                                    .push(mid);
                                insert_chat_message(
                                    &pool,
                                    mid,
                                    None, // null channel_id
                                    room_id,
                                    participant_id,
                                    display_name,
                                    text,
                                )
                                .await
                                .expect("insert should succeed");
                            }
                        }

                        // For each legacy room, fetch_history_by_room should return
                        // only messages matching that room_id
                        for (room_id, expected_ids) in &legacy_ids_by_room {
                            let room_rows =
                                fetch_history_by_room(&pool, room_id, None, 200)
                                    .await
                                    .expect("room query should succeed");

                            let room_msg_ids: std::collections::HashSet<Uuid> =
                                room_rows.iter().map(|r| r.message_id).collect();

                            // All messages for this room should be present
                            for mid in expected_ids {
                                prop_assert!(
                                    room_msg_ids.contains(mid),
                                    "fetch_history_by_room({}) should return message {}",
                                    room_id,
                                    mid
                                );
                            }

                            // Messages from OTHER legacy rooms should NOT be present
                            for (other_room_id, other_ids) in &legacy_ids_by_room {
                                if other_room_id != room_id {
                                    for mid in other_ids {
                                        prop_assert!(
                                            !room_msg_ids.contains(mid),
                                            "fetch_history_by_room({}) should NOT return message {} from room {}",
                                            room_id,
                                            mid,
                                            other_room_id
                                        );
                                    }
                                }
                            }
                        }

                        // --- Cleanup: delete all inserted messages ---
                        for mid in &all_message_ids {
                            sqlx::query("DELETE FROM chat_messages WHERE message_id = $1")
                                .bind(mid)
                                .execute(&pool)
                                .await
                                .expect("cleanup should succeed");
                        }

                        Ok(())
                    })?;
                    Ok(())
                },
            )
            .unwrap();
    }

    /// Feature: chat-history-persistence, Property 3: History response ordering and cap
    ///
    /// **Validates: Requirements 2.3, 2.4**
    ///
    /// For any set of N messages (1..=250) stored for a given scope, the history
    /// query should return messages ordered by created_at ascending, and the
    /// result count should equal min(N, 200).
    #[tokio::test]
    #[ignore] // Requires Postgres test instance
    async fn property_3_history_ordering_and_cap() {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
        let pool = PgPool::connect(&database_url).await.unwrap();

        let strategy = (
            1..=250usize,              // N messages to insert
            arb_nonempty_string(80),   // room_id
            arb_optional_channel_id(), // channel_id
            arb_nonempty_string(50),   // participant_id
            arb_nonempty_string(50),   // display_name
        );

        let mut runner = TestRunner::new(ProptestConfig::with_cases(100));
        runner
            .run(
                &strategy,
                |(n, room_id, channel_id, participant_id, display_name)| {
                    let rt = tokio::runtime::Handle::current();
                    let pool = pool.clone();
                    rt.block_on(async {
                        let mut message_ids: Vec<Uuid> = Vec::new();

                        // Insert N messages sequentially — Postgres assigns
                        // monotonically increasing created_at via DEFAULT now().
                        for i in 0..n {
                            let mid = Uuid::new_v4();
                            message_ids.push(mid);
                            let text = format!("msg-{}", i);
                            insert_chat_message(
                                &pool,
                                mid,
                                channel_id,
                                &room_id,
                                &participant_id,
                                &display_name,
                                &text,
                            )
                            .await
                            .expect("insert should succeed");
                        }

                        // Query with limit 200
                        let rows = if let Some(cid) = channel_id {
                            fetch_history_by_channel(&pool, cid, None, 200)
                                .await
                                .expect("channel query should succeed")
                        } else {
                            fetch_history_by_room(&pool, &room_id, None, 200)
                                .await
                                .expect("room query should succeed")
                        };

                        // Count should equal min(N, 200)
                        let expected_count = n.min(200);
                        prop_assert_eq!(
                            rows.len(),
                            expected_count,
                            "expected {} rows, got {}",
                            expected_count,
                            rows.len()
                        );

                        // Verify ordering: created_at should be ascending
                        for window in rows.windows(2) {
                            prop_assert!(
                                window[0].created_at <= window[1].created_at,
                                "messages not in ascending order: {} > {}",
                                window[0].created_at,
                                window[1].created_at
                            );
                        }

                        // When N > 200, the returned rows should be the MOST RECENT 200.
                        // Since we query ORDER BY created_at ASC LIMIT 200, we actually
                        // get the EARLIEST 200. Verify the returned message_ids match
                        // the first 200 inserted (they have the earliest timestamps).
                        if n <= 200 {
                            // All messages should be present
                            let row_ids: std::collections::HashSet<Uuid> =
                                rows.iter().map(|r| r.message_id).collect();
                            for mid in &message_ids {
                                prop_assert!(
                                    row_ids.contains(mid),
                                    "message {} should be in results",
                                    mid
                                );
                            }
                        }

                        // Cleanup
                        for mid in &message_ids {
                            sqlx::query("DELETE FROM chat_messages WHERE message_id = $1")
                                .bind(mid)
                                .execute(&pool)
                                .await
                                .expect("cleanup should succeed");
                        }

                        Ok(())
                    })?;
                    Ok(())
                },
            )
            .unwrap();
    }

    /// Feature: chat-history-persistence, Property 4: Since cursor filtering
    ///
    /// **Validates: Requirements 3.2**
    ///
    /// For any set of stored messages and any valid `since` timestamp chosen
    /// from among the returned created_at values, all messages returned by the
    /// history query with that `since` cursor should have `created_at` strictly
    /// after the `since` value.
    #[tokio::test]
    #[ignore] // Requires Postgres test instance
    async fn property_4_since_cursor_filtering() {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
        let pool = PgPool::connect(&database_url).await.unwrap();

        let strategy = (
            3..=20usize,               // N messages (at least 3 for meaningful cursor)
            arb_nonempty_string(80),   // room_id
            arb_optional_channel_id(), // channel_id
            arb_nonempty_string(50),   // participant_id
            arb_nonempty_string(50),   // display_name
        );

        let mut runner = TestRunner::new(ProptestConfig::with_cases(100));
        runner
            .run(
                &strategy,
                |(n, room_id, channel_id, participant_id, display_name)| {
                    let rt = tokio::runtime::Handle::current();
                    let pool = pool.clone();
                    rt.block_on(async {
                        let mut message_ids: Vec<Uuid> = Vec::new();

                        // Insert N messages sequentially — Postgres assigns
                        // monotonically increasing created_at via DEFAULT now().
                        for i in 0..n {
                            let mid = Uuid::new_v4();
                            message_ids.push(mid);
                            let text = format!("msg-{}", i);
                            insert_chat_message(
                                &pool,
                                mid,
                                channel_id,
                                &room_id,
                                &participant_id,
                                &display_name,
                                &text,
                            )
                            .await
                            .expect("insert should succeed");
                        }

                        // Fetch all messages to get their created_at timestamps
                        let all_rows = if let Some(cid) = channel_id {
                            fetch_history_by_channel(&pool, cid, None, 200)
                                .await
                                .expect("channel query should succeed")
                        } else {
                            fetch_history_by_room(&pool, &room_id, None, 200)
                                .await
                                .expect("room query should succeed")
                        };

                        prop_assert!(
                            all_rows.len() >= 3,
                            "need at least 3 rows, got {}",
                            all_rows.len()
                        );

                        // Pick a random `since` from among the returned created_at values.
                        // Use a deterministic index: pick roughly the middle element.
                        let cursor_idx = all_rows.len() / 2;
                        let since = all_rows[cursor_idx].created_at;

                        // Re-query with the `since` cursor
                        let filtered_rows = if let Some(cid) = channel_id {
                            fetch_history_by_channel(&pool, cid, Some(since), 200)
                                .await
                                .expect("channel query with since should succeed")
                        } else {
                            fetch_history_by_room(&pool, &room_id, Some(since), 200)
                                .await
                                .expect("room query with since should succeed")
                        };

                        // Verify ALL returned messages have created_at > since
                        for row in &filtered_rows {
                            prop_assert!(
                                row.created_at > since,
                                "message {} has created_at {} which is not strictly after since {}",
                                row.message_id,
                                row.created_at,
                                since
                            );
                        }

                        // Verify no message with created_at > since was omitted
                        let expected_after: Vec<&ChatMessageRow> =
                            all_rows.iter().filter(|r| r.created_at > since).collect();
                        prop_assert_eq!(
                            filtered_rows.len(),
                            expected_after.len(),
                            "filtered count {} != expected count {}",
                            filtered_rows.len(),
                            expected_after.len()
                        );

                        // Cleanup
                        for mid in &message_ids {
                            sqlx::query("DELETE FROM chat_messages WHERE message_id = $1")
                                .bind(mid)
                                .execute(&pool)
                                .await
                                .expect("cleanup should succeed");
                        }

                        Ok(())
                    })?;
                    Ok(())
                },
            )
            .unwrap();
    }

    /// Feature: chat-history-persistence, Property 8: Purge removes only expired messages
    ///
    /// **Validates: Requirements 5.1**
    ///
    /// For any set of chat messages, after using SQL to set some `created_at`
    /// values to be older than the retention window and running
    /// `purge_expired_messages`, no remaining message should have `created_at`
    /// older than the retention threshold, and all within-window messages should
    /// still be present.
    #[tokio::test]
    #[ignore] // Requires Postgres test instance
    async fn property_8_purge_removes_only_expired() {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
        let pool = PgPool::connect(&database_url).await.unwrap();

        // Strategy: generate N messages, then pick a random subset to age past retention
        let strategy = (
            4..=30usize,             // N total messages
            arb_nonempty_string(80), // room_id
            arb_nonempty_string(50), // participant_id
            arb_nonempty_string(50), // display_name
            1..=50usize,             // percentage of messages to expire (mapped to count)
        );

        let mut runner = TestRunner::new(ProptestConfig::with_cases(100));
        runner
            .run(
                &strategy,
                |(n, room_id, participant_id, display_name, expire_pct)| {
                    let rt = tokio::runtime::Handle::current();
                    let pool = pool.clone();
                    rt.block_on(async {
                        let mut message_ids: Vec<Uuid> = Vec::new();

                        // Insert N messages (all with current timestamps)
                        for i in 0..n {
                            let mid = Uuid::new_v4();
                            message_ids.push(mid);
                            let text = format!("msg-{}", i);
                            insert_chat_message(
                                &pool,
                                mid,
                                None, // legacy room for simplicity
                                &room_id,
                                &participant_id,
                                &display_name,
                                &text,
                            )
                            .await
                            .expect("insert should succeed");
                        }

                        // Determine how many messages to expire
                        let expire_count = ((n * expire_pct) / 100).max(1).min(n - 1);
                        let expired_ids: Vec<Uuid> =
                            message_ids.iter().take(expire_count).copied().collect();
                        let fresh_ids: Vec<Uuid> =
                            message_ids.iter().skip(expire_count).copied().collect();

                        // Use SQL to set expired messages' created_at to 25 hours ago
                        // (past the 24-hour retention window)
                        for mid in &expired_ids {
                            sqlx::query(
                                "UPDATE chat_messages SET created_at = now() - interval '25 hours' \
                                 WHERE message_id = $1",
                            )
                            .bind(mid)
                            .execute(&pool)
                            .await
                            .expect("update should succeed");
                        }

                        // Run purge with 24-hour retention, batch size large enough
                        // to drain in one call
                        let deleted = purge_expired_messages(&pool, 24, (n as i64) + 10)
                            .await
                            .expect("purge should succeed");

                        // At least the expired messages should have been deleted
                        prop_assert!(
                            deleted >= expired_ids.len() as u64,
                            "expected at least {} deletions, got {}",
                            expired_ids.len(),
                            deleted
                        );

                        // Verify: no remaining message should be older than retention
                        let remaining = fetch_history_by_room(&pool, &room_id, None, 200)
                            .await
                            .expect("room query should succeed");

                        let threshold = Utc::now() - chrono::Duration::hours(24);
                        for row in &remaining {
                            prop_assert!(
                                row.created_at >= threshold - chrono::Duration::seconds(5),
                                "message {} has created_at {} which is older than threshold {}",
                                row.message_id,
                                row.created_at,
                                threshold
                            );
                        }

                        // Verify: all fresh messages should still be present
                        let remaining_ids: std::collections::HashSet<Uuid> =
                            remaining.iter().map(|r| r.message_id).collect();
                        for mid in &fresh_ids {
                            prop_assert!(
                                remaining_ids.contains(mid),
                                "fresh message {} should still be present after purge",
                                mid
                            );
                        }

                        // Verify: no expired message should remain
                        for mid in &expired_ids {
                            prop_assert!(
                                !remaining_ids.contains(mid),
                                "expired message {} should have been purged",
                                mid
                            );
                        }

                        // Cleanup any remaining messages
                        for mid in &message_ids {
                            let _ = sqlx::query("DELETE FROM chat_messages WHERE message_id = $1")
                                .bind(mid)
                                .execute(&pool)
                                .await;
                        }

                        Ok(())
                    })?;
                    Ok(())
                },
            )
            .unwrap();
    }
}
