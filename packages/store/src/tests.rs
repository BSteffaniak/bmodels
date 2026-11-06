use bmodels_model::{ModelInfo, RecordKind, SourceKind};

use super::*;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

fn model(key: &str, status: ModelStatus) -> CanonicalModel {
    CanonicalModel {
        key: key.to_owned(),
        vendor: key.split('/').next().unwrap().to_owned(),
        display_name: key.to_owned(),
        kind: RecordKind::Model,
        first_seen_at: 100,
        created_at: None,
        status,
        acked_at: None,
        note: None,
    }
}

fn listing(source: &str, id: &str, canonical: &str, status: ListingStatus) -> Listing {
    Listing {
        key: ListingKey::new(source, id),
        canonical_key: canonical.to_owned(),
        kind: RecordKind::Model,
        display_name: Some(id.to_owned()),
        created_at: Some(50),
        url: None,
        info: ModelInfo {
            context_window: Some(128_000),
            ..ModelInfo::default()
        },
        first_seen_at: 100,
        removed_at: None,
        baseline: false,
        status,
        acked_at: None,
    }
}

fn event(kind: EventKind, source: &str, id: &str, canonical: &str) -> NewEvent {
    NewEvent {
        at: 100,
        kind,
        canonical_key: canonical.to_owned(),
        listing: ListingKey::new(source, id),
        changes: Vec::new(),
        acked_at: None,
    }
}

/// gpt-x listed by openai and openrouter (both unseen, one `NEW` and one `NEW_LISTING` event),
/// claude-y listed by openrouter only.
async fn seeded() -> Store {
    let store = Store::open_in_memory().await.unwrap();
    store
        .apply_sync(&SyncWrite {
            canonical_models: vec![
                model("openai/gpt-x", ModelStatus::Unseen),
                model("anthropic/claude-y", ModelStatus::Unseen),
            ],
            listings: vec![
                listing("openai", "gpt-x", "openai/gpt-x", ListingStatus::Unseen),
                listing(
                    "openrouter",
                    "openai/gpt-x",
                    "openai/gpt-x",
                    ListingStatus::Unseen,
                ),
                listing(
                    "openrouter",
                    "anthropic/claude-y",
                    "anthropic/claude-y",
                    ListingStatus::Unseen,
                ),
            ],
            events: vec![
                event(EventKind::New, "openai", "gpt-x", "openai/gpt-x"),
                event(
                    EventKind::NewListing,
                    "openrouter",
                    "openai/gpt-x",
                    "openai/gpt-x",
                ),
                event(
                    EventKind::New,
                    "openrouter",
                    "anthropic/claude-y",
                    "anthropic/claude-y",
                ),
            ],
            state: SourceState {
                id: "openrouter".into(),
                kind: Some(SourceKind::Openrouter),
                baseline_at: Some(90),
                last_success_at: Some(100),
                listing_count: 2,
                etag: Some("\"abc\"".into()),
                ..SourceState::default()
            },
            run: Some(SyncRun {
                source: "openrouter".into(),
                started_at: 99,
                finished_at: 100,
                error: None,
                fetched: 2,
                new: 1,
                new_listings: 1,
                changed: 0,
                removed: 0,
            }),
            moved: Vec::new(),
        })
        .await
        .unwrap();
    store
}

fn unacked(store: &Store) -> Vec<Event> {
    block_on(store.events(&EventQuery {
        unacked: true,
        ..EventQuery::default()
    }))
    .unwrap()
}

#[test]
fn round_trips_everything() {
    block_on(async {
        let store = seeded().await;
        let state = store.source_state("openrouter").await.unwrap();
        assert_eq!(state.kind, Some(SourceKind::Openrouter));
        assert_eq!(state.etag.as_deref(), Some("\"abc\""));
        assert_eq!(state.baseline_at, Some(90));
        assert_eq!(store.source_state("never").await.unwrap().id, "never");

        let listings = store.listings_of_source("openrouter").await.unwrap();
        assert_eq!(listings.len(), 2);
        assert_eq!(listings[0].info.context_window, Some(128_000));

        assert_eq!(store.models().await.unwrap().len(), 2);
        assert_eq!(
            store.listings_of_model("openai/gpt-x").await.unwrap().len(),
            2
        );
        let runs = store.sync_runs(Some("openrouter"), 10).await.unwrap();
        assert_eq!(runs[0].new_listings, 1);

        let snapshot = store.snapshot(10).await.unwrap();
        assert_eq!(snapshot.events.len(), 3);
        assert_eq!(snapshot.listings.len(), 3);
    });
}

#[test]
fn acking_a_model_cascades_to_listings_and_events() {
    let store = block_on(seeded());
    let outcome = block_on(store.ack(&AckTarget::Model("openai/gpt-x".into()), 200)).unwrap();
    assert_eq!(
        outcome,
        AckOutcome {
            events: 2,
            listings: 2,
            models: 1
        }
    );
    let left = unacked(&store);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].canonical_key, "anthropic/claude-y");
    let m = block_on(store.model("openai/gpt-x")).unwrap().unwrap();
    assert_eq!((m.status, m.acked_at), (ModelStatus::Seen, Some(200)));
    // Acking again changes nothing.
    assert!(
        block_on(store.ack(&AckTarget::Model("openai/gpt-x".into()), 300))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn acking_the_last_listing_settles_the_model() {
    let store = block_on(seeded());
    let first =
        block_on(store.ack(&AckTarget::Listing(ListingKey::new("openai", "gpt-x")), 200)).unwrap();
    assert_eq!((first.listings, first.events, first.models), (1, 1, 0));
    assert_eq!(
        block_on(store.model("openai/gpt-x"))
            .unwrap()
            .unwrap()
            .status,
        ModelStatus::Unseen
    );
    let second = block_on(store.ack(
        &AckTarget::Listing(ListingKey::new("openrouter", "openai/gpt-x")),
        201,
    ))
    .unwrap();
    assert_eq!((second.listings, second.events, second.models), (1, 1, 1));
    assert_eq!(
        block_on(store.model("openai/gpt-x"))
            .unwrap()
            .unwrap()
            .status,
        ModelStatus::Seen
    );
}

#[test]
fn acking_a_new_event_acks_its_listing() {
    let store = block_on(seeded());
    let id = unacked(&store)
        .into_iter()
        .find(|e| e.canonical_key == "anthropic/claude-y")
        .unwrap()
        .id;
    let outcome = block_on(store.ack(&AckTarget::Event(id), 200)).unwrap();
    assert_eq!(
        (outcome.events, outcome.listings, outcome.models),
        (1, 1, 1)
    );
    assert!(matches!(
        block_on(store.ack(&AckTarget::Event(9_999), 200)),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn ack_all_filters_by_source_and_vendor() {
    let store = block_on(seeded());
    let by_vendor = block_on(store.ack(
        &AckTarget::All {
            source: None,
            vendor: Some("anthropic".into()),
        },
        200,
    ))
    .unwrap();
    assert_eq!(by_vendor.events, 1);
    let by_source = block_on(store.ack(
        &AckTarget::All {
            source: Some("openai".into()),
            vendor: None,
        },
        201,
    ))
    .unwrap();
    assert_eq!(by_source.events, 1);
    let rest = block_on(store.ack(
        &AckTarget::All {
            source: None,
            vendor: None,
        },
        202,
    ))
    .unwrap();
    assert_eq!(rest.events, 1);
    assert_eq!(unacked(&store).len(), 0);
}

#[test]
fn status_changes() {
    let store = block_on(seeded());
    block_on(store.set_status("openai/gpt-x", ModelStatus::Starred, 200)).unwrap();
    let m = block_on(store.model("openai/gpt-x")).unwrap().unwrap();
    assert_eq!(m.status, ModelStatus::Starred);
    assert_eq!(unacked(&store).len(), 1, "starring acknowledges");

    block_on(store.set_status("openai/gpt-x", ModelStatus::Unseen, 300)).unwrap();
    let listings = block_on(store.listings_of_model("openai/gpt-x")).unwrap();
    assert!(listings.iter().all(|l| l.status == ListingStatus::Unseen));

    block_on(store.set_note("openai/gpt-x", Some("try it"))).unwrap();
    assert_eq!(
        block_on(store.model("openai/gpt-x"))
            .unwrap()
            .unwrap()
            .note
            .as_deref(),
        Some("try it")
    );
    assert!(matches!(
        block_on(store.set_status("nope/x", ModelStatus::Seen, 1)),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn moved_listings_carry_events_and_drop_orphans() {
    let store = block_on(seeded());
    let mut moved = listing(
        "openrouter",
        "anthropic/claude-y",
        "anthropic/claude-z",
        ListingStatus::Unseen,
    );
    moved.canonical_key = "anthropic/claude-z".into();
    block_on(store.apply_sync(&SyncWrite {
        canonical_models: vec![model("anthropic/claude-z", ModelStatus::Unseen)],
        listings: vec![moved],
        state: SourceState {
            id: "openrouter".into(),
            ..SourceState::default()
        },
        moved: vec![(
            ListingKey::new("openrouter", "anthropic/claude-y"),
            "anthropic/claude-y".into(),
        )],
        ..SyncWrite::default()
    }))
    .unwrap();
    assert!(
        block_on(store.model("anthropic/claude-y"))
            .unwrap()
            .is_none()
    );
    let events = block_on(store.events(&EventQuery {
        canonical_key: Some("anthropic/claude-z".into()),
        ..EventQuery::default()
    }))
    .unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn notified_marking_and_event_queries() {
    let store = block_on(seeded());
    let all = unacked(&store);
    block_on(store.mark_notified(&[all[0].id], 500)).unwrap();
    let pending = block_on(store.events(&EventQuery {
        unnotified: true,
        ..EventQuery::default()
    }))
    .unwrap();
    assert_eq!(pending.len(), 2);
    let for_model = block_on(store.events(&EventQuery {
        canonical_key: Some("openai/gpt-x".into()),
        ..EventQuery::default()
    }))
    .unwrap();
    assert_eq!(for_model.len(), 2);
}

#[test]
fn reopens_a_file_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("bmodels.sqlite");
    block_on(async {
        let store = Store::open(&path).await.unwrap();
        store
            .record_attempt(
                &SourceState {
                    id: "x".into(),
                    last_error: Some("boom".into()),
                    failures: 2,
                    ..SourceState::default()
                },
                None,
            )
            .await
            .unwrap();
    });
    let store = block_on(Store::open(&path)).unwrap();
    let state = block_on(store.source_state("x")).unwrap();
    assert_eq!(
        (state.failures, state.last_error.as_deref()),
        (2, Some("boom"))
    );
}
