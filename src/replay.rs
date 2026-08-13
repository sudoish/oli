//! Deterministic, provider-free replay of captured transcript and ledger fixtures.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{AgentError, Result};
use crate::ledger::{
    ContextEstimate, ContextRollup, RequestObservation, RequestPurpose, RunIdentity, RunSummary,
};

pub const SCHEMA: &str = "oli.replay/1";
const FULL_HISTORY_ARM: &str = "full-history";

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReplayReport {
    pub schema: String,
    pub fixture_hash: String,
    pub runs: Vec<RunComparison>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunComparison {
    pub session: String,
    pub run: String,
    pub provider: String,
    pub model: String,
    pub config_hash: String,
    pub resumed: Option<bool>,
    pub terminated: bool,
    pub arms: Vec<ArmReport>,
    pub comparisons: Vec<ArmComparison>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArmComparison {
    pub control_arm: String,
    pub candidate_arm: String,
    pub candidate_minus_control: EstimateDelta,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArmReport {
    pub arm: String,
    pub strategy: String,
    pub strategy_version: u32,
    pub requests: u32,
    pub strategy_internal_calls: u32,
    pub turns: u32,
    pub estimated: crate::ledger::ContextRollup,
    pub first_request: Option<crate::ledger::ContextEstimate>,
    pub materialization_misses: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EstimateDelta {
    pub total: Option<i64>,
    pub first_request: Option<i64>,
}

pub fn compare_bytes(bytes: &[u8]) -> Result<ReplayReport> {
    let fixture: CapturedFixture = serde_json::from_slice(bytes)?;
    let mut runs: Vec<CapturedRun> = Vec::new();
    let mut request_order = Vec::new();

    for record in fixture.ledger {
        match record.get("kind").and_then(Value::as_str) {
            Some("request") => {
                let observation: RequestObservation = serde_json::from_value(record)?;
                validate_ledger_schema(&observation.schema)?;
                let identity = identity_of(&observation);
                let run_index = run_index(&mut runs, identity)?;
                let request_index = runs[run_index].requests.len();
                runs[run_index].requests.push(observation);
                request_order.push((run_index, request_index));
            }
            Some("summary") => {
                let summary: RunSummary = serde_json::from_value(record)?;
                validate_ledger_schema(&summary.schema)?;
                let resumed = summary.resumed;
                let run_index = run_index(&mut runs, summary.identity.clone())?;
                if runs[run_index]
                    .resumed
                    .is_some_and(|value| value != resumed)
                {
                    return Err(AgentError::Config(format!(
                        "replay run {} changes resumed state",
                        runs[run_index].identity.run
                    )));
                }
                runs[run_index].resumed = Some(resumed);
                runs[run_index].terminated = true;
            }
            _ => {}
        }
    }
    if runs.is_empty() {
        return Err(AgentError::Config(
            "replay fixture contains no ledger runs".into(),
        ));
    }
    validate_transcript_meta(&fixture.transcript, &runs)?;

    let materialized: Vec<(usize, usize)> = request_order
        .into_iter()
        .filter(|(run, request)| is_materialized_request(&runs[*run].requests, *request))
        .collect();
    let tool_schemas: Vec<u64> = materialized
        .iter()
        .map(|(run, request)| runs[*run].requests[*request].estimated.tool_schemas)
        .collect();
    let replay = materialize_full_history(&fixture.transcript, &tool_schemas);
    let mut replayed_by_run: Vec<Vec<Option<ContextEstimate>>> =
        runs.iter().map(|_| Vec::new()).collect();
    for ((run, _), estimate) in materialized.iter().zip(replay.estimates) {
        replayed_by_run[*run].push(estimate);
    }

    let mut comparisons = Vec::with_capacity(runs.len());
    for (run_index, captured) in runs.into_iter().enumerate() {
        let materialized_requests: Vec<&RequestObservation> = captured
            .requests
            .iter()
            .enumerate()
            .filter(|(index, _)| is_materialized_request(&captured.requests, *index))
            .map(|(_, observation)| observation)
            .collect();
        let current_estimates: Vec<ContextEstimate> = materialized_requests
            .iter()
            .map(|observation| observation.estimated)
            .collect();
        let full_estimates = &replayed_by_run[run_index];
        let control_misses = full_estimates
            .iter()
            .filter(|estimate| estimate.is_none())
            .count() as u32;
        let full_present: Vec<ContextEstimate> = full_estimates
            .iter()
            .filter_map(|estimate| *estimate)
            .collect();
        let candidate_arm = format!("recorded:{}", captured.identity.strategy);

        let control = ArmReport {
            arm: FULL_HISTORY_ARM.into(),
            strategy: "full-history".into(),
            strategy_version: 1,
            requests: materialized_requests.len() as u32,
            strategy_internal_calls: 0,
            turns: materialized_requests
                .iter()
                .map(|observation| observation.turn)
                .max()
                .unwrap_or(0),
            estimated: rollup(&full_present),
            first_request: full_estimates.first().and_then(|estimate| *estimate),
            materialization_misses: control_misses,
        };
        let candidate = ArmReport {
            arm: candidate_arm.clone(),
            strategy: captured.identity.strategy.clone(),
            strategy_version: captured.identity.strategy_version,
            requests: materialized_requests.len() as u32,
            strategy_internal_calls: captured
                .requests
                .len()
                .saturating_sub(materialized_requests.len())
                as u32,
            turns: materialized_requests
                .iter()
                .map(|observation| observation.turn)
                .max()
                .unwrap_or(0),
            estimated: rollup(&current_estimates),
            first_request: current_estimates.first().copied(),
            // A summary-bearing estimate is comparable, but the raw
            // transcript cannot reconstruct the summary content that
            // produced it. Keep that limitation visible in the report.
            materialization_misses: materialized_requests
                .iter()
                .filter(|observation| observation.estimated.summary > 0)
                .count() as u32,
        };
        let candidate_minus_control = EstimateDelta {
            total: (control.materialization_misses == 0)
                .then(|| delta(candidate.estimated.total, control.estimated.total)),
            first_request: match (candidate.first_request, control.first_request) {
                (Some(candidate), Some(control)) => Some(delta(candidate.total, control.total)),
                _ => None,
            },
        };
        comparisons.push(RunComparison {
            session: captured.identity.session,
            run: captured.identity.run,
            provider: captured.identity.provider,
            model: captured.identity.model,
            config_hash: captured.identity.config_hash,
            resumed: captured.resumed,
            terminated: captured.terminated,
            arms: vec![control, candidate],
            comparisons: vec![ArmComparison {
                control_arm: FULL_HISTORY_ARM.into(),
                candidate_arm,
                candidate_minus_control,
            }],
        });
    }

    Ok(ReplayReport {
        schema: SCHEMA.into(),
        fixture_hash: format!("{:x}", Sha256::digest(bytes)),
        runs: comparisons,
    })
}

pub fn compare_path(path: &std::path::Path) -> Result<ReplayReport> {
    compare_bytes(&std::fs::read(path)?)
}

#[derive(Deserialize)]
struct CapturedFixture {
    transcript: Vec<Value>,
    ledger: Vec<Value>,
}

struct CapturedRun {
    identity: RunIdentity,
    requests: Vec<RequestObservation>,
    resumed: Option<bool>,
    terminated: bool,
}

fn run_index(runs: &mut Vec<CapturedRun>, identity: RunIdentity) -> Result<usize> {
    if let Some(index) = runs
        .iter()
        .position(|captured| captured.identity.run == identity.run)
    {
        validate_identity(&runs[index].identity, &identity)?;
        return Ok(index);
    }
    runs.push(CapturedRun {
        identity,
        requests: Vec::new(),
        resumed: None,
        terminated: false,
    });
    Ok(runs.len() - 1)
}

fn validate_ledger_schema(schema: &str) -> Result<()> {
    if schema != crate::ledger::SCHEMA {
        return Err(AgentError::Config(format!(
            "unsupported replay ledger schema: {schema}"
        )));
    }
    Ok(())
}

fn validate_transcript_meta(transcript: &[Value], runs: &[CapturedRun]) -> Result<()> {
    let metas: Vec<&Value> = transcript
        .iter()
        .filter(|event| event.get("op").and_then(Value::as_str) == Some("meta"))
        .filter_map(|event| event.get("meta"))
        .collect();

    for meta in &metas {
        if !runs.iter().any(|run| meta_matches_run(meta, run)) {
            return Err(AgentError::Config(
                "transcript meta does not match a pinned ledger identity".into(),
            ));
        }
    }
    // Old transcripts have no metadata at all and remain replayable. Once a
    // capture does carry metadata, validate the join in both directions so an
    // unrelated extra ledger run cannot borrow another run's transcript.
    if !metas.is_empty()
        && runs
            .iter()
            .any(|run| !metas.iter().any(|meta| meta_matches_run(meta, run)))
    {
        return Err(AgentError::Config(
            "pinned ledger identity has no matching transcript meta".into(),
        ));
    }
    Ok(())
}

fn meta_matches_run(meta: &Value, run: &CapturedRun) -> bool {
    metadata_field_matches(meta, "provider", &run.identity.provider)
        && metadata_field_matches(meta, "model", &run.identity.model)
        && metadata_field_matches(meta, "strategy", &run.identity.strategy)
        && metadata_field_matches(meta, "config_hash", &run.identity.config_hash)
}

fn metadata_field_matches(meta: &Value, field: &str, expected: &str) -> bool {
    meta.get(field)
        .and_then(Value::as_str)
        .is_some_and(|actual| actual == expected)
}

fn identity_of(observation: &RequestObservation) -> RunIdentity {
    RunIdentity {
        session: observation.session.clone(),
        run: observation.run.clone(),
        provider: observation.provider.clone(),
        model: observation.model.clone(),
        strategy: observation.strategy.clone(),
        strategy_version: observation.strategy_version,
        config_hash: observation.config_hash.clone(),
    }
}

fn validate_identity(expected: &RunIdentity, actual: &RunIdentity) -> Result<()> {
    for (field, before, after) in [
        (
            "session",
            expected.session.as_str(),
            actual.session.as_str(),
        ),
        (
            "provider",
            expected.provider.as_str(),
            actual.provider.as_str(),
        ),
        ("model", expected.model.as_str(), actual.model.as_str()),
        (
            "strategy",
            expected.strategy.as_str(),
            actual.strategy.as_str(),
        ),
        (
            "config_hash",
            expected.config_hash.as_str(),
            actual.config_hash.as_str(),
        ),
    ] {
        if before != after {
            return Err(AgentError::Config(format!(
                "replay run {} changes {field} from {before} to {after}",
                expected.run
            )));
        }
    }
    if expected.strategy_version != actual.strategy_version {
        return Err(AgentError::Config(format!(
            "replay run {} changes strategy_version from {} to {}",
            expected.run, expected.strategy_version, actual.strategy_version
        )));
    }
    Ok(())
}

fn is_materialized_request(requests: &[RequestObservation], index: usize) -> bool {
    match requests[index].purpose {
        Some(RequestPurpose::Agent) => return true,
        Some(RequestPurpose::Compaction) => return false,
        None => {}
    }
    // A linear compaction call is recorded immediately before the real
    // model request on the same turn. The last call for that turn is the
    // context snapshot the agent actually consumed.
    requests
        .get(index + 1)
        .is_none_or(|next| next.turn != requests[index].turn)
        && !is_legacy_linear_compaction(&requests[index])
}

fn is_legacy_linear_compaction(observation: &RequestObservation) -> bool {
    // Ledger v1 did not label a call's purpose. Linear compaction requests are
    // nevertheless distinguishable from agent requests in existing captures:
    // they summarize only recent messages and never carry pinned content,
    // an existing summary, or tool schemas. This also catches `/compact`, whose
    // call occupies a turn by itself rather than preceding a model request.
    observation.strategy == "linear-with-compact"
        && observation.estimated.pinned == 0
        && observation.estimated.tool_schemas == 0
        && observation.estimated.summary == 0
}

struct MaterializedReplay {
    estimates: Vec<Option<ContextEstimate>>,
}

fn materialize_full_history(transcript: &[Value], tool_schemas: &[u64]) -> MaterializedReplay {
    let mut pinned = Vec::new();
    let mut recent = Vec::new();
    let mut estimates = vec![None; tool_schemas.len()];
    let mut next_request = 0;
    let mut uncertain = false;
    let mut assistant_records: Vec<(usize, usize)> = Vec::new();

    for event in transcript {
        match event.get("op").and_then(Value::as_str) {
            Some("pin") => {
                if let Some(message) = event.get("msg") {
                    pinned.push(message.clone());
                } else {
                    uncertain = true;
                }
            }
            Some("record") => {
                if let Some(message) = event.get("msg") {
                    if message.get("role").and_then(Value::as_str) == Some("assistant")
                        && next_request < tool_schemas.len()
                    {
                        if !uncertain {
                            let pinned_tokens = crate::ledger::estimate::estimate_messages(&pinned);
                            let recent_tokens = crate::ledger::estimate::estimate_messages(&recent);
                            let tool_tokens = tool_schemas[next_request];
                            estimates[next_request] = Some(ContextEstimate {
                                pinned: pinned_tokens,
                                tool_schemas: tool_tokens,
                                summary: 0,
                                recent: recent_tokens,
                                total: pinned_tokens + tool_tokens + recent_tokens,
                            });
                        }
                        assistant_records.push((recent.len(), next_request));
                        next_request += 1;
                    }
                    recent.push(message.clone());
                } else {
                    uncertain = true;
                }
            }
            Some("truncate") => {
                if let Some(len) = event
                    .get("n")
                    .and_then(Value::as_u64)
                    .and_then(|len| usize::try_from(len).ok())
                {
                    let removed_requests: Vec<usize> = assistant_records
                        .iter()
                        .filter(|(message_index, _)| *message_index >= len)
                        .map(|(_, request_index)| *request_index)
                        .collect();
                    if !removed_requests.is_empty() {
                        for request_index in &removed_requests {
                            estimates[*request_index] = None;
                        }
                        assistant_records.retain(|(message_index, _)| *message_index < len);
                        next_request = next_request.saturating_sub(removed_requests.len());
                        // A completed `/undo` and a cancelled in-flight tool
                        // turn have the same transcript shape in ledger v1.
                        // Reuse the request slot but report a miss rather than
                        // assigning the rolled-back context to a later call.
                        uncertain = true;
                    }
                    recent.truncate(len);
                } else {
                    uncertain = true;
                }
            }
            Some("clear") => {
                recent.clear();
                assistant_records.clear();
            }
            Some("meta" | "read") => {}
            Some(_) | None => uncertain = true,
        }
    }

    MaterializedReplay { estimates }
}

fn rollup(estimates: &[ContextEstimate]) -> ContextRollup {
    let mut out = ContextRollup::default();
    for estimate in estimates {
        out.pinned += estimate.pinned;
        out.tool_schemas += estimate.tool_schemas;
        out.summary += estimate.summary;
        out.recent += estimate.recent;
        out.total += estimate.total;
    }
    out
}

fn delta(candidate: u64, control: u64) -> i64 {
    i128::from(candidate)
        .saturating_sub(i128::from(control))
        .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/ledger")
                .join(format!("{name}.json")),
        )
        .unwrap()
    }

    #[test]
    fn compacted_fixture_compares_recorded_linear_with_replayed_full_history() {
        let report = compare_bytes(&fixture("compacted")).unwrap();
        let encoded = serde_json::to_value(&report).unwrap();
        assert!(encoded["runs"][0]["arms"].is_array());
        assert!(encoded["runs"][0]["comparisons"].is_array());
        assert_eq!(report.schema, SCHEMA);
        assert_eq!(report.fixture_hash.len(), 64);
        assert_eq!(report.runs.len(), 1);

        let run = &report.runs[0];
        let control = &run.arms[0];
        let candidate = &run.arms[1];
        assert_eq!(control.arm, "full-history");
        assert_eq!(control.strategy, "full-history");
        assert_eq!(control.requests, 4);
        assert_eq!(control.strategy_internal_calls, 0);
        assert_eq!(control.materialization_misses, 0);
        assert_eq!(candidate.arm, "recorded:linear-with-compact");
        assert_eq!(candidate.strategy, "linear-with-compact");
        assert_eq!(candidate.requests, 4);
        assert_eq!(candidate.strategy_internal_calls, 1);
        assert_eq!(candidate.materialization_misses, 1);
        assert!(run.comparisons[0].candidate_minus_control.total.unwrap() < 0);
    }

    #[test]
    fn standalone_compaction_calls_remain_strategy_internal() {
        let mut value: Value = serde_json::from_slice(&fixture("compacted")).unwrap();
        let mut requests: Vec<&mut Value> = value["ledger"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .filter(|record| record["kind"] == "request")
            .collect();
        requests[3]["turn"] = json!(4);
        requests[4]["turn"] = json!(5);

        let report = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let candidate = &report.runs[0].arms[1];
        assert_eq!(candidate.requests, 4);
        assert_eq!(candidate.strategy_internal_calls, 1);
    }

    #[test]
    fn resumed_fixture_keeps_append_only_runs_separate_and_in_order() {
        let report = compare_bytes(&fixture("resumed")).unwrap();
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[0].run, "baseline-run-1");
        assert_eq!(report.runs[1].run, "baseline-run-2");
        assert_eq!(report.runs[0].resumed, Some(false));
        assert_eq!(report.runs[1].resumed, Some(true));
        assert_eq!(report.runs[0].arms[1].requests, 2);
        assert_eq!(report.runs[1].arms[1].requests, 1);
        assert_eq!(
            report.runs[1].comparisons[0].candidate_minus_control.total,
            Some(0)
        );
    }

    #[test]
    fn missing_summary_keeps_an_incomplete_run_in_the_report() {
        let mut value: Value = serde_json::from_slice(&fixture("fresh")).unwrap();
        value["ledger"]
            .as_array_mut()
            .unwrap()
            .retain(|record| record["kind"] != "summary");

        let report = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(report.runs.len(), 1);
        assert!(!report.runs[0].terminated);
        assert_eq!(report.runs[0].arms[1].requests, 1);
    }

    #[test]
    fn a_failed_run_with_no_successful_requests_is_not_discarded() {
        let summary = crate::ledger::Ledger::new(
            RunIdentity {
                session: "failed-session".into(),
                run: "failed-run".into(),
                provider: "baseline-provider".into(),
                model: "baseline-model".into(),
                strategy: "linear-with-compact".into(),
                strategy_version: 1,
                config_hash: "failed-config".into(),
            },
            crate::ledger::PromptAccounting::CacheInclusive,
        )
        .summary();
        let fixture = serde_json::to_vec(&json!({
            "transcript": [{"op": "record", "msg": {"role": "user", "content": "fail"}}],
            "ledger": [summary],
        }))
        .unwrap();

        let report = compare_bytes(&fixture).unwrap();
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.runs[0].run, "failed-run");
        assert!(report.runs[0].terminated);
        assert_eq!(report.runs[0].arms[0].requests, 0);
        assert_eq!(report.runs[0].arms[1].requests, 0);
    }

    #[test]
    fn a_run_that_changes_comparability_identity_is_rejected() {
        let mut value: Value = serde_json::from_slice(&fixture("long")).unwrap();
        let mut requests: Vec<&mut Value> = value["ledger"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .filter(|record| record["kind"] == "request")
            .collect();
        requests[1]["model"] = json!("different-model");

        let error = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("changes model"), "{error}");
    }

    #[test]
    fn transcript_metadata_must_match_a_pinned_ledger_identity() {
        let mut value: Value = serde_json::from_slice(&fixture("fresh")).unwrap();
        value["transcript"][0]["meta"]["model"] = json!("different-model");

        let error = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("transcript meta"), "{error}");
    }

    #[test]
    fn every_pinned_ledger_identity_must_match_transcript_metadata() {
        let mut value: Value = serde_json::from_slice(&fixture("fresh")).unwrap();
        let mut extra = value["ledger"][0].clone();
        extra["run"] = json!("unrelated-run");
        extra["model"] = json!("unrelated-model");
        value["ledger"].as_array_mut().unwrap().push(extra);

        let error = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("ledger identity"), "{error}");
    }

    #[test]
    fn unknown_state_events_turn_following_snapshots_into_explicit_misses() {
        let mut value: Value = serde_json::from_slice(&fixture("fresh")).unwrap();
        value["transcript"]
            .as_array_mut()
            .unwrap()
            .insert(2, json!({"op": "future-state-op"}));

        let report = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let run = &report.runs[0];
        assert_eq!(run.arms[0].materialization_misses, 1);
        assert!(run.arms[0].first_request.is_none());
        assert!(
            run.comparisons[0]
                .candidate_minus_control
                .first_request
                .is_none()
        );
        assert!(
            serde_json::to_value(report).unwrap()["runs"][0]["comparisons"][0]
                ["candidate_minus_control"]["total"]
                .is_null()
        );
    }

    #[test]
    fn malformed_truncate_events_turn_following_snapshots_into_explicit_misses() {
        let mut value: Value = serde_json::from_slice(&fixture("fresh")).unwrap();
        value["transcript"]
            .as_array_mut()
            .unwrap()
            .insert(2, json!({"op": "truncate"}));

        let report = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let run = &report.runs[0];
        assert_eq!(run.arms[0].materialization_misses, 1);
        assert!(run.arms[0].first_request.is_none());
        assert!(
            run.comparisons[0]
                .candidate_minus_control
                .first_request
                .is_none()
        );
        assert!(
            serde_json::to_value(report).unwrap()["runs"][0]["comparisons"][0]
                ["candidate_minus_control"]["total"]
                .is_null()
        );
    }

    #[test]
    fn rolled_back_assistants_do_not_consume_the_next_request_slot() {
        let mut value: Value = serde_json::from_slice(&fixture("fresh")).unwrap();
        value["transcript"].as_array_mut().unwrap().extend([
            json!({"op": "truncate", "n": 0}),
            json!({"op": "record", "msg": {"role": "user", "content": "retry"}}),
            json!({"op": "record", "msg": {"role": "assistant", "content": "done"}}),
        ]);

        let report = compare_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let run = &report.runs[0];
        assert_eq!(run.arms[0].requests, 1);
        assert_eq!(run.arms[0].materialization_misses, 1);
        assert!(run.arms[0].first_request.is_none());
        assert!(run.comparisons[0].candidate_minus_control.total.is_none());
    }

    #[test]
    fn comparing_a_path_does_not_modify_the_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.json");
        let before = fixture("fresh");
        std::fs::write(&path, &before).unwrap();

        compare_path(&path).unwrap();

        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn the_same_fixture_produces_byte_identical_json() {
        let bytes = fixture("long");
        let first = serde_json::to_vec(&compare_bytes(&bytes).unwrap()).unwrap();
        let second = serde_json::to_vec(&compare_bytes(&bytes).unwrap()).unwrap();
        assert_eq!(first, second);
    }
}
