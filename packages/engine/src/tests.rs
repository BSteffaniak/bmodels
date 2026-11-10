//! End-to-end sync tests: fake HTTP, in-memory store.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use bmodels_model::{
    AckTarget, Config, EventKind, ModelStatus, Secret, SecretName, SecretRef, SecretValue,
    SourceKind, SourceSpec,
};
use bmodels_sources::testing::{FakeHttp, block_on};
use bmodels_sources::{Response, SecretLookup, SourceError};
use bmodels_store::{EventQuery, Store};

use super::*;

static CLOCK: AtomicI64 = AtomicI64::new(1_000_000);

fn clock() -> i64 {
    CLOCK.load(Ordering::SeqCst)
}

fn advance(secs: i64) {
    CLOCK.fetch_add(secs, Ordering::SeqCst);
}

struct Env(BTreeMap<&'static str, &'static str>);

impl SecretLookup for Env {
    fn var(&self, name: &str) -> Option<String> {
        self.0.get(name).map(|v| (*v).to_owned())
    }
}

const ROUTER: &str = "https://router.test/models";
const OPENAI: &str = "https://openai.test/v1/models";

fn config() -> Config {
    let mut config = Config::default();
    let mut router = SourceSpec::new(SourceKind::Openrouter);
    router.url = Some(ROUTER.into());
    config.sources.insert("router".into(), router);
    let mut openai = SourceSpec::new(SourceKind::OpenaiCompatible);
    openai.url = Some("https://openai.test/v1".into());
    openai.vendor = Some("openai".into());
    openai.api_key_env = Some("OPENAI_KEY".into());
    config.sources.insert("openai".into(), openai);
    let mut locked = SourceSpec::new(SourceKind::Anthropic);
    locked.api_key_env = Some("MISSING_KEY".into());
    config.sources.insert("locked".into(), locked);
    config
}

fn router_body(ids: &[&str]) -> String {
    let data: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"id":"{id}","name":"Vendor: {id}","pricing":{{"prompt":"0.000001","completion":"0.000002"}}}}"#))
        .collect();
    format!(r#"{{"data":[{}]}}"#, data.join(","))
}

fn openai_body(ids: &[&str]) -> String {
    let data: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"id":"{id}","object":"model","created":1}}"#))
        .collect();
    format!(r#"{{"data":[{}]}}"#, data.join(","))
}

fn run(engine: &Engine<'_>, selection: &Selection) -> SyncReport {
    let seen = Mutex::new(Vec::new());
    let report = block_on(engine.sync(selection, &|p| {
        if let Progress::Finished(r) = p {
            seen.lock().unwrap().push(r.source.clone());
        }
    }))
    .unwrap();
    assert_eq!(seen.into_inner().unwrap().len(), report.sources.len());
    report
}

fn outcome<'a>(report: &'a SyncReport, id: &str) -> &'a SourceOutcome {
    &report
        .sources
        .iter()
        .find(|s| s.source == id)
        .unwrap()
        .outcome
}

#[test]
fn baseline_then_new_models_then_ack() {
    let config = config();
    let store = block_on(Store::open_in_memory()).unwrap();
    let http = FakeHttp::new()
        .route(
            ROUTER,
            router_body(&["openai/gpt-4o", "anthropic/claude-x"]),
        )
        .route(OPENAI, openai_body(&["gpt-4o", "gpt-4o-2024-08-06"]));
    let env = Env(BTreeMap::from([("OPENAI_KEY", "sk-test")]));
    let engine = Engine::new(&config, &store, &http, &env).with_clock(clock);

    // 1. Baseline: nothing is new; the keyless source is skipped, not failed.
    let report = run(&engine, &Selection::Enabled);
    assert!(
        matches!(outcome(&report, "router"), SourceOutcome::Synced(c) if c.baseline == 2 && c.new == 0)
    );
    assert!(matches!(outcome(&report, "openai"), SourceOutcome::Synced(c) if c.baseline == 2));
    assert_eq!(
        outcome(&report, "locked"),
        &SourceOutcome::Skipped(
            "needs an API key: run `bmodels auth login locked` or set MISSING_KEY".into()
        )
    );
    assert_eq!(
        block_on(store.models()).unwrap().len(),
        2,
        "gpt-4o merged across sources"
    );
    let auth = http
        .requests()
        .into_iter()
        .find(|r| r.url == OPENAI)
        .unwrap();
    assert!(
        auth.headers
            .contains(&("authorization".into(), "Bearer sk-test".into()))
    );
    let locked = block_on(store.source_state("locked")).unwrap();
    assert!(
        locked
            .skipped
            .as_deref()
            .is_some_and(|s| s.contains("bmodels auth login locked"))
    );

    // 2. A new model appears on both sources.
    advance(3_600);
    http.set(
        ROUTER,
        Ok(Response::ok(router_body(&[
            "openai/gpt-4o",
            "anthropic/claude-x",
            "openai/gpt-6",
        ]))),
    );
    http.set(
        OPENAI,
        Ok(Response::ok(openai_body(&[
            "gpt-4o",
            "gpt-4o-2024-08-06",
            "gpt-6",
        ]))),
    );
    let report = run(
        &engine,
        &Selection::Ids(vec!["openai".into(), "router".into()]),
    );
    let totals = report.totals();
    assert_eq!((totals.new, totals.new_listings), (1, 1));
    let inbox = block_on(store.events(&EventQuery {
        unacked: true,
        ..EventQuery::default()
    }))
    .unwrap();
    assert_eq!(inbox.len(), 1, "the second listing is pre-acknowledged");
    assert_eq!(inbox[0].kind, EventKind::New);
    assert_eq!(inbox[0].canonical_key, "openai/gpt-6");

    // 3. Notices: the NEW event, once.
    let pending = block_on(notices::pending(&config, &store, clock())).unwrap();
    assert_eq!(pending.notices.len(), 1);
    assert_eq!(pending.ids.len(), 2);
    block_on(store.mark_notified(&pending.ids, clock())).unwrap();
    assert_eq!(
        block_on(notices::pending(&config, &store, clock()))
            .unwrap()
            .ids,
        Vec::<i64>::new()
    );

    // 4. Acking the model clears the inbox.
    block_on(store.ack(&AckTarget::Model("openai/gpt-6".into()), clock())).unwrap();
    let model = block_on(store.model("openai/gpt-6")).unwrap().unwrap();
    assert_eq!(model.status, ModelStatus::Seen);
    assert_eq!(
        block_on(store.events(&EventQuery {
            unacked: true,
            ..EventQuery::default()
        }))
        .unwrap(),
        Vec::new()
    );
}

#[test]
fn failures_back_off_and_recover_with_conditional_get() {
    let config = config();
    let store = block_on(Store::open_in_memory()).unwrap();
    let http = FakeHttp::new();
    http.set(ROUTER, Err(SourceError::Http("connection refused".into())));
    let env = Env(BTreeMap::new());
    let engine = Engine::new(&config, &store, &http, &env).with_clock(clock);

    let report = run(&engine, &Selection::Ids(vec!["router".into()]));
    assert!(
        matches!(outcome(&report, "router"), SourceOutcome::Failed(e) if e.contains("connection refused"))
    );
    let state = block_on(store.source_state("router")).unwrap();
    assert_eq!(state.failures, 1);
    let runs = block_on(store.sync_runs(Some("router"), 5)).unwrap();
    assert!(runs[0].error.is_some());
    assert_eq!(report.failures().count(), 1);

    // Recovery with an ETag, then a 304.
    let mut ok = Response::ok(router_body(&["openai/gpt-4o"]));
    ok.headers.insert("etag".into(), "\"v1\"".into());
    http.set(ROUTER, Ok(ok));
    run(&engine, &Selection::Ids(vec!["router".into()]));
    let state = block_on(store.source_state("router")).unwrap();
    assert_eq!((state.failures, state.etag.as_deref()), (0, Some("\"v1\"")));

    http.set(
        ROUTER,
        Ok(Response {
            status: 304,
            headers: BTreeMap::new(),
            body: Vec::new(),
        }),
    );
    let report = run(&engine, &Selection::Ids(vec!["router".into()]));
    assert_eq!(outcome(&report, "router"), &SourceOutcome::NotModified);
    let last = http.requests().pop().unwrap();
    assert!(
        last.headers
            .contains(&("if-none-match".into(), "\"v1\"".into()))
    );
    assert_eq!(
        block_on(store.source_state("router"))
            .unwrap()
            .listing_count,
        1
    );

    // Changing the rules invalidates the cache: no conditional header is sent.
    let mut changed = config.clone();
    changed.canonical.collapse_snapshots = false;
    let engine = Engine::new(&changed, &store, &http, &env).with_clock(clock);
    run(&engine, &Selection::Ids(vec!["router".into()]));
    let last = http.requests().pop().unwrap();
    assert!(!last.headers.iter().any(|(k, _)| k == "if-none-match"));
}

#[test]
fn due_selection_and_unknown_ids() {
    let config = config();
    let store = block_on(Store::open_in_memory()).unwrap();
    let http = FakeHttp::new().route(ROUTER, router_body(&["a/b"]));
    let env = Env(BTreeMap::new());
    let engine = Engine::new(&config, &store, &http, &env).with_clock(clock);
    assert!(matches!(
        block_on(engine.resolve(&Selection::Ids(vec!["nope".into()]))),
        Err(EngineError::UnknownSource(_))
    ));
    run(&engine, &Selection::Ids(vec!["router".into()]));
    let due = block_on(engine.resolve(&Selection::Due)).unwrap();
    assert!(!due.contains(&"router".to_owned()), "just synced");
    assert!(due.contains(&"openai".to_owned()));
}

/// Environment plus vault entries.
struct Vault(BTreeMap<&'static str, &'static str>);

impl SecretLookup for Vault {
    fn var(&self, _name: &str) -> Option<String> {
        None
    }

    fn vault(&self, name: &SecretName) -> Result<Option<Secret>, String> {
        Ok(self
            .0
            .get(name.to_string().as_str())
            .map(|v| Secret::new(*v)))
    }
}

const ANTHROPIC: &str = "https://api.anthropic.com/v1/models?limit=1000";

#[test]
fn vault_keys_and_header_references_reach_requests() {
    let mut config = config();
    config.sources.get_mut("openai").unwrap().headers.insert(
        "x-org".into(),
        SecretValue::Ref(SecretRef::Vault("openai/ORG".into())),
    );
    let store = block_on(Store::open_in_memory()).unwrap();
    let http = FakeHttp::new()
        .route(OPENAI, openai_body(&["gpt-4o"]))
        .route(ANTHROPIC, r#"{"data":[],"has_more":false}"#);
    let vault = Vault(BTreeMap::from([
        ("openai/API_KEY", "sk-vault"),
        ("openai/ORG", "org-secret"),
        ("locked/API_KEY", "sk-ant"),
    ]));
    let engine = Engine::new(&config, &store, &http, &vault).with_clock(clock);
    let report = run(
        &engine,
        &Selection::Ids(vec!["openai".into(), "locked".into()]),
    );
    assert!(matches!(
        outcome(&report, "openai"),
        SourceOutcome::Synced(_)
    ));
    assert!(matches!(
        outcome(&report, "locked"),
        SourceOutcome::Synced(_)
    ));
    let sent = http.requests();
    let openai = sent.iter().find(|r| r.url == OPENAI).unwrap();
    assert!(
        openai
            .headers
            .contains(&("authorization".into(), "Bearer sk-vault".into()))
    );
    assert!(
        openai
            .headers
            .contains(&("x-org".into(), "org-secret".into()))
    );
    let anthropic = sent.iter().find(|r| r.url == ANTHROPIC).unwrap();
    assert!(
        anthropic
            .headers
            .contains(&("x-api-key".into(), "sk-ant".into()))
    );
}

#[test]
fn missing_header_secret_fails_and_names_the_header() {
    let mut config = config();
    config.sources.get_mut("router").unwrap().headers.insert(
        "x-token".into(),
        SecretValue::Ref(SecretRef::Vault("router/TOKEN".into())),
    );
    let store = block_on(Store::open_in_memory()).unwrap();
    let http = FakeHttp::new().route(ROUTER, router_body(&["a/b"]));
    let env = Env(BTreeMap::new());
    let engine = Engine::new(&config, &store, &http, &env).with_clock(clock);
    let report = run(&engine, &Selection::Ids(vec!["router".into()]));
    let SourceOutcome::Failed(error) = outcome(&report, "router") else {
        panic!("expected failure");
    };
    assert!(error.contains("headers.x-token"), "{error}");
    assert!(error.contains("bmodels secret set router/TOKEN"), "{error}");
    assert_eq!(http.requests().len(), 0);
}

#[test]
fn rejected_keys_are_named_and_secrets_are_redacted() {
    let mut config = config();
    config.sources.get_mut("router").unwrap().headers.insert(
        "x-token".into(),
        SecretValue::Literal("hdr-secret-value".into()),
    );
    let store = block_on(Store::open_in_memory()).unwrap();
    let http = FakeHttp::new();
    http.set(
        OPENAI,
        Ok(Response {
            status: 401,
            headers: BTreeMap::new(),
            body: b"bad key".to_vec(),
        }),
    );
    http.set(
        ROUTER,
        Ok(Response {
            status: 500,
            headers: BTreeMap::new(),
            body: b"echo: hdr-secret-value".to_vec(),
        }),
    );
    let env = Env(BTreeMap::from([("OPENAI_KEY", "sk-test")]));
    let engine = Engine::new(&config, &store, &http, &env).with_clock(clock);
    let report = run(
        &engine,
        &Selection::Ids(vec!["openai".into(), "router".into()]),
    );
    assert_eq!(
        outcome(&report, "openai"),
        &SourceOutcome::Failed(
            SourceError::KeyRejected {
                source_id: "openai".into(),
                status: 401
            }
            .to_string()
        )
    );
    let SourceOutcome::Failed(error) = outcome(&report, "router") else {
        panic!("expected failure");
    };
    assert!(
        !error.contains("hdr-secret-value") && error.contains("<redacted>"),
        "{error}"
    );
    let state = block_on(store.source_state("router")).unwrap();
    assert!(!state.last_error.unwrap().contains("hdr-secret-value"));
}
