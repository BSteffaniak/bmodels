use bmodels_model::{CanonicalRules, ModelInfo, RecordKind, SourceKind};
use bmodels_sources::Validators;

use super::*;

const NOW: Timestamp = 1_000_000;

struct World {
    spec: SourceSpec,
    state: SourceState,
    existing: Vec<Listing>,
    models: BTreeMap<String, CanonicalModel>,
    canonicalizer: Canonicalizer,
    source_filter: IdFilter,
    global_filter: IdFilter,
}

impl World {
    fn new(vendor: Option<&str>) -> Self {
        let mut spec = SourceSpec::new(SourceKind::OpenaiCompatible);
        spec.vendor = vendor.map(str::to_owned);
        Self {
            spec,
            state: SourceState::default(),
            existing: Vec::new(),
            models: BTreeMap::new(),
            canonicalizer: Canonicalizer::new(&CanonicalRules::default()),
            source_filter: IdFilter::new(&[], &[]).unwrap(),
            global_filter: IdFilter::new(&[], &[]).unwrap(),
        }
    }

    fn baselined(mut self) -> Self {
        self.state.baseline_at = Some(NOW - 10_000);
        self
    }

    fn plan(
        &self,
        source: &str,
        records: Vec<ModelRecord>,
        complete: bool,
    ) -> Result<Plan, PlanError> {
        let fetched = Fetched {
            records,
            complete,
            validators: Validators::default(),
            warnings: Vec::new(),
        };
        plan(&PlanInput {
            source_id: source,
            spec: &self.spec,
            now: NOW,
            state: &self.state,
            fetched: &fetched,
            canonicalizer: &self.canonicalizer,
            source_filter: &self.source_filter,
            global_filter: &self.global_filter,
            existing: &self.existing,
            models: &self.models,
            price_tolerance: 0.05,
        })
    }

    /// Applies a plan's writes to this world, like the store would.
    fn apply(&mut self, plan: &Plan) {
        for m in &plan.write.canonical_models {
            self.models.insert(m.key.clone(), m.clone());
        }
        for l in &plan.write.listings {
            self.existing.retain(|e| e.key != l.key);
            self.existing.push(l.clone());
        }
        self.state.baseline_at.get_or_insert(NOW);
    }
}

fn rec(id: &str) -> ModelRecord {
    let mut r = ModelRecord::new(id);
    r.display_name = Some(id.to_uppercase());
    r
}

fn priced(id: &str, input: f64) -> ModelRecord {
    let mut r = rec(id);
    r.info = ModelInfo {
        input_price: Some(input),
        ..ModelInfo::default()
    };
    r
}

fn kinds(plan: &Plan) -> Vec<(EventKind, &str, bool)> {
    plan.write
        .events
        .iter()
        .map(|e| (e.kind, e.listing.model_id.as_str(), e.acked_at.is_some()))
        .collect()
}

#[test]
fn first_sync_is_a_silent_baseline() {
    let w = World::new(Some("openai"));
    let plan = w
        .plan(
            "openai",
            vec![rec("gpt-4o"), rec("gpt-4o-2024-08-06")],
            true,
        )
        .unwrap();
    assert_eq!(plan.write.events, Vec::new());
    assert_eq!(plan.counts.baseline, 2);
    assert_eq!(plan.counts.present, 2);
    assert!(
        plan.write
            .listings
            .iter()
            .all(|l| l.baseline && l.status == ListingStatus::Seen)
    );
    assert_eq!(
        plan.write.canonical_models.len(),
        1,
        "snapshot folds into gpt-4o"
    );
    assert_eq!(plan.write.canonical_models[0].status, ModelStatus::Seen);
}

#[test]
fn baseline_can_be_disabled() {
    let mut w = World::new(Some("openai"));
    w.spec.baseline = false;
    let plan = w.plan("openai", vec![rec("gpt-4o")], true).unwrap();
    assert_eq!(kinds(&plan), [(EventKind::New, "gpt-4o", false)]);
}

#[test]
fn new_model_then_new_listing_elsewhere() {
    let mut w = World::new(Some("openai")).baselined();
    let first = w.plan("openai", vec![rec("gpt-6")], true).unwrap();
    assert_eq!(kinds(&first), [(EventKind::New, "gpt-6", false)]);
    let model = &first.write.canonical_models[0];
    assert_eq!(
        (model.key.as_str(), model.status),
        ("openai/gpt-6", ModelStatus::Unseen)
    );
    assert_eq!(model.display_name, "GPT-6");
    w.apply(&first);

    // Same model on OpenRouter while still unseen: its NEW is already in the inbox.
    let mut router = World::new(None).baselined();
    router.models.clone_from(&w.models);
    let listing = router
        .plan("openrouter", vec![rec("openai/gpt-6")], true)
        .unwrap();
    assert_eq!(
        kinds(&listing),
        [(EventKind::NewListing, "openai/gpt-6", true)]
    );
    assert_eq!(listing.write.listings[0].status, ListingStatus::Unseen);

    // Once the model is seen, a new listing surfaces.
    router.models.get_mut("openai/gpt-6").unwrap().status = ModelStatus::Seen;
    let listing = router
        .plan("openrouter", vec![rec("openai/gpt-6")], true)
        .unwrap();
    assert_eq!(
        kinds(&listing),
        [(EventKind::NewListing, "openai/gpt-6", false)]
    );
}

#[test]
fn two_snapshots_of_a_new_model_in_one_sync() {
    let w = World::new(Some("openai")).baselined();
    let plan = w
        .plan("openai", vec![rec("o9"), rec("o9-2026-11-01")], true)
        .unwrap();
    assert_eq!(
        kinds(&plan),
        [
            (EventKind::New, "o9", false),
            (EventKind::NewListing, "o9-2026-11-01", true)
        ]
    );
}

#[test]
fn tracked_changes_emit_changed_untracked_update_silently() {
    let mut w = World::new(Some("openai")).baselined();
    let first = w.plan("openai", vec![priced("gpt-4o", 2.5)], true).unwrap();
    w.apply(&first);

    let mut described = priced("gpt-4o", 2.5);
    described.info.description = Some("now with a description".into());
    let quiet = w.plan("openai", vec![described], true).unwrap();
    assert_eq!(quiet.write.events, Vec::new());
    assert_eq!(quiet.write.listings.len(), 1, "listing rewritten");

    let cheaper = w
        .plan("openai", vec![priced("gpt-4o", 1.25)], true)
        .unwrap();
    assert_eq!(kinds(&cheaper), [(EventKind::Changed, "gpt-4o", false)]);
    let change = &cheaper.write.events[0].changes[0];
    assert_eq!(change.field, "input_price");
    assert_eq!(
        (change.old.as_deref(), change.new.as_deref()),
        (Some("$2.50"), Some("$1.25"))
    );

    let same = w.plan("openai", vec![priced("gpt-4o", 2.5)], true).unwrap();
    assert_eq!(
        same.write.listings,
        Vec::new(),
        "unchanged rows are not rewritten"
    );
}

#[test]
fn removal_reappearance_and_the_empty_guard() {
    let mut w = World::new(Some("openai")).baselined();
    let first = w.plan("openai", vec![rec("a"), rec("b")], true).unwrap();
    w.apply(&first);

    let partial = w.plan("openai", vec![rec("a")], false).unwrap();
    assert_eq!(
        partial.write.events,
        Vec::new(),
        "incomplete responses never remove"
    );

    let removed = w.plan("openai", vec![rec("a")], true).unwrap();
    assert_eq!(kinds(&removed), [(EventKind::Removed, "b", false)]);
    assert_eq!(removed.counts.present, 1);
    w.apply(&removed);

    let back = w.plan("openai", vec![rec("a"), rec("b")], true).unwrap();
    assert_eq!(kinds(&back), [(EventKind::Reappeared, "b", false)]);
    assert_eq!(back.counts.present, 2);

    assert_eq!(
        w.plan("openai", Vec::new(), true),
        Err(PlanError::EmptyResponse(1))
    );
}

#[test]
fn windowed_sources_treat_old_records_as_baseline() {
    let w = World::new(None).baselined();
    let mut old = rec("Qwen/Qwen2-7B");
    old.created_at = Some(NOW - 30 * 86_400);
    let mut fresh = rec("Qwen/Qwen9-7B");
    fresh.created_at = Some(NOW - 60);
    let plan = w.plan("huggingface", vec![old, fresh], false).unwrap();
    assert_eq!(kinds(&plan), [(EventKind::New, "Qwen/Qwen9-7B", false)]);
    assert_eq!(plan.counts.baseline, 1);
}

#[test]
fn ignored_models_never_surface() {
    let mut w = World::new(Some("openai")).baselined();
    let first = w.plan("openai", vec![priced("gpt-4o", 2.5)], true).unwrap();
    w.apply(&first);
    w.models.get_mut("openai/gpt-4o").unwrap().status = ModelStatus::Ignored;
    let plan = w
        .plan(
            "openai",
            vec![priced("gpt-4o", 9.0), rec("gpt-4o-2026-01-01")],
            true,
        )
        .unwrap();
    assert!(plan.write.events.iter().all(|e| e.acked_at.is_some()));
    assert_eq!(plan.write.events.len(), 2);
}

#[test]
fn filtered_listings_are_left_alone() {
    let mut w = World::new(Some("openai")).baselined();
    let first = w
        .plan("openai", vec![rec("gpt-4o"), rec("tts-1")], true)
        .unwrap();
    w.apply(&first);
    w.global_filter = IdFilter::new(&[], &["*tts*".into()]).unwrap();
    let plan = w
        .plan("openai", vec![rec("gpt-4o"), rec("tts-1")], true)
        .unwrap();
    assert_eq!(plan.write.events, Vec::new());
    // tts-1 is now excluded upstream too: still not removed.
    let plan = w.plan("openai", vec![rec("gpt-4o")], true).unwrap();
    assert_eq!(plan.write.events, Vec::new());
    assert_eq!(plan.counts.present, 2);
}

#[test]
fn human_names_replace_id_like_names() {
    let mut a = World::new(None).baselined();
    let first = a
        .plan("litellm", vec![ModelRecord::new("gpt-4o")], true)
        .unwrap();
    assert_eq!(first.write.canonical_models[0].display_name, "gpt-4o");
    a.apply(&first);
    let mut named = ModelRecord::new("openai/gpt-4o");
    named.display_name = Some("GPT-4o".into());
    let second = a.plan("openrouter", vec![named], true).unwrap();
    assert_eq!(second.write.canonical_models[0].display_name, "GPT-4o");
    a.apply(&second);
    // A later id-like name never replaces a human one.
    let mut idlike = ModelRecord::new("openai/gpt-4o");
    idlike.display_name = Some("gpt-4o".into());
    let third = a.plan("other", vec![idlike], true).unwrap();
    assert_eq!(third.write.canonical_models, Vec::new());
}

#[test]
fn direct_vendor_sources_name_the_model() {
    let mut router = World::new(None).baselined();
    let mut r = rec("anthropic/claude-x");
    r.display_name = Some("Claude X (router)".into());
    let first = router.plan("openrouter", vec![r], true).unwrap();
    router.apply(&first);

    let mut direct = World::new(Some("anthropic")).baselined();
    direct.models.clone_from(&router.models);
    let mut d = rec("claude-x");
    d.display_name = Some("Claude X".into());
    d.created_at = Some(5);
    let plan = direct.plan("anthropic", vec![d], true).unwrap();
    let model = &plan.write.canonical_models[0];
    assert_eq!(model.display_name, "Claude X");
    assert_eq!(model.created_at, Some(5));
    assert_eq!(model.kind, RecordKind::Model);
}
