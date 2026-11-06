//! Maps listings onto canonical models (`vendor/family`).
//!
//! 1. Vendor: the source's `vendor`; else, when the adapter's hint names a *host* (models.dev
//!    and LiteLLM providers resell other vendors' models), a known `family_vendors` prefix wins
//!    over that hint; else the hint, the id's `vendor/` prefix (a true owner, so fine-tunes
//!    like `nvidia/llama-…` stay NVIDIA's), the family prefix, or the source id. Then
//!    lowercased, stripped of OpenRouter's `~` alias marker, and mapped through
//!    `vendor_aliases`.
//! 2. Family: the id's last `/` segment, lowercased, with `strip_suffixes` removed, `.`/`_`/
//!    spaces turned into `-` (so `claude-sonnet-4.5` and `claude-sonnet-4-5` meet), and, with
//!    `collapse_snapshots`, a trailing date/version stamp removed (`-2024-08-06`,
//!    `-20241022`, `-preview-05-06` → `-preview`, `-0613`, `-001`).
//! 3. `aliases` remaps the resulting key.
//!
//! Announcements never merge: their key is `news/<source>/<hash of id>`.

use bmodels_model::{CanonicalRules, ModelRecord, RecordKind, SourceSpec};

/// A listing's canonical identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canonical {
    /// `vendor/family`.
    pub key: String,
    /// Vendor.
    pub vendor: String,
    /// Whether the listing's own id is the canonical family (not a snapshot or variant).
    pub exact: bool,
}

/// Applies [`CanonicalRules`].
#[derive(Debug, Clone)]
pub struct Canonicalizer {
    rules: CanonicalRules,
    family_prefixes: Vec<(String, String)>,
}

impl Canonicalizer {
    /// Prepares the rules.
    #[must_use]
    pub fn new(rules: &CanonicalRules) -> Self {
        let mut family_prefixes: Vec<(String, String)> = rules
            .family_vendors
            .iter()
            .map(|(p, v)| (p.to_ascii_lowercase(), v.clone()))
            .collect();
        family_prefixes.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        Self {
            rules: rules.clone(),
            family_prefixes,
        }
    }

    /// Canonical identity of `record` from source `source_id`.
    #[must_use]
    pub fn canonicalize(
        &self,
        source_id: &str,
        spec: &SourceSpec,
        record: &ModelRecord,
    ) -> Canonical {
        if record.kind == RecordKind::Announcement {
            let vendor = self.vendor(spec.vendor.as_deref().unwrap_or("news"));
            return Canonical {
                key: format!("news/{source_id}/{:016x}", fnv1a(&record.model_id)),
                vendor,
                exact: true,
            };
        }
        let raw = record.model_id.trim();
        let (id_vendor, last) = match raw.split_once('/') {
            Some((prefix, _)) => (Some(prefix), raw.rsplit('/').next().unwrap_or(raw)),
            None => (None, raw),
        };
        let stripped = self.strip_suffixes(&last.to_lowercase());
        let normalized = normalize(&stripped);
        let family = if self.rules.collapse_snapshots {
            strip_snapshot(&normalized).to_owned()
        } else {
            normalized
        };
        let family_vendor = self.vendor_from_family(&family);
        let vendor = match (&spec.vendor, &record.vendor_hint, family_vendor) {
            (Some(v), _, _) => self.vendor(v),
            (None, Some(_), Some(known)) => known,
            (None, hint, known) => hint
                .as_deref()
                .or(id_vendor)
                .map(|v| self.vendor(v))
                .or(known)
                .unwrap_or_else(|| self.vendor(source_id)),
        };
        let unaliased = format!("{vendor}/{family}");
        let aliased = self.rules.aliases.get(&unaliased);
        let exact = aliased.is_none() && normalize(&last.to_lowercase()) == family;
        let key = aliased.cloned().unwrap_or(unaliased);
        let vendor = key.split('/').next().unwrap_or(&vendor).to_owned();
        Canonical { key, vendor, exact }
    }

    fn vendor(&self, raw: &str) -> String {
        let lower = raw.trim().trim_start_matches('~').to_ascii_lowercase();
        self.rules
            .vendor_aliases
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(&lower))
            .map_or(lower, |(_, vendor)| vendor.clone())
    }

    fn vendor_from_family(&self, family: &str) -> Option<String> {
        self.family_prefixes
            .iter()
            .find(|(prefix, _)| family.starts_with(prefix.as_str()))
            .map(|(_, vendor)| vendor.clone())
    }

    fn strip_suffixes(&self, id: &str) -> String {
        let mut out = id.to_owned();
        let mut changed = true;
        while changed {
            changed = false;
            for suffix in &self.rules.strip_suffixes {
                let suffix = suffix.to_lowercase();
                if out.len() > suffix.len() && out.ends_with(&suffix) {
                    out.truncate(out.len() - suffix.len());
                    changed = true;
                }
            }
        }
        out
    }
}

/// Lowercase, `.`/`_`/space → `-`, collapsed and trimmed dashes.
fn normalize(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for c in id.chars() {
        let c = match c {
            '.' | '_' | ' ' => '-',
            c => c.to_ascii_lowercase(),
        };
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('-').to_owned()
}

/// Removes one trailing snapshot stamp.
fn strip_snapshot(family: &str) -> &str {
    let parts: Vec<&str> = family.split('-').collect();
    let n = parts.len();
    let digits = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit());
    let cut = |k: usize| -> &str {
        // Keep the first `n - k` parts.
        let keep: usize = parts[..n - k].iter().map(|p| p.len() + 1).sum::<usize>();
        &family[..keep.saturating_sub(1)]
    };
    // Never strip down to nothing or to a bare number.
    let ok = |k: usize| {
        n > k
            && parts[..n - k]
                .iter()
                .any(|p| p.bytes().any(|b| b.is_ascii_alphabetic()))
    };
    if n >= 4
        && digits(parts[n - 3], 4)
        && digits(parts[n - 2], 2)
        && digits(parts[n - 1], 2)
        && ok(3)
    {
        return cut(3);
    }
    if n >= 2 && digits(parts[n - 1], 8) && ok(1) {
        return cut(1);
    }
    if n >= 3 && digits(parts[n - 2], 2) && digits(parts[n - 1], 2) && ok(2) {
        return cut(2);
    }
    if n >= 2 && (digits(parts[n - 1], 4) || digits(parts[n - 1], 3)) && ok(1) {
        return cut(1);
    }
    family
}

/// 64-bit FNV-1a (stable across builds, unlike `DefaultHasher`).
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bmodels_model::SourceKind;

    use super::*;

    fn rules() -> CanonicalRules {
        CanonicalRules {
            collapse_snapshots: true,
            strip_suffixes: vec![":free".into(), "-latest".into()],
            vendor_aliases: BTreeMap::from([
                ("meta-llama".into(), "meta".into()),
                ("gemini".into(), "google".into()),
            ]),
            family_vendors: BTreeMap::from([
                ("llama".into(), "meta".into()),
                ("gpt".into(), "openai".into()),
                ("gpt-oss".into(), "openai".into()),
            ]),
            aliases: BTreeMap::from([("openai/chatgpt-4o".into(), "openai/gpt-4o".into())]),
        }
    }

    fn key(source: &str, vendor: Option<&str>, hint: Option<&str>, id: &str) -> String {
        let mut spec = SourceSpec::new(SourceKind::OpenaiCompatible);
        spec.vendor = vendor.map(str::to_owned);
        let mut record = ModelRecord::new(id);
        record.vendor_hint = hint.map(str::to_owned);
        Canonicalizer::new(&rules())
            .canonicalize(source, &spec, &record)
            .key
    }

    #[test]
    fn same_model_meets_across_sources() {
        let direct = key(
            "anthropic",
            Some("anthropic"),
            None,
            "claude-sonnet-4-5-20250929",
        );
        let router = key("openrouter", None, None, "anthropic/claude-sonnet-4.5");
        let router_free = key("openrouter", None, None, "anthropic/claude-sonnet-4.5:free");
        let dev = key(
            "models_dev",
            None,
            Some("anthropic"),
            "anthropic/claude-sonnet-4-5",
        );
        assert_eq!(direct, "anthropic/claude-sonnet-4-5");
        assert_eq!(router, direct);
        assert_eq!(router_free, direct);
        assert_eq!(dev, direct);
    }

    #[test]
    fn snapshot_stamps() {
        for (id, want) in [
            ("gpt-4o-2024-08-06", "gpt-4o"),
            ("gpt-4-0613", "gpt-4"),
            ("gemini-1.5-pro-002", "gemini-1-5-pro"),
            ("gemini-2.5-pro-preview-05-06", "gemini-2-5-pro-preview"),
            ("deepseek-v3-0324", "deepseek-v3"),
            ("o1-2024-12-17", "o1"),
            ("gpt-4.1", "gpt-4-1"),
            ("llama-3.1-405b", "llama-3-1-405b"),
            ("gpt-3.5-turbo-16k", "gpt-3-5-turbo-16k"),
            ("2024", "2024"),
        ] {
            assert_eq!(key("x", Some("v"), None, id), format!("v/{want}"), "{id}");
        }
    }

    #[test]
    fn vendor_resolution_order() {
        assert_eq!(
            key("groq", None, None, "llama-3.3-70b-versatile"),
            "meta/llama-3-3-70b-versatile"
        );
        assert_eq!(
            key("groq", None, None, "gpt-oss-120b"),
            "openai/gpt-oss-120b"
        );
        assert_eq!(
            key("together", None, None, "meta-llama/Llama-3.3-70B"),
            "meta/llama-3-3-70b"
        );
        assert_eq!(
            key("litellm", None, Some("gemini"), "gemini/gemini-2.5-pro"),
            "google/gemini-2-5-pro"
        );
        assert_eq!(
            key("local", None, None, "mystery-model"),
            "local/mystery-model"
        );
        assert_eq!(
            key("openai", Some("openai"), None, "chatgpt-4o-latest"),
            "openai/gpt-4o"
        );
        // A host hint loses to a known family; an owner prefix does not.
        assert_eq!(
            key("models_dev", None, Some("alibaba"), "alibaba/llama-4-scout"),
            "meta/llama-4-scout"
        );
        assert_eq!(
            key("openrouter", None, None, "nvidia/llama-3.1-nemotron"),
            "nvidia/llama-3-1-nemotron"
        );
        assert_eq!(
            key("openrouter", None, None, "~openai/gpt-latest"),
            "openai/gpt"
        );
    }

    #[test]
    fn exactness_and_announcements() {
        let c = Canonicalizer::new(&rules());
        let spec = SourceSpec::new(SourceKind::Openrouter);
        assert!(
            c.canonicalize("o", &spec, &ModelRecord::new("openai/gpt-4o"))
                .exact
        );
        assert!(
            !c.canonicalize("o", &spec, &ModelRecord::new("openai/gpt-4o-2024-08-06"))
                .exact
        );

        let mut news = ModelRecord::new("https://openai.com/index/x");
        news.kind = RecordKind::Announcement;
        let a = c.canonicalize("openai_news", &spec, &news);
        assert!(a.key.starts_with("news/openai_news/"));
        assert_eq!(a.key, c.canonicalize("openai_news", &spec, &news).key);
    }
}
