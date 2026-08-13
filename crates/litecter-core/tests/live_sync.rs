//! End-to-end sync against a real backend.
//!
//! Ignored by default so `cargo test` stays offline and deterministic. Run it
//! when touching the wire format, which unit tests can't cover.
//!
//! Litecter runs no sync service, so the test needs a backend to point at. The
//! cheapest one is local, and it exercises the same code path as a deployed
//! Worker:
//!
//! ```bash
//! cd worker && cp wrangler.toml.example wrangler.toml
//! npx wrangler dev --port 8787 &
//!
//! # The backend authenticates against a token derived from the key, so the
//! # two have to be set together.
//! KEY=$(litecter sync key)                # or `litecter sync setup` on a fresh db
//! LITECTER_TEST_ENDPOINT=http://localhost:8787 LITECTER_TEST_KEY="$KEY" \
//!   cargo test -p litecter-core --test live_sync -- --ignored --nocapture
//! ```
//!
//! Without `LITECTER_TEST_KEY` the test generates a fresh one and prints the
//! token the backend would need — useful against a throwaway deployment that
//! accepts anything, and self-explanatory when it 401s.

use litecter_core::store::Store;
use litecter_core::sync::{self, SyncKey};
use litecter_core::{Schedule, textproc};

/// Where to sync. No default: an invented endpoint would either be a server
/// somebody pays for or an address that 404s halfway through the assertions.
fn endpoint() -> String {
    std::env::var("LITECTER_TEST_ENDPOINT").expect(
        "set LITECTER_TEST_ENDPOINT to a backend to test against \
         (see the module docs for a local `wrangler dev` recipe)",
    )
}

/// The key to sync with. Reused from the environment when the backend already
/// has a matching `SYNC_TOKEN`, otherwise fresh — in which case the test writes
/// its own object and touches nothing belonging to anyone else.
fn test_key() -> SyncKey {
    match std::env::var("LITECTER_TEST_KEY") {
        Ok(raw) => SyncKey::decode(&raw).expect("LITECTER_TEST_KEY is not a sync key"),
        Err(_) => SyncKey::generate().unwrap(),
    }
}

/// A store already pointed at the test backend.
fn connected(key: &SyncKey, now: i64) -> Store {
    let store = Store::open_in_memory().unwrap();
    sync::save_key(&store, key, now).unwrap();
    sync::save_endpoint(&store, &endpoint(), now).unwrap();
    store
}

/// Two databases sharing one key: the disk-loss scenario, including the inbox.
#[tokio::test]
#[ignore = "hits the network"]
async fn a_watch_list_and_its_inbox_survive_a_new_machine() {
    let key = test_key();
    let now = 1_800_000_000;
    eprintln!("SYNC_TOKEN this run expects: {}", key.auth_token());

    // --- the machine that is about to die ---
    let old = connected(&key, now);

    let watched = old.add_url("https://example.com/pricing", Schedule::Daily, None, now).unwrap();
    old.add_url("https://example.com/changelog", Schedule::Weekly, None, now).unwrap();

    // An unreviewed change, with the two snapshots its diff needs.
    let before = "Pro plan $20 per month";
    let after = "Pro plan $25 per month";
    let from_id = old.insert_snapshot(watched.id, &textproc::hash(before), before, now).unwrap();
    let to_id = old.insert_snapshot(watched.id, &textproc::hash(after), after, now).unwrap();
    old.set_last_seen(watched.id, from_id).unwrap();
    old.record_change(watched.id, from_id, to_id, 1, 1, Some("Pro plan $25 per month"), now).unwrap();
    old.update_check_ok(watched.id, Some("Pricing"), now, now + 86_400).unwrap();

    let pushed = sync::sync_now(&old, now).await.expect("first sync");
    assert_eq!(pushed.urls_in_document, 2);
    assert_eq!(pushed.diffs_dropped_for_size, 0);

    // --- the replacement machine: nothing but the connection ---
    let new = connected(&key, now);
    let pulled = sync::sync_now(&new, now + 60).await.expect("restore");

    assert_eq!(pulled.stats.added, 2, "watch list restored");
    assert_eq!(pulled.stats.pendings_restored, 1, "inbox restored");

    let restored = new.resolve_url("https://example.com/pricing").unwrap().unwrap();
    assert_eq!(restored.schedule, Schedule::Daily);
    assert_eq!(restored.title.as_deref(), Some("Pricing"));

    let inbox = new.list_changes(true).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(new.snapshot_text(inbox[0].from_snapshot_id).unwrap(), before);
    assert_eq!(new.snapshot_text(inbox[0].to_snapshot_id).unwrap(), after);

    // --- reviewing on the new machine settles it on the old one ---
    new.mark_seen(inbox[0].id, now + 120).unwrap();
    sync::sync_now(&new, now + 130).await.expect("push the review");
    sync::sync_now(&old, now + 140).await.expect("pull the review");
    assert_eq!(old.count_unseen().unwrap(), 0, "reviewing propagates");

    // --- and a delete propagates rather than being resurrected ---
    old.remove_url(watched.id, now + 200).unwrap();
    sync::sync_now(&old, now + 210).await.expect("push the delete");
    sync::sync_now(&new, now + 220).await.expect("pull the delete");
    assert!(
        new.resolve_url("https://example.com/pricing").unwrap().is_none(),
        "the delete must not be undone by the machine that still had it"
    );
    assert_eq!(new.list_urls().unwrap().len(), 1);
}
