//! Deterministic, evidence-preserving EPG reconciliation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StationEvidence {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub tvg_id: Option<String>,
    #[serde(default)]
    pub callsign: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MappingMethod {
    Manual,
    Previous,
    TvgId,
    Callsign,
    Alias,
    ExactName,
    Fuzzy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MappingCandidate {
    pub epg_id: String,
    pub method: MappingMethod,
    pub confidence: f32,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MappingDecision {
    Applied {
        candidate: MappingCandidate,
    },
    Review {
        candidates: Vec<MappingCandidate>,
        reason: String,
    },
    Unresolved {
        reason: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct MappingContext {
    /// Explicit stream/channel to EPG bindings.
    pub manual: BTreeMap<String, String>,
    /// The last activated binding, retained ahead of newly inferred matches.
    pub previous: BTreeMap<String, String>,
    /// Normalized provider names/callsigns to a configured EPG ID.
    pub aliases: BTreeMap<String, String>,
    /// Minimum token similarity considered for constrained fuzzy review.
    pub fuzzy_threshold: f32,
}

impl MappingContext {
    pub fn with_safe_defaults() -> Self {
        Self {
            fuzzy_threshold: 0.82,
            ..Self::default()
        }
    }
}

/// Reconciles one provider station against a narrowed EPG candidate set.
///
/// Automatic application is deliberately limited to unique, exact evidence or
/// an existing explicit/stable binding. Fuzzy candidates always require review.
pub fn reconcile_epg(
    station: &StationEvidence,
    candidates: &[StationEvidence],
    context: &MappingContext,
) -> MappingDecision {
    if candidates.is_empty() {
        return MappingDecision::Unresolved {
            reason: "the EPG source returned no candidates; the current binding must be retained"
                .to_owned(),
        };
    }

    for (method, configured) in [
        (MappingMethod::Manual, context.manual.get(&station.id)),
        (MappingMethod::Previous, context.previous.get(&station.id)),
    ] {
        if let Some(epg_id) = configured
            && candidates.iter().any(|candidate| candidate.id == *epg_id)
        {
            return applied(epg_id, method, 1.0, "configured stable binding");
        }
    }

    if let Some(tvg_id) = nonblank(station.tvg_id.as_deref()) {
        let exact = candidates
            .iter()
            .filter(|candidate| {
                nonblank(candidate.tvg_id.as_deref())
                    .is_some_and(|candidate_id| candidate_id.eq_ignore_ascii_case(tvg_id))
                    || candidate.id.eq_ignore_ascii_case(tvg_id)
            })
            .collect::<Vec<_>>();
        if let Some(decision) = unique_or_review(exact, MappingMethod::TvgId, 0.99, "tvg-id") {
            return decision;
        }
    }

    if let Some(callsign) = nonblank(station.callsign.as_deref()) {
        let exact = candidates
            .iter()
            .filter(|candidate| {
                nonblank(candidate.callsign.as_deref())
                    .is_some_and(|value| value.eq_ignore_ascii_case(callsign))
            })
            .collect::<Vec<_>>();
        if let Some(decision) = unique_or_review(exact, MappingMethod::Callsign, 0.98, "callsign") {
            return decision;
        }
    }

    for alias_key in [station.callsign.as_deref(), Some(station.name.as_str())]
        .into_iter()
        .flatten()
        .map(normalize)
    {
        if let Some(epg_id) = context.aliases.get(&alias_key)
            && candidates.iter().any(|candidate| candidate.id == *epg_id)
        {
            return applied(epg_id, MappingMethod::Alias, 0.97, "configured alias");
        }
    }

    let station_name = normalize(&station.name);
    let exact = candidates
        .iter()
        .filter(|candidate| !station_name.is_empty() && normalize(&candidate.name) == station_name)
        .collect::<Vec<_>>();
    if let Some(decision) = unique_or_review(exact, MappingMethod::ExactName, 0.95, "name") {
        return decision;
    }

    let threshold = if context.fuzzy_threshold > 0.0 {
        context.fuzzy_threshold
    } else {
        0.82
    };
    let mut fuzzy = candidates
        .iter()
        .filter(|candidate| same_scope(station, candidate))
        .filter_map(|candidate| {
            let confidence = token_similarity(&station.name, &candidate.name);
            (confidence >= threshold).then(|| MappingCandidate {
                epg_id: candidate.id.clone(),
                method: MappingMethod::Fuzzy,
                confidence,
                evidence: vec![format!(
                    "scope-constrained token similarity {confidence:.3}"
                )],
            })
        })
        .collect::<Vec<_>>();
    fuzzy.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| left.epg_id.cmp(&right.epg_id))
    });
    if !fuzzy.is_empty() {
        return MappingDecision::Review {
            candidates: fuzzy,
            reason: "fuzzy evidence is never activated without review".to_owned(),
        };
    }

    MappingDecision::Unresolved {
        reason: "no sufficiently corroborated EPG candidate".to_owned(),
    }
}

/// Keeps an activated mapping when a recomputation produces no usable result.
pub fn apply_non_destructive(
    current_epg_id: Option<&str>,
    decision: &MappingDecision,
) -> Option<String> {
    match decision {
        MappingDecision::Applied { candidate } => Some(candidate.epg_id.clone()),
        MappingDecision::Review { .. } | MappingDecision::Unresolved { .. } => {
            current_epg_id.map(str::to_owned)
        }
    }
}

fn applied(
    epg_id: &str,
    method: MappingMethod,
    confidence: f32,
    evidence: &str,
) -> MappingDecision {
    MappingDecision::Applied {
        candidate: MappingCandidate {
            epg_id: epg_id.to_owned(),
            method,
            confidence,
            evidence: vec![evidence.to_owned()],
        },
    }
}

fn unique_or_review(
    candidates: Vec<&StationEvidence>,
    method: MappingMethod,
    confidence: f32,
    evidence: &str,
) -> Option<MappingDecision> {
    match candidates.as_slice() {
        [] => None,
        [candidate] => Some(applied(&candidate.id, method, confidence, evidence)),
        _ => Some(MappingDecision::Review {
            candidates: candidates
                .into_iter()
                .map(|candidate| MappingCandidate {
                    epg_id: candidate.id.clone(),
                    method,
                    confidence,
                    evidence: vec![format!("ambiguous exact {evidence}")],
                })
                .collect(),
            reason: format!("multiple EPG channels share the same {evidence}"),
        }),
    }
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn same_scope(left: &StationEvidence, right: &StationEvidence) -> bool {
    option_scope_matches(left.group.as_deref(), right.group.as_deref())
        && option_scope_matches(left.country.as_deref(), right.country.as_deref())
}

fn option_scope_matches(left: Option<&str>, right: Option<&str>) -> bool {
    match (nonblank(left), nonblank(right)) {
        (Some(left), Some(right)) => normalize(left) == normalize(right),
        _ => true,
    }
}

#[allow(clippy::cast_precision_loss)] // Station names contain far fewer than 2^24 tokens.
fn token_similarity(left: &str, right: &str) -> f32 {
    let left = normalize(left)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let right = normalize(right)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count() as f32;
    (2.0 * intersection) / (left.len() + right.len()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(id: &str, name: &str) -> StationEvidence {
        StationEvidence {
            id: id.to_owned(),
            name: name.to_owned(),
            ..StationEvidence::default()
        }
    }

    fn applied_method(decision: &MappingDecision) -> MappingMethod {
        let MappingDecision::Applied { candidate } = decision else {
            panic!("expected applied mapping: {decision:?}");
        };
        candidate.method
    }

    #[test]
    fn manual_and_previous_bindings_have_precedence_and_must_still_exist() {
        let source = station("stream", "News");
        let candidates = [station("manual", "Other"), station("previous", "News")];
        let mut context = MappingContext::with_safe_defaults();
        context.manual.insert("stream".into(), "manual".into());
        context.previous.insert("stream".into(), "previous".into());
        assert_eq!(
            applied_method(&reconcile_epg(&source, &candidates, &context)),
            MappingMethod::Manual
        );
        context.manual.insert("stream".into(), "missing".into());
        assert_eq!(
            applied_method(&reconcile_epg(&source, &candidates, &context)),
            MappingMethod::Previous
        );
    }

    #[test]
    fn exact_evidence_follows_tvg_callsign_alias_then_name_order() {
        let mut source = station("stream", "Channel Seven HD");
        source.tvg_id = Some("tvg.seven".into());
        source.callsign = Some("KSEV".into());
        let mut tvg = station("epg-tvg", "Different");
        tvg.tvg_id = Some("TVG.SEVEN".into());
        let mut callsign = station("epg-call", "Different");
        callsign.callsign = Some("ksev".into());
        let exact = station("epg-name", "channel-seven hd");
        let candidates = [tvg, callsign, exact];
        let mut context = MappingContext::with_safe_defaults();
        context
            .aliases
            .insert("channel seven hd".into(), "epg-name".into());
        assert_eq!(
            applied_method(&reconcile_epg(&source, &candidates, &context)),
            MappingMethod::TvgId
        );
        source.tvg_id = None;
        assert_eq!(
            applied_method(&reconcile_epg(&source, &candidates, &context)),
            MappingMethod::Callsign
        );
        source.callsign = None;
        assert_eq!(
            applied_method(&reconcile_epg(&source, &candidates, &context)),
            MappingMethod::Alias
        );
        context.aliases.clear();
        assert_eq!(
            applied_method(&reconcile_epg(&source, &candidates, &context)),
            MappingMethod::ExactName
        );
    }

    #[test]
    fn ambiguous_exact_and_fuzzy_matches_enter_review_deterministically() {
        let mut source = station("stream", "Denver Sports Network HD");
        source.tvg_id = Some("duplicate".into());
        source.group = Some("Sports".into());
        source.country = Some("US".into());
        let mut one = station("b", "Denver Sports Network");
        one.tvg_id = Some("duplicate".into());
        one.group = Some("Sports".into());
        one.country = Some("US".into());
        let mut two = one.clone();
        two.id = "a".into();
        let decision = reconcile_epg(
            &source,
            &[one.clone(), two],
            &MappingContext::with_safe_defaults(),
        );
        assert!(
            matches!(decision, MappingDecision::Review { ref candidates, .. } if candidates.len() == 2)
        );

        source.tvg_id = None;
        let decision = reconcile_epg(&source, &[one], &MappingContext::with_safe_defaults());
        assert!(
            matches!(decision, MappingDecision::Review { ref candidates, .. }
            if candidates[0].method == MappingMethod::Fuzzy)
        );
    }

    #[test]
    fn fuzzy_matching_is_scope_constrained_and_never_auto_applied() {
        let mut source = station("stream", "Denver Sports Network HD");
        source.group = Some("Sports".into());
        source.country = Some("US".into());
        let mut wrong_scope = station("wrong", "Denver Sports Network");
        wrong_scope.group = Some("News".into());
        wrong_scope.country = Some("CA".into());
        assert!(matches!(
            reconcile_epg(
                &source,
                &[wrong_scope],
                &MappingContext::with_safe_defaults()
            ),
            MappingDecision::Unresolved { .. }
        ));
    }

    #[test]
    fn zero_result_and_review_never_clear_a_working_binding() {
        let source = station("stream", "Unknown");
        let unresolved = reconcile_epg(&source, &[], &MappingContext::with_safe_defaults());
        assert_eq!(
            apply_non_destructive(Some("working"), &unresolved).as_deref(),
            Some("working")
        );
        let applied = MappingDecision::Applied {
            candidate: MappingCandidate {
                epg_id: "replacement".into(),
                method: MappingMethod::Manual,
                confidence: 1.0,
                evidence: vec![],
            },
        };
        assert_eq!(
            apply_non_destructive(Some("working"), &applied).as_deref(),
            Some("replacement")
        );
    }

    #[test]
    fn empty_and_punctuation_only_names_cannot_fuzzy_match() {
        let source = station("stream", "---");
        let candidate = station("epg", "...");
        assert!(matches!(
            reconcile_epg(&source, &[candidate], &MappingContext::with_safe_defaults()),
            MappingDecision::Unresolved { .. }
        ));
    }
}
