//! Integration tests for the channel-membership domain layer.
//!
//! All tests in this file require a running Postgres instance.
//! Run with: `cargo test --test channel_integration -- --ignored`
//!
//! The DATABASE_URL env var must point to a test database.
//! Tables are truncated between tests for isolation.

use proptest::prelude::*;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use wavis_backend::channel::channel;
use wavis_backend::channel::channel_models::ChannelRole;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Get a test database pool. Reads DATABASE_URL from env,
/// falling back to a local default for convenience.
/// Runs migrations to ensure the schema is current.
async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wavis:wavis@localhost:5432/wavis".to_string());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("Failed to connect to test database");
    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("Failed to run migrations");
    pool
}

/// Truncate all channel-related and auth tables. Call between tests for isolation.
async fn truncate_channel_tables(pool: &PgPool) {
    sqlx::query(
        "TRUNCATE channel_invites, channel_memberships, channels, refresh_tokens, devices, users CASCADE",
    )
    .execute(pool)
    .await
    .expect("Failed to truncate channel tables");
}

/// Register a test device and return its user_id.
/// Uses the device-auth domain function with a deterministic test secret.
const TEST_PEPPER: &[u8] = b"test-pepper-at-least-32-bytes!!!!!!";

async fn register_test_user(pool: &PgPool) -> Uuid {
    let secret = b"test-auth-secret-at-least-32-bytes!!".to_vec();
    let reg = wavis_backend::auth::auth::register_device(pool, &secret, 30, 30, TEST_PEPPER)
        .await
        .expect("register_device failed");
    reg.user_id
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 4: Channel creation invariant
// For any valid user_id and name (1-100 chars), create_channel inserts
// channels + owner membership atomically; owner_user_id matches membership
// user_id.
// Validates: Requirements 1.1, 1.2, 1.3
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop4_channel_creation_invariant() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9 ]{1,50}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let user_id = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, user_id, &name).await.unwrap();

            // (a) channels row exists with correct name and owner
            prop_assert_eq!(&ch.name, &name);
            prop_assert_eq!(ch.owner_user_id, user_id);

            // (b) membership row exists with role=owner
            let row = sqlx::query(
                "SELECT user_id, role FROM channel_memberships WHERE channel_id = $1 AND role = 'owner'"
            )
            .bind(ch.channel_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            let membership_user_id: Uuid = row.get("user_id");
            prop_assert_eq!(membership_user_id, user_id);

            // (c) owner_user_id matches membership user_id
            prop_assert_eq!(ch.owner_user_id, membership_user_id);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 5: Channel listing filter and ordering
// For any user with mixed banned/non-banned memberships, list_channels returns
// exactly non-banned channels ordered by joined_at DESC.
// Validates: Requirements 2.1, 2.2, 2.3
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop5_channel_listing_filter_and_ordering() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(
        name1 in "[a-zA-Z0-9]{1,20}",
        name2 in "[a-zA-Z0-9]{1,20}",
        name3 in "[a-zA-Z0-9]{1,20}",
    )| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner_id = register_test_user(&pool).await;
            let joiner_id = register_test_user(&pool).await;

            // Create 3 channels owned by owner
            let ch1 = channel::create_channel(&pool, owner_id, &name1).await.unwrap();
            let ch2 = channel::create_channel(&pool, owner_id, &name2).await.unwrap();
            let ch3 = channel::create_channel(&pool, owner_id, &name3).await.unwrap();

            // Create invites and have joiner join all 3
            let inv1 = channel::create_invite(&pool, ch1.channel_id, owner_id, None, None).await.unwrap();
            let inv2 = channel::create_invite(&pool, ch2.channel_id, owner_id, None, None).await.unwrap();
            let inv3 = channel::create_invite(&pool, ch3.channel_id, owner_id, None, None).await.unwrap();

            channel::join_channel_by_invite(&pool, joiner_id, &inv1.code).await.unwrap();
            // Small delay to ensure different joined_at timestamps
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            channel::join_channel_by_invite(&pool, joiner_id, &inv2.code).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            channel::join_channel_by_invite(&pool, joiner_id, &inv3.code).await.unwrap();

            // Ban joiner from ch2
            channel::ban_member(&pool, ch2.channel_id, owner_id, joiner_id).await.unwrap();

            // List channels for joiner — should see ch1 and ch3 (not ch2), ordered by joined_at DESC
            let list = channel::list_channels(&pool, joiner_id).await.unwrap();
            prop_assert_eq!(list.len(), 2);
            // ch3 was joined last, so it should be first (DESC order)
            prop_assert_eq!(list[0].channel_id, ch3.channel_id);
            prop_assert_eq!(list[1].channel_id, ch1.channel_id);
            // Verify roles
            prop_assert_eq!(list[0].role, ChannelRole::Member);
            prop_assert_eq!(list[1].role, ChannelRole::Member);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 6: Channel detail non-banned member filter
// For any channel with mixed banned/non-banned members, get_channel_detail
// returns exactly non-banned members.
// Validates: Requirements 3.1, 3.2
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop6_channel_detail_non_banned_member_filter() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let m1 = register_test_user(&pool).await;
            let m2 = register_test_user(&pool).await;
            let m3 = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();

            channel::join_channel_by_invite(&pool, m1, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, m2, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, m3, &inv.code).await.unwrap();

            // Ban m2
            channel::ban_member(&pool, ch.channel_id, owner, m2).await.unwrap();

            let detail = channel::get_channel_detail(&pool, ch.channel_id, owner).await.unwrap();
            let member_ids: Vec<Uuid> = detail.members.iter().map(|m| m.user_id).collect();

            // Should contain owner, m1, m3 but NOT m2
            prop_assert_eq!(detail.members.len(), 3);
            prop_assert!(member_ids.contains(&owner));
            prop_assert!(member_ids.contains(&m1));
            prop_assert!(member_ids.contains(&m3));
            prop_assert!(!member_ids.contains(&m2));

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 7: Channel deletion cascades
// Owner delete removes channel + all memberships + all invites; non-owner
// gets Forbidden and channel unchanged.
// Validates: Requirements 4.1, 4.2, 4.3
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop7_channel_deletion_cascades() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let member = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();
            channel::join_channel_by_invite(&pool, member, &inv.code).await.unwrap();

            // Non-owner cannot delete
            let err = channel::delete_channel(&pool, ch.channel_id, member).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::Forbidden)));

            // Owner deletes
            channel::delete_channel(&pool, ch.channel_id, owner).await.unwrap();

            // Verify all rows gone
            let ch_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channels WHERE channel_id = $1")
                .bind(ch.channel_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(ch_count.0, 0);

            let mem_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channel_memberships WHERE channel_id = $1")
                .bind(ch.channel_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(mem_count.0, 0);

            let inv_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channel_invites WHERE channel_id = $1")
                .bind(ch.channel_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(inv_count.0, 0);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 9: Invite creation authorization
// Owner/admin succeed; member/non-member get Forbidden.
// Validates: Requirements 5.3, 5.5
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop9_invite_creation_authorization() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let admin_user = register_test_user(&pool).await;
            let member_user = register_test_user(&pool).await;
            let outsider = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();

            // Join admin and member
            channel::join_channel_by_invite(&pool, admin_user, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, member_user, &inv.code).await.unwrap();

            // Promote admin_user to admin
            channel::change_role(&pool, ch.channel_id, owner, admin_user, "admin").await.unwrap();

            // Owner can create invite
            prop_assert!(channel::create_invite(&pool, ch.channel_id, owner, None, None).await.is_ok());

            // Admin can create invite
            prop_assert!(channel::create_invite(&pool, ch.channel_id, admin_user, None, None).await.is_ok());

            // Member cannot create invite
            let err = channel::create_invite(&pool, ch.channel_id, member_user, None, None).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::Forbidden)));

            // Non-member cannot create invite
            let err = channel::create_invite(&pool, ch.channel_id, outsider, None, None).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::Forbidden)));

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 10: Join via invite creates membership
// and increments uses.
// Valid invite + non-member + not banned → membership created, uses +1.
// Validates: Requirements 6.1, 6.2
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop10_join_creates_membership_and_increments_uses() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let joiner = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();

            // Check uses before
            let before: (i32,) = sqlx::query_as("SELECT uses FROM channel_invites WHERE code = $1")
                .bind(&inv.code)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(before.0, 0);

            let result = channel::join_channel_by_invite(&pool, joiner, &inv.code).await.unwrap();
            prop_assert_eq!(result.channel_id, ch.channel_id);
            prop_assert_eq!(result.role, ChannelRole::Member);

            // Check uses after
            let after: (i32,) = sqlx::query_as("SELECT uses FROM channel_invites WHERE code = $1")
                .bind(&inv.code)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(after.0, 1);

            // Verify membership exists
            let mem_count: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM channel_memberships WHERE channel_id = $1 AND user_id = $2"
            )
            .bind(ch.channel_id)
            .bind(joiner)
            .fetch_one(&pool)
            .await
            .unwrap();
            prop_assert_eq!(mem_count.0, 1);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 11: Ban overrides invite validity
// Banned user + valid invite → Banned error, uses not incremented.
// Validates: Requirements 6.5
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop11_ban_overrides_invite_validity() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let target = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();

            // Join then get banned
            channel::join_channel_by_invite(&pool, target, &inv.code).await.unwrap();
            channel::ban_member(&pool, ch.channel_id, owner, target).await.unwrap();

            // Create a new invite for the banned user to try
            let inv2 = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();
            let before2: (i32,) = sqlx::query_as("SELECT uses FROM channel_invites WHERE code = $1")
                .bind(&inv2.code)
                .fetch_one(&pool)
                .await
                .unwrap();

            let err = channel::join_channel_by_invite(&pool, target, &inv2.code).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::Banned)));

            // Uses should NOT be incremented (tx rolled back on early return)
            let after2: (i32,) = sqlx::query_as("SELECT uses FROM channel_invites WHERE code = $1")
                .bind(&inv2.code)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(before2.0, after2.0);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 12: Join idempotency rejection
// Already-member + valid invite → AlreadyMember, uses not incremented.
// Validates: Requirements 6.4
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop12_join_idempotency_rejection() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let joiner = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();

            // First join succeeds
            channel::join_channel_by_invite(&pool, joiner, &inv.code).await.unwrap();

            // Record uses
            let before: (i32,) = sqlx::query_as("SELECT uses FROM channel_invites WHERE code = $1")
                .bind(&inv.code)
                .fetch_one(&pool)
                .await
                .unwrap();

            // Second join with same invite → AlreadyMember
            let err = channel::join_channel_by_invite(&pool, joiner, &inv.code).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::AlreadyMember)));

            // Uses should NOT be incremented (tx rolled back on early return)
            let after: (i32,) = sqlx::query_as("SELECT uses FROM channel_invites WHERE code = $1")
                .bind(&inv.code)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(before.0, after.0);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 13: Leave channel rules
// Non-owner non-banned → membership deleted; owner → OwnerCannotLeave;
// banned → error (Banned), membership preserved.
// Validates: Requirements 7.1, 7.3
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop13_leave_channel_rules() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let member = register_test_user(&pool).await;
            let banned_user = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();
            channel::join_channel_by_invite(&pool, member, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, banned_user, &inv.code).await.unwrap();
            channel::ban_member(&pool, ch.channel_id, owner, banned_user).await.unwrap();

            // Owner cannot leave
            let err = channel::leave_channel(&pool, ch.channel_id, owner).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::OwnerCannotLeave)));

            // Banned user cannot leave
            let err = channel::leave_channel(&pool, ch.channel_id, banned_user).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::Banned)));
            // Verify banned membership still exists
            let count: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM channel_memberships WHERE channel_id = $1 AND user_id = $2"
            )
            .bind(ch.channel_id)
            .bind(banned_user)
            .fetch_one(&pool)
            .await
            .unwrap();
            prop_assert_eq!(count.0, 1);

            // Normal member can leave
            channel::leave_channel(&pool, ch.channel_id, member).await.unwrap();
            let count: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM channel_memberships WHERE channel_id = $1 AND user_id = $2"
            )
            .bind(ch.channel_id)
            .bind(member)
            .fetch_one(&pool)
            .await
            .unwrap();
            prop_assert_eq!(count.0, 0);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 14: Ban/unban round-trip
// Ban sets banned_at, unban clears it, role preserved after round-trip.
// Validates: Requirements 9.1, 10.1
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop14_ban_unban_round_trip() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let target = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();
            channel::join_channel_by_invite(&pool, target, &inv.code).await.unwrap();

            // Promote to admin to test role preservation
            channel::change_role(&pool, ch.channel_id, owner, target, "admin").await.unwrap();

            // Ban
            let ban_result = channel::ban_member(&pool, ch.channel_id, owner, target).await.unwrap();
            prop_assert_eq!(ban_result.user_id, target);

            // Verify banned_at is set
            let row = sqlx::query("SELECT banned_at, role FROM channel_memberships WHERE channel_id = $1 AND user_id = $2")
                .bind(ch.channel_id)
                .bind(target)
                .fetch_one(&pool)
                .await
                .unwrap();
            let banned_at: Option<chrono::DateTime<chrono::Utc>> = row.get("banned_at");
            prop_assert!(banned_at.is_some());

            // Unban
            channel::unban_member(&pool, ch.channel_id, owner, target).await.unwrap();

            // Verify banned_at is cleared and role preserved
            let row = sqlx::query("SELECT banned_at, role FROM channel_memberships WHERE channel_id = $1 AND user_id = $2")
                .bind(ch.channel_id)
                .bind(target)
                .fetch_one(&pool)
                .await
                .unwrap();
            let banned_at: Option<chrono::DateTime<chrono::Utc>> = row.get("banned_at");
            let role: String = row.get("role");
            prop_assert!(banned_at.is_none());
            prop_assert_eq!(role, "admin");

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 15: Ban authorization hierarchy
// Owner can ban admin/member; admin can ban member but not admin;
// member cannot ban; self-ban rejected; owner cannot be banned.
// Validates: Requirements 9.3, 9.4, 9.5, 9.9
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop15_ban_authorization_hierarchy() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let admin1 = register_test_user(&pool).await;
            let admin2 = register_test_user(&pool).await;
            let member1 = register_test_user(&pool).await;
            let member2 = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();
            channel::join_channel_by_invite(&pool, admin1, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, admin2, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, member1, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, member2, &inv.code).await.unwrap();

            channel::change_role(&pool, ch.channel_id, owner, admin1, "admin").await.unwrap();
            channel::change_role(&pool, ch.channel_id, owner, admin2, "admin").await.unwrap();

            // (e) Self-ban rejected (test early — non-destructive)
            let err = channel::ban_member(&pool, ch.channel_id, owner, owner).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::SelfBan)));

            // (f) Owner cannot be banned
            let err = channel::ban_member(&pool, ch.channel_id, admin2, owner).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::CannotBanOwner)));

            // (c) Admin cannot ban another admin (must test BEFORE owner bans admin1,
            //     otherwise AlreadyBanned would be returned instead of InsufficientPrivileges)
            let err = channel::ban_member(&pool, ch.channel_id, admin2, admin1).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::InsufficientPrivileges)));

            // (d) Member cannot ban anyone
            let err = channel::ban_member(&pool, ch.channel_id, member2, member1).await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::Forbidden)));

            // (b) Admin can ban member
            prop_assert!(channel::ban_member(&pool, ch.channel_id, admin2, member1).await.is_ok());

            // (a) Owner can ban admin
            prop_assert!(channel::ban_member(&pool, ch.channel_id, owner, admin1).await.is_ok());

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 16: Role management constraints
// Owner can set admin/member; role='owner' rejected; cannot change owner's
// own role; cannot change banned member's role; non-owner gets Forbidden.
// Validates: Requirements 11.1, 11.3, 11.4, 11.5, 11.7
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop16_role_management_constraints() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let target = register_test_user(&pool).await;
            let non_owner = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();
            channel::join_channel_by_invite(&pool, target, &inv.code).await.unwrap();
            channel::join_channel_by_invite(&pool, non_owner, &inv.code).await.unwrap();

            // (a) Owner can set member to admin
            prop_assert!(channel::change_role(&pool, ch.channel_id, owner, target, "admin").await.is_ok());

            // (a) Owner can set admin back to member
            prop_assert!(channel::change_role(&pool, ch.channel_id, owner, target, "member").await.is_ok());

            // (b) role='owner' rejected
            let err = channel::change_role(&pool, ch.channel_id, owner, target, "owner").await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::InvalidRole)));

            // (c) Cannot change owner's own role
            let err = channel::change_role(&pool, ch.channel_id, owner, owner, "admin").await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::CannotChangeOwnerRole)));

            // (d) Cannot change banned member's role
            channel::ban_member(&pool, ch.channel_id, owner, target).await.unwrap();
            let err = channel::change_role(&pool, ch.channel_id, owner, target, "admin").await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::CannotChangeBannedRole)));

            // (e) Non-owner gets Forbidden
            let err = channel::change_role(&pool, ch.channel_id, non_owner, target, "admin").await;
            prop_assert!(matches!(err, Err(wavis_backend::channel::channel_models::ChannelError::Forbidden)));

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 17: Expired invite sweep
// Deletes exactly invites with non-null expires_at in the past; future and
// NULL expires_at preserved.
// Validates: Requirements 14.1
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop17_expired_invite_sweep() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();

            // Insert invites with specific expires_at values via raw SQL
            // 1. Expired (1 hour ago)
            let code_expired = channel::generate_invite_code();
            sqlx::query("INSERT INTO channel_invites (code, channel_id, expires_at) VALUES ($1, $2, now() - interval '1 hour')")
                .bind(&code_expired)
                .bind(ch.channel_id)
                .execute(&pool)
                .await
                .unwrap();

            // 2. Future (1 hour from now)
            let code_future = channel::generate_invite_code();
            sqlx::query("INSERT INTO channel_invites (code, channel_id, expires_at) VALUES ($1, $2, now() + interval '1 hour')")
                .bind(&code_future)
                .bind(ch.channel_id)
                .execute(&pool)
                .await
                .unwrap();

            // 3. NULL expires_at (never expires)
            let code_null = channel::generate_invite_code();
            sqlx::query("INSERT INTO channel_invites (code, channel_id, expires_at) VALUES ($1, $2, NULL)")
                .bind(&code_null)
                .bind(ch.channel_id)
                .execute(&pool)
                .await
                .unwrap();

            // Sweep
            let count = channel::sweep_expired_invites(&pool).await.unwrap();
            prop_assert_eq!(count, 1); // only the expired one

            // Verify expired is gone
            let expired_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channel_invites WHERE code = $1")
                .bind(&code_expired)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(expired_count.0, 0);

            // Verify future and null remain
            let future_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channel_invites WHERE code = $1")
                .bind(&code_future)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(future_count.0, 1);

            let null_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channel_invites WHERE code = $1")
                .bind(&code_null)
                .fetch_one(&pool)
                .await
                .unwrap();
            prop_assert_eq!(null_count.0, 1);

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 18: Opaque error uniformity
// Non-member access, non-existent channel, banned user access all produce
// only opaque domain errors (NotMember, Banned, ChannelNotFound, Forbidden)
// whose Display strings don't contain "ChannelError" variant names.
// Validates: Requirements 15.1
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop18_opaque_error_uniformity() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let outsider = register_test_user(&pool).await;
            let banned_user = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();
            let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None).await.unwrap();
            channel::join_channel_by_invite(&pool, banned_user, &inv.code).await.unwrap();
            channel::ban_member(&pool, ch.channel_id, owner, banned_user).await.unwrap();

            // Non-member accessing channel detail → NotMember (maps to opaque "forbidden")
            let err = channel::get_channel_detail(&pool, ch.channel_id, outsider).await.unwrap_err();
            prop_assert!(matches!(err, wavis_backend::channel::channel_models::ChannelError::NotMember));

            // Non-existent channel → NotMember (membership query returns no rows first)
            let fake_id = Uuid::new_v4();
            let err = channel::get_channel_detail(&pool, fake_id, owner).await.unwrap_err();
            prop_assert!(matches!(
                err,
                wavis_backend::channel::channel_models::ChannelError::NotMember
                    | wavis_backend::channel::channel_models::ChannelError::ChannelNotFound
            ));

            // Banned user accessing channel detail → Banned (maps to opaque "forbidden")
            let err = channel::get_channel_detail(&pool, ch.channel_id, banned_user).await.unwrap_err();
            prop_assert!(matches!(err, wavis_backend::channel::channel_models::ChannelError::Banned));

            // Verify error Display strings don't contain "ChannelError" — the handler
            // maps these to opaque messages, and the domain errors themselves should
            // not leak variant names in their Display output.
            let opaque_variants = [
                wavis_backend::channel::channel_models::ChannelError::NotMember,
                wavis_backend::channel::channel_models::ChannelError::Banned,
                wavis_backend::channel::channel_models::ChannelError::ChannelNotFound,
                wavis_backend::channel::channel_models::ChannelError::Forbidden,
            ];
            for variant in &opaque_variants {
                let msg = format!("{variant}");
                prop_assert!(!msg.contains("ChannelError"));
            }

            Ok(())
        })?;
    });
}

// ---------------------------------------------------------------------------
// Feature: channel-membership, Property 19: Owner consistency invariant
// channels.owner_user_id must equal the owner membership user_id; mismatch
// returns OwnerConsistencyViolation.
// Validates: Requirements 12.7
// ---------------------------------------------------------------------------

#[test]
#[ignore] // requires Postgres — run with: cargo test --test channel_integration -- --ignored
fn prop19_owner_consistency_invariant() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = rt.block_on(test_pool());

    proptest!(ProptestConfig::with_cases(32), |(name in "[a-zA-Z0-9]{1,20}")| {
        rt.block_on(async {
            truncate_channel_tables(&pool).await;
            let owner = register_test_user(&pool).await;
            let other_user = register_test_user(&pool).await;

            let ch = channel::create_channel(&pool, owner, &name).await.unwrap();

            // Manually corrupt: set channels.owner_user_id to a different user
            sqlx::query("UPDATE channels SET owner_user_id = $1 WHERE channel_id = $2")
                .bind(other_user)
                .bind(ch.channel_id)
                .execute(&pool)
                .await
                .unwrap();

            // get_channel_detail should detect the mismatch
            // (owner is still a member, so membership check passes)
            let err = channel::get_channel_detail(&pool, ch.channel_id, owner).await.unwrap_err();
            prop_assert!(matches!(
                err,
                wavis_backend::channel::channel_models::ChannelError::OwnerConsistencyViolation
            ));

            Ok(())
        })?;
    });
}

// ===========================================================================
// Example-based integration tests
// ===========================================================================

// ---------------------------------------------------------------------------
// Example: concurrent join race — two tasks, same user+channel, one wins
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore] // requires Postgres
async fn example_concurrent_join_race() {
    let pool = test_pool().await;
    truncate_channel_tables(&pool).await;
    let owner = register_test_user(&pool).await;
    let joiner = register_test_user(&pool).await;

    let ch = channel::create_channel(&pool, owner, "race-test")
        .await
        .unwrap();
    let inv = channel::create_invite(&pool, ch.channel_id, owner, None, Some(1))
        .await
        .unwrap();

    // Two concurrent join attempts for the same user — one should succeed, one should fail
    let pool2 = pool.clone();
    let code = inv.code.clone();
    let handle1 =
        tokio::spawn(async move { channel::join_channel_by_invite(&pool2, joiner, &code).await });
    // Small delay to avoid exact same timing
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let result_direct = channel::join_channel_by_invite(&pool, joiner, &inv.code).await;
    let result_spawned = handle1.await.unwrap();

    // One should succeed, the other should fail (AlreadyMember since same user)
    let successes = [&result_direct, &result_spawned]
        .iter()
        .filter(|r| r.is_ok())
        .count();
    let failures = [&result_direct, &result_spawned]
        .iter()
        .filter(|r| r.is_err())
        .count();
    // At least one succeeds (the first to commit), the other fails
    assert!(successes >= 1);
    assert_eq!(successes + failures, 2);
}

// ---------------------------------------------------------------------------
// Example: CASCADE delete removes memberships and invites
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore] // requires Postgres
async fn example_cascade_delete() {
    let pool = test_pool().await;
    truncate_channel_tables(&pool).await;
    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;

    let ch = channel::create_channel(&pool, owner, "cascade-test")
        .await
        .unwrap();
    let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None)
        .await
        .unwrap();
    channel::join_channel_by_invite(&pool, member, &inv.code)
        .await
        .unwrap();

    // Create additional invites
    channel::create_invite(&pool, ch.channel_id, owner, None, None)
        .await
        .unwrap();

    channel::delete_channel(&pool, ch.channel_id, owner)
        .await
        .unwrap();

    // All related rows should be gone
    let mem: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM channel_memberships WHERE channel_id = $1")
            .bind(ch.channel_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mem.0, 0);

    let invs: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM channel_invites WHERE channel_id = $1")
        .bind(ch.channel_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(invs.0, 0);
}

// ---------------------------------------------------------------------------
// Example: partial unique index rejects second owner
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore] // requires Postgres
async fn example_partial_unique_index_rejects_second_owner() {
    let pool = test_pool().await;
    truncate_channel_tables(&pool).await;
    let owner = register_test_user(&pool).await;
    let other = register_test_user(&pool).await;

    let ch = channel::create_channel(&pool, owner, "unique-owner-test")
        .await
        .unwrap();

    // Try to manually insert a second owner — should fail due to partial unique index
    let result = sqlx::query(
        "INSERT INTO channel_memberships (channel_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(ch.channel_id)
    .bind(other)
    .execute(&pool)
    .await;

    assert!(
        result.is_err(),
        "Partial unique index should reject second owner"
    );
}

// ---------------------------------------------------------------------------
// Example: banned member cannot leave
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore] // requires Postgres
async fn example_banned_member_cannot_leave() {
    let pool = test_pool().await;
    truncate_channel_tables(&pool).await;
    let owner = register_test_user(&pool).await;
    let member = register_test_user(&pool).await;

    let ch = channel::create_channel(&pool, owner, "ban-leave-test")
        .await
        .unwrap();
    let inv = channel::create_invite(&pool, ch.channel_id, owner, None, None)
        .await
        .unwrap();
    channel::join_channel_by_invite(&pool, member, &inv.code)
        .await
        .unwrap();
    channel::ban_member(&pool, ch.channel_id, owner, member)
        .await
        .unwrap();

    let err = channel::leave_channel(&pool, ch.channel_id, member)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        wavis_backend::channel::channel_models::ChannelError::Banned
    ));
}

// ---------------------------------------------------------------------------
// Example: expired invite rejected on join
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore] // requires Postgres
async fn example_expired_invite_rejected() {
    let pool = test_pool().await;
    truncate_channel_tables(&pool).await;
    let owner = register_test_user(&pool).await;
    let joiner = register_test_user(&pool).await;

    let ch = channel::create_channel(&pool, owner, "expired-invite-test")
        .await
        .unwrap();

    // Insert an already-expired invite via raw SQL
    let code = channel::generate_invite_code();
    sqlx::query("INSERT INTO channel_invites (code, channel_id, expires_at) VALUES ($1, $2, now() - interval '1 hour')")
        .bind(&code)
        .bind(ch.channel_id)
        .execute(&pool)
        .await
        .unwrap();

    let err = channel::join_channel_by_invite(&pool, joiner, &code)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        wavis_backend::channel::channel_models::ChannelError::InvalidInvite
    ));
}

// ---------------------------------------------------------------------------
// Example: max_uses exhausted invite rejected on join
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore] // requires Postgres
async fn example_max_uses_exhausted_invite_rejected() {
    let pool = test_pool().await;
    truncate_channel_tables(&pool).await;
    let owner = register_test_user(&pool).await;
    let joiner1 = register_test_user(&pool).await;
    let joiner2 = register_test_user(&pool).await;

    let ch = channel::create_channel(&pool, owner, "max-uses-test")
        .await
        .unwrap();
    let inv = channel::create_invite(&pool, ch.channel_id, owner, None, Some(1))
        .await
        .unwrap();

    // First join succeeds
    channel::join_channel_by_invite(&pool, joiner1, &inv.code)
        .await
        .unwrap();

    // Second join fails — max_uses exhausted
    let err = channel::join_channel_by_invite(&pool, joiner2, &inv.code)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        wavis_backend::channel::channel_models::ChannelError::InvalidInvite
    ));
}
