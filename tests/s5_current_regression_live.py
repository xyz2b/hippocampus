#!/usr/bin/env python3
"""Current S5 development regression: one frozen operation per step, no retries."""
import argparse
import contextlib
import datetime
import fcntl
import json
import os
import sqlite3
import sys
from collections import Counter
from pathlib import Path

from s5_original_n05_fullchain import LongClient, clean_env, digest, receipt, save
from s5_normal_intake import settings
from s6_holdout_scoring import score_intake, score_contract, score_validate
from s6_holdout_live import plan, call, model_reports, runtime_checks, check_task_visibility

if sys.flags.optimize:
    raise RuntimeError("run without Python optimization; acceptance guards must remain enabled")
ROOT = Path(__file__).resolve().parents[1]
PACK = ROOT / "docs/refactor/s5/current-regression-20260927"
OWNER = "s5-current-regression-20260927"
DEPENDENCIES = (
    "tests/s5_current_regression_live.py", "tests/s5_current_regression_check.py",
    "tests/s6_holdout_live.py", "tests/s6_holdout_scoring.py",
    "tests/s5_original_n05_fullchain.py", "tests/mcp_smoke.py", "tests/s5_normal_intake.py",
    "tests/source_support_fixture.py",
    "docs/refactor/s6/holdout-v1-20260927/preflight.py",
)

def read(path):
    return json.loads(path.read_text())


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def formal_state():
    paths = {"profile": ".config/tunnel-client/personal-memory.yaml",
             "wrapper": ".local/bin/pmr-mcp-configured",
             "database": ".local/share/personal-memory-runtime/memory.sqlite3"}
    return {name: {"sha256": digest(Path.home() / path),
                   "mtime_ns": (Path.home() / path).stat().st_mtime_ns}
            for name, path in paths.items()}


def phase_config(budgets, scenario, phase):
    config = json.loads(json.dumps(budgets["runtime_defaults"]))
    config.update(budgets["phase_settings"][phase])
    config["model_budget"].update(budgets["groups"][scenario][phase])
    return config


def static_check(inputs, labels, budgets, *, allow_coverage_repair=False, allow_extraction_repair=False):
    """Freeze data, accounting and query labels without sending any case."""
    assert inputs["version"] == labels["version"] == 1
    scenarios = inputs["scenarios"]
    assert len(scenarios) == len({s["id"] for s in scenarios}) == 4
    assert len({s["category"] for s in scenarios}) == 4
    assert budgets["groups"].keys() == {s["id"] for s in scenarios}
    assert type(allow_coverage_repair) is bool
    assert type(allow_extraction_repair) is bool
    intake_cap = 4 if allow_coverage_repair or allow_extraction_repair else 2
    totals, query_counts = Counter(), Counter()
    all_ids = {key: set() for key in ("sources", "queries", "validations", "entities")}
    group_ids = []
    for scenario in scenarios:
        assert len(scenario["sources"]) == len(scenario["validations"]) == 2
        assert len(scenario["queries"]) == 6
        for key in all_ids:
            ids = [row["id"] for row in scenario[key]]
            assert len(set(ids)) == len(ids) and not set(ids) & all_ids[key]
            all_ids[key].update(ids)
        names = {e["canonical_name"] for e in scenario["entities"]}
        for source in scenario["sources"]:
            truth = labels["intake"][source["id"]]
            assert 0 < len(source["text"].encode()) <= 24000
            assert truth["subject"] in names and truth["subject"] in source["text"]
            assert truth["relation"] and truth["value"] in source["text"]
            assert truth["supporting_quote"] and truth["supporting_quote"] in source["text"]
        queries = {q["id"]: q for q in scenario["queries"]}
        for query in queries.values():
            truth = labels["queries"][query["id"]]
            assert 0 < len(query["query"].encode()) <= 24000
            assert query["after_source_count"] in (1, 2)
            assert isinstance(query.get("context", []), list)
            assert len(query.get("context", [])) <= 8
            assert all(isinstance(v, str) and 0 < len(v) <= 2000 for v in query.get("context", []))
            visible = {s["id"] for s in scenario["sources"][:query["after_source_count"]]}
            assert set(truth["required_source_refs"]) <= visible
            assert set(truth["forbidden_source_refs"]) <= {s["id"] for s in scenario["sources"]}
            assert not set(truth["required_source_refs"]) & set(truth["forbidden_source_refs"])
            assert truth["expected"] in ("answer", "clarify", "insufficient_evidence")
            query_counts[truth["expected"]] += 1
            if truth["expected"] == "answer":
                assert len(truth["answer_values"]) == len(truth["required_source_refs"]) == 1
                assert truth["answer_values"][0] == labels["intake"][truth["required_source_refs"][0]]["value"]
            else:
                assert truth["answer_values"] == truth["required_source_refs"] == []
        for case in scenario["validations"]:
            query = queries[case["query_id"]]
            truth, verdict = labels["queries"][query["id"]], labels["validations"][case["id"]]
            assert type(verdict["expected_approved"]) is bool
            assert 0 < len(case["draft_answer"].encode()) <= 4000
            assert set(case["source_refs"]) <= {s["id"] for s in scenario["sources"][:query["after_source_count"]]}
            if verdict["expected_approved"]:
                assert truth["expected"] == "answer"
                assert case["source_refs"] == truth["required_source_refs"]
                assert case["draft_answer"] == truth["answer_values"][0] + "。"
            else:
                assert truth["expected"] != "answer" or case["draft_answer"] != truth["answer_values"][0] + "。"
        for phase, key, cap in (("intake", "sources", intake_cap), ("recall", "queries", 1), ("validate", "validations", 1)):
            count = len(scenario[key])
            totals[phase] += count
            policy = budgets["groups"][scenario["id"]][phase]
            assert policy["max_calls"] == count * cap and policy["max_operation_calls"] == cap
            assert policy.get("allow_coverage_repair", False) is (allow_coverage_repair and phase == "intake")
            assert policy.get("allow_extraction_repair", False) is (allow_extraction_repair and phase == "intake")
            assert policy["group_id"] == f"{OWNER}-{scenario['id']}-{phase}"
            group_ids.append(policy["group_id"])
    for key, name in (("sources", "intake"), ("queries", "queries"), ("validations", "validations")):
        assert all_ids[key] == labels[name].keys()
    assert len(group_ids) == len(set(group_ids)) == 12
    calls = {phase: count * (intake_cap if phase == "intake" else 1) for phase, count in totals.items()}
    assert budgets["phase_max_physical_requests"] == calls
    assert budgets["total_max_physical_requests"] == sum(calls.values()) == (64 if intake_cap == 4 else 48)
    assert budgets["expected_query_counts"] == dict(query_counts)
    policy = budgets["runtime_defaults"]["model_budget"]
    assert policy.get("allow_coverage_repair", False) is False, "repair permission must be explicit in intake groups only"
    assert policy.get("allow_extraction_repair", False) is False, "repair permission must be explicit in intake groups only"
    assert policy["max_input_bytes"] == 65536
    assert policy["extraction_output_tokens"] == policy["review_output_tokens"] == 16384
    assert policy["query_output_tokens"] == 8192
    assert budgets["max_reserved_output_tokens"] == calls["intake"] * 16384 + (calls["recall"] + calls["validate"]) * 8192
    assert budgets["max_aggregate_input_bytes"] == sum(calls.values()) * 65536
    assert budgets["provider_base_url"] == "https://ark.cn-beijing.volces.com/api/v3"
    assert budgets["timeout_ms"] == 180000 and budgets["automatic_retries"] == 0
    assert budgets["phase_settings"] == {"intake": {"retrieval_mode": "local", "enrichment_mode": "inline"},
        "recall": {"retrieval_mode": "assisted", "enrichment_mode": "off"},
        "validate": {"retrieval_mode": "assisted", "enrichment_mode": "off"}}
    assert Counter(op["phase"] for op in plan(inputs)) == totals
    return dict(totals)


def check_intake_stages(saved, *, allow_coverage_repair=False, allow_extraction_repair=False):
    if saved.get("enrichment", {}).get("status") == "committed":
        calls = saved.get("model_calls", [])
        stages = [c["stage"] for c in calls]
        ordinary = ["extraction", "extraction_review"]
        repaired = ordinary + ["extraction_repair", "extraction_repair_review"]
        local_repair = ["extraction", "extraction_repair", "extraction_repair_review"]
        assert (stages == ordinary or (allow_coverage_repair or allow_extraction_repair) and stages == repaired
                or allow_extraction_repair and stages == local_repair), "committed intake lacks a complete authorized independent review sequence"
        assert all(c["outcome"] == "completed" for c in calls)


CALENDAR_CORRECTION_REASONS = frozenset({
    "unsupported expression must not provide a model calendar date",
    "model calendar date must be a valid YYYY-MM-DD",
    "relative day requires a model YYYY-MM-DD date",
    "model date does not match the USER expression and local anchor",
    "single-day expression must not provide an end date",
    "calendar interval requires model start and exclusive end dates",
    "model dates do not match the USER calendar interval",
    "calendar expiry requires a model inclusive YYYY-MM-DD date",
    "model expiry date does not match the USER calendar interval",
})


def check_extraction_correction_trace(calls, events, *, allow_coverage_repair=False, source_text=None):
    """Bind a correction to runtime validation and unchanged source input, never model claims alone."""
    stages = [c['stage'] for c in calls]
    local = ['extraction', 'extraction_repair', 'extraction_repair_review']
    reviewed = ['extraction', 'extraction_review', 'extraction_repair', 'extraction_repair_review']
    assert stages in (local[:2], local, reviewed[:3], reviewed), 'invalid correction stage order'
    assert len({c['operation_id'] for c in calls}) == 1 and len({c['call_id'] for c in calls}) == len(calls)
    assert all(c['outcome'] == 'completed' for c in calls), 'provider failure is terminal'
    operation = calls[0]['operation_id']
    bound = [e for e in events if e['run_id'] == operation]
    def data(kind, call):
        found = [e['data'] for e in bound if e['kind'] == kind and e['data']['call_id'] == call['call_id']]
        assert len(found) == 1, 'missing or repeated model event'
        return found[0]
    starts = [data('model_start', c) for c in calls]
    ends = [data('model_end', c) for c in calls]
    assert all(e['status'] == 'completed' and e['http_status'] == 200 and e['parsing_status'] == 'valid_json'
               and e.get('error') is None and e['response']['status'] == 'completed' for e in ends)
    inputs = [json.loads(e['request']['input']) for e in starts]
    original = inputs[0]
    assert not {'coverage_correction', 'extraction_correction'} & original.keys()
    source, history = original['current_user_turn'], original['conversation_history']
    assert isinstance(source, str) and source.strip()
    assert source_text is None or source == source_text, 'correction belongs to another source'
    repair_index = stages.index('extraction_repair')
    repair = inputs[repair_index]
    keys = {'coverage_correction', 'extraction_correction'} & repair.keys()
    assert len(keys) == 1 and {k: v for k, v in repair.items() if k not in keys} == original, 'source or source clock changed'
    validations = [e['data'] for e in bound if e['kind'] == 'validation'
        and e['data']['stage'] in ('extraction', 'extraction_repair', 'source_support')]
    def checked_review(index):
        request, review = inputs[index], ends[index]['parsed_result']
        assert {k: request[k] for k in original} == original
        assert review['review_binding'] == request['review_binding']
        count = len(request['frozen_candidates'])
        assert request['expected_review_count'] == count
        assert request['expected_candidate_indices'] == list(range(count))
        indices = [entry['candidate_index'] for entry in review['reviews']]
        assert all(type(i) is int for i in indices) and sorted(indices) == list(range(count))
        assert all(entry.get('current_user_quote') == source for entry in review['reviews'])
        return review
    if repair_index == 1:
        assert keys == {'extraction_correction'}
        reason = validations[0].get('error', '').removeprefix('invalid structured output: ')
        assert reason in CALENDAR_CORRECTION_REASONS
        assert validations[0]['error'] == 'invalid structured output: ' + reason
        feedback = repair['extraction_correction']
        issue, = feedback['issues']
        assert set(issue) == {'candidate_index', 'field', 'reason'} and issue['reason'] == reason
        index = issue['candidate_index']
        assert type(index) is int and 0 <= index < len(ends[0]['parsed_result']['candidates'])
        assert issue['field'] in ('fact_time', 'memory_request')
        expected = [('extraction', 'failed')]
    else:
        first = checked_review(1)
        checks = {'subject', 'relation', 'value', 'polarity', 'scope', 'temporal', 'intent', 'source', 'metadata'}
        assert all(set(e['checks']) == checks for e in first['reviews'])
        if keys == {'coverage_correction'}:
            assert allow_coverage_repair and first['coverage']['status'] == 'incomplete'
            missing = first['coverage']['missing_user_spans']
            assert missing and len(set(missing)) == len(missing) and all(s.strip() and s in source for s in missing)
            assert all(set(e['checks'].values()) == {'supported'} and not e['issues'] for e in first['reviews'])
            assert repair['coverage_correction']['missing_user_spans'] == missing
            assert validations[1].get('error') == 'source_coverage_incomplete'
            feedback = repair['coverage_correction']
            expected = [('extraction', 'completed'), ('source_support', 'failed')]
        else:
            assert first['coverage'] == {'status': 'complete', 'missing_user_spans': []}
            issues = []
            for entry in first['reviews']:
                bad = {k for k, v in entry['checks'].items() if v != 'supported'}
                assert bad <= {'value', 'scope', 'temporal', 'metadata'}
                assert all(entry['checks'][k] == 'contradicted' for k in bad)
                assert {issue['check'] for issue in entry['issues']} == bad
                for issue in entry['issues']:
                    check, field = issue['check'], issue['field']
                    assert (field == check or field.startswith(check + '.') if check != 'metadata' else
                        field in {'memory_type', 'certainty', 'extraction_confidence', 'strength', 'retention_tier',
                                  'explicit_memory_request', 'cross_turn_useful', 'tags'})
                    assert issue['reason'].strip() and any(s['turn_index'] == 0 for s in issue['evidence'])
                    assert all(type(s['turn_index']) is int and 0 <= s['turn_index'] <= len(history)
                        and s['quote'].strip() and s['quote'] in ([source] + history)[s['turn_index']] for s in issue['evidence'])
                    issues.append(dict(candidate_index=entry['candidate_index'],
                        **{k: issue[k] for k in ('field', 'reason', 'evidence')}))
            assert issues and repair['extraction_correction']['issues'] == issues
            feedback = repair['extraction_correction']
            expected = [('extraction', 'completed'), ('source_support', 'completed')]
    authority = ('untrusted_review_hints_recheck_against_entire_USER_turn' if 'coverage_correction' in keys
                 else 'untrusted_validation_feedback_recheck_against_entire_USER_turn')
    assert feedback['authority'] == authority and feedback['instruction'].strip()
    expected += [('extraction_repair', 'completed')]
    if stages[-1] == 'extraction_repair_review':
        expected += [('source_support', 'completed')]
        final_input = inputs[-1]
        assert not {'coverage_correction', 'extraction_correction'} & final_input.keys()
        assert {k: final_input[k] for k in original} == original
        if validations[-1]['status'] == 'completed':
            checked_review(len(calls)-1)
    assert len(validations) == len(expected), 'missing or additional runtime validation'
    # Only the final stage may refuse after the one correction; receipt inspection
    # classifies its exact reason. Audit never turns such a refusal into a pass.
    if validations[-1]['status'] == 'failed':
        expected[-1] = (expected[-1][0], 'failed')
    assert [(v['stage'], v['status']) for v in validations] == expected
    return validations


def check_extraction_correction_guards(inspect=None):
    """Synthetic trace/receipt checks only; no client, credentials, sockets, or provider."""
    import copy
    def rejects(action):
        try:
            action()
        except (AssertionError, KeyError, ValueError):
            return
        raise AssertionError('invalid correction trace accepted')
    text = '明天松风工作坊的报名费为47元。'
    reason = 'model date does not match the USER expression and local anchor'
    checks = dict.fromkeys('subject relation value polarity scope temporal intent source metadata'.split(), 'supported')
    def fixture(kind='local', failed=False):
        stages = (['extraction', 'extraction_repair'] if kind == 'local' else
                  ['extraction', 'extraction_review', 'extraction_repair'])
        if not failed:
            stages += ['extraction_repair_review']
        calls = [dict(call_id=f'call-{i}', operation_id='operation', stage=s, outcome='completed') for i, s in enumerate(stages)]
        original = dict(owner_id='synthetic-owner', now=1790555261, utc_offset_seconds=28800,
            current_user_turn=text, conversation_history=[])
        issue = dict(check='temporal', field='temporal.valid_from', reason='date differs from USER',
                     evidence=[dict(turn_index=0, quote='明天')])
        review = dict(review_binding='synthetic-binding', omission_supported=False,
            coverage=dict(status='complete', missing_user_spans=[]),
            reviews=[dict(candidate_index=0, current_user_quote=text, checks={**checks, 'temporal':'contradicted'}, issues=[issue])])
        if kind == 'coverage':
            review['coverage'] = dict(status='incomplete', missing_user_spans=[text])
            review['reviews'][0].update(checks=checks.copy(), issues=[])
        correction = dict(authority='untrusted_validation_feedback_recheck_against_entire_USER_turn',
            instruction='Re-extract once.', issues=[dict(candidate_index=0, field='fact_time', reason=reason)]
                if kind == 'local' else [dict(candidate_index=0, **{k: issue[k] for k in ('field', 'reason', 'evidence')})])
        key = 'extraction_correction'
        if kind == 'coverage':
            key = 'coverage_correction'
            correction = dict(authority='untrusted_review_hints_recheck_against_entire_USER_turn',
                instruction='Re-extract once.', missing_user_spans=[text])
        events = []
        for i, call in enumerate(calls):
            stage = call['stage']
            request = copy.deepcopy(original)
            if stage == 'extraction_repair':
                request[key] = copy.deepcopy(correction)
            elif stage.endswith('review'):
                request.update(frozen_candidates=[{}], expected_review_count=1,
                    expected_candidate_indices=[0], review_binding='synthetic-binding')
            parsed = copy.deepcopy(review) if stage.endswith('review') else dict(candidates=[{}])
            if stage == 'extraction_repair_review':
                parsed['coverage'] = dict(status='complete', missing_user_spans=[])
                parsed['reviews'][0].update(checks=checks.copy(), issues=[])
            events.append(dict(kind='model_start', run_id='operation', data=dict(call_id=call['call_id'],
                request=dict(input=json.dumps(request)))))
            events.append(dict(kind='model_end', run_id='operation', data=dict(call_id=call['call_id'],
                status='completed', http_status=200, parsing_status='valid_json', error=None,
                response=dict(status='completed'), parsed_result=parsed)))
            validation = dict(stage='source_support' if stage.endswith('review') else stage, status='completed')
            if stage == 'extraction' and kind == 'local' or failed and i == len(calls)-1:
                validation.update(status='failed', error='invalid structured output: ' + reason)
            if stage == 'extraction_review' and kind == 'coverage':
                validation.update(status='failed', error='source_coverage_incomplete')
            events.append(dict(kind='validation', run_id='operation', data=validation))
        saved = dict(model_calls=calls, enrichment=dict(status='failed', error='invalid_candidates')
            if failed else dict(status='committed', error=None))
        return saved, events
    def verify(pair):
        saved, events = pair
        check_extraction_correction_trace(saved['model_calls'], events, allow_coverage_repair=True, source_text=text)
        if inspect:
            inspect(saved, events, text, allow_extraction_repair=True)
    for kind in ('local', 'review', 'coverage'):
        for failed in (False, True):
            verify(fixture(kind, failed))
    local = fixture()
    for permitted in CALENDAR_CORRECTION_REASONS:
        saved, events = copy.deepcopy(local)
        events[2]['data']['error'] = 'invalid structured output: ' + permitted
        request = json.loads(events[3]['data']['request']['input'])
        request['extraction_correction']['issues'][0]['reason'] = permitted
        events[3]['data']['request']['input'] = json.dumps(request)
        verify((saved, events))
    for fault in ('schema_reason', 'wrong_field', 'wrong_index', 'clock', 'feedback_reason', 'stage',
                  'operation', 'provider', 'missing_end', 'missing_validation', 'second_correction'):
        saved, events = copy.deepcopy(local)
        request = json.loads(events[3]['data']['request']['input'])
        if fault == 'schema_reason': events[2]['data']['error'] = 'invalid structured output: missing field value'
        elif fault == 'wrong_field': request['extraction_correction']['issues'][0]['field'] = 'subject'
        elif fault == 'wrong_index': request['extraction_correction']['issues'][0]['candidate_index'] = 1
        elif fault == 'clock': request['now'] += 1
        elif fault == 'feedback_reason': request['extraction_correction']['issues'][0]['reason'] = 'invented'
        elif fault == 'stage': saved['model_calls'][1]['stage'] = 'extraction_review'
        elif fault == 'operation': saved['model_calls'][1]['operation_id'] = 'another'
        elif fault == 'provider': events[4]['data']['status'] = 'failed'
        elif fault == 'missing_end': events[4]['kind'] = 'unavailable'
        elif fault == 'missing_validation': events[2]['kind'] = 'unavailable'
        else: saved['model_calls'].append(copy.deepcopy(saved['model_calls'][1]))
        events[3]['data']['request']['input'] = json.dumps(request)
        rejects(lambda: verify((saved, events)))
    for fault in ('unverified', 'binding', 'quote', 'index', 'insufficient', 'subject', 'evidence', 'field', 'coverage'):
        saved, events = fixture('review')
        review = events[4]['data']['parsed_result']
        entry = review['reviews'][0]
        if fault == 'unverified': events[5]['data'].update(status='failed', error='malformed review')
        elif fault == 'binding': review['review_binding'] = 'wrong'
        elif fault == 'quote': entry['current_user_quote'] = text[:1]
        elif fault == 'index': entry['candidate_index'] = 1
        elif fault == 'insufficient': entry['checks']['temporal'] = 'insufficient'
        elif fault == 'subject': entry['checks']['subject'] = 'contradicted'
        elif fault == 'evidence': entry['issues'][0]['evidence'][0]['quote'] = 'invented'
        elif fault == 'field': entry['issues'][0]['field'] = 'subject_id'
        else: review['coverage']['status'] = 'uncertain'
        rejects(lambda: verify((saved, events)))
    for fault in ('binding', 'quote', 'index', 'count'):
        saved, events = fixture()
        if fault == 'count':
            request = json.loads(events[-3]['data']['request']['input'])
            request['expected_review_count'] = 2
            events[-3]['data']['request']['input'] = json.dumps(request)
        else:
            final = events[-2]['data']['parsed_result']
            if fault == 'binding': final['review_binding'] = 'wrong-binding'
            elif fault == 'quote': final['reviews'][0]['current_user_quote'] = text[:1]
            else: final['reviews'][0]['candidate_index'] = 1
        rejects(lambda: verify((saved, events)))
    saved, events = local
    rejects(lambda: check_intake_stages(saved, allow_coverage_repair=True))
    check_intake_stages(saved, allow_extraction_repair=True)
    if inspect:
        rejects(lambda: inspect(saved, events, text))
        for error in ('model_budget_rejected', 'invalid_candidates'):
            bad, events = fixture(failed=True)
            bad['enrichment']['error'] = error
            events[-1]['data']['error'] = 'invalid structured output: missing field value'
            rejects(lambda: inspect(bad, events, text, allow_extraction_repair=True))
    print(json.dumps(dict(status='correction_guards_passed', real_model_requests=0,
        flows=6, exact_local_reasons=len(CALENDAR_CORRECTION_REASONS), malformed_traces_rejected=24)))


def frozen():
    runtime, dataset = read(PACK / "runtime-freeze.json"), read(PACK / "dataset-freeze.json")
    for path, fingerprint in {**runtime["runtime_files"], **dataset["files"]}.items():
        assert digest(ROOT / path) == fingerprint, f"frozen file changed: {path}"
    assert set(DEPENDENCIES) <= runtime["runtime_files"].keys(), "missing runner fingerprint"
    assert {str((PACK / name).relative_to(ROOT)) for name in ("inputs.json", "labels.json", "budgets.json")} <= dataset["files"].keys()
    assert digest(Path(runtime["binary"]["path"])) == runtime["binary"]["sha256"]
    assert formal_state() == runtime["formal_baseline"], "formal S4 baseline changed"
    return runtime


def profile(db):
    with sqlite3.connect(db.as_uri() + "?mode=ro", uri=True) as connection:
        return json.loads(connection.execute("SELECT state_json FROM memory_profiles WHERE owner_id=?", (OWNER,)).fetchone()[0])


def audit(output, budgets):
    calls, parse_errors, events = [], [], []
    for scenario in budgets["groups"]:
        db = output / scenario / "business.sqlite3"
        if db.exists():
            with sqlite3.connect(db.as_uri() + "?mode=ro", uri=True) as connection:
                connection.row_factory = sqlite3.Row
                calls.extend({**dict(row), "scenario": scenario} for row in connection.execute("SELECT * FROM model_calls"))
    for path in sorted(output.glob("*/trace/*.jsonl")):
        for line in path.read_text().splitlines():
            try:
                events.append(json.loads(line))
            except ValueError:
                parse_errors.append(str(path.relative_to(output)))
    starts = [e["data"] for e in events if e["kind"] == "model_start"]
    ends = [e["data"] for e in events if e["kind"] == "model_end"]
    result = {"reserved_calls": len(calls), "physical_requests": len(starts), "ledger": calls,
              "completed_request_events": len(ends), "trace_parse_errors": parse_errors,
              "known_input_tokens": sum(row["input_tokens"] or 0 for row in calls),
              "known_output_tokens": sum(row["output_tokens"] or 0 for row in calls),
              "unknown_usage_requests": sum(row["usage_status"] != "known" for row in calls)}
    # Preserve reservations and attempted sends even when the checks below stop the run.
    save(output / "accounting.json", result)
    assert not parse_errors, "trace contains incomplete JSON"
    for scenario, phases in budgets["groups"].items():
        rows = [row for row in calls if row["scenario"] == scenario]
        configs = {row["group_id"]: row for row in phases.values()}
        assert all(row["group_id"] in configs and row["owner_id"] == OWNER for row in rows)
        for phase, config in phases.items():
            group = [row for row in rows if row["group_id"] == config["group_id"]]
            assert len(group) <= config["max_calls"], "scenario/phase cap exceeded"
            assert len({(r["operation_id"], r["stage"]) for r in group}) == len(group), "duplicate operation stage"
            assert all(n <= config["max_operation_calls"] for n in Counter(r["operation_id"] for r in group).values())
            cap = 16384 if phase == "intake" else 8192
            stages = {"extraction", "extraction_review"} if phase == "intake" else {"assistance"}
            if config.get("allow_coverage_repair", False) or config.get("allow_extraction_repair", False):
                assert phase == "intake" and config["max_operation_calls"] == 4
                stages |= {"extraction_repair", "extraction_repair_review"}
            assert all(row["stage"] in stages and row["output_token_limit"] == cap
                       and row["input_bytes"] <= 65536 for row in group)
            if config.get("allow_extraction_repair", False):
                for operation in {r['operation_id'] for r in group}:
                    operation_calls = [r for r in group if r['operation_id'] == operation]
                    if any(r['stage'] == 'extraction_repair' for r in operation_calls):
                        check_extraction_correction_trace(operation_calls, events,
                            allow_coverage_repair=config.get('allow_coverage_repair', False))
    assert len(calls) <= budgets["total_max_physical_requests"]
    assert sum(row["input_bytes"] for row in calls) <= budgets["max_aggregate_input_bytes"]
    assert sum(row["output_token_limit"] for row in calls) <= budgets["max_reserved_output_tokens"]
    ids = {row["call_id"] for row in calls}
    assert len(ids) == len(calls) == len(starts) == len(ends), "trace/ledger counts differ"
    assert ids == {e["call_id"] for e in starts} == {e["call_id"] for e in ends}
    reports = [report for path in output.glob("*/*-poll-*.json") for report in model_reports(read(path).get("structuredContent", {}))]
    assert ids == {r["call_id"] for r in reports}, "tool receipts/ledger differ"
    assert all(row["outcome"] == "completed" for row in calls), "provider request failed"
    by_id = {row["call_id"]: row for row in calls}
    for start in starts:
        request, row = start["request"], by_id[start["call_id"]]
        assert start["attempt"] == 1 and start["timeout_ms"] == 180000
        assert start["provider"] == "ark.cn-beijing.volces.com" and start["budget_group"] == row["group_id"]
        assert request["max_output_tokens"] == row["output_token_limit"]
        assert request["model"] == budgets["model"] and request["reasoning"]["effort"] == budgets["reasoning"]
    for end in ends:
        assert end["http_status"] == 200 and end["status"] == "completed"
        assert end["response"]["status"] == "completed" and end["parsing_status"] == "valid_json"
        assert end.get("error") is None
    assert all(e.get("data", {}).get("evidence_status") != "evidence_incomplete" for e in events)
    return result


def source_request(scenario, source):
    earlier = scenario["sources"][:scenario["sources"].index(source)]
    turn = {"text": source["text"], "session_id": f"{OWNER}-{scenario['id']}",
            "turn_id": source["id"], "capture_source": True,
            "history": [row["text"] for row in earlier if row.get("task_id") == source.get("task_id")]}
    if "task_id" in source:
        turn["task_id"] = source["task_id"]
    return {"user_turn": turn}


def query_request(query):
    return {key: query[key] for key in ("query", "context", "task_id") if key in query}


def summary(report):
    counts = {phase: dict(Counter(row["status"] for row in report["rows"] if row["phase"] == phase))
              for phase in ("intake", "recall", "validate")}
    return {"status": report["status"], "completed_steps": len(report["rows"]),
            "total_steps": sum(report["fixed_denominators"].values()), "counts": counts,
            "physical_requests": report.get("accounting", {}).get("physical_requests"),
            "last": report["rows"][-1] if report["rows"] else None}


def preflight(output):
    runtime = frozen()
    inputs, labels, budgets = [read(PACK / name) for name in ("inputs.json", "labels.json", "budgets.json")]
    counts = static_check(inputs, labels, budgets)
    dependencies = {str(path): runtime["runtime_files"][path] for path in DEPENDENCIES}
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    operations = plan(inputs)
    assert Counter(op["phase"] for op in operations) == counts
    configs = {}
    for scenario in inputs["scenarios"]:
        directory = output / scenario["id"]
        directory.mkdir(mode=0o700)
        (directory / "trace").mkdir(mode=0o700)
        for phase in ("intake", "recall", "validate"):
            path = settings(directory, phase, **phase_config(budgets, scenario["id"], phase))
            configs[str(path.relative_to(output))] = digest(path)
    manifest = {"created_at_utc": now(), "output": str(output), "owner": OWNER,
                "dataset_freeze_sha256": digest(PACK / "dataset-freeze.json"), "runtime": runtime,
                "fingerprints": dependencies,
                "budgets": budgets, "operations": operations, "configs": configs, "preflight": "passed"}
    save(output / "manifest.json", manifest, exclusive=True)
    save(output / "report.json", {"status": "ready", "rows": [], "source_ids": {}, "blocked_scenarios": [],
                                  "fixed_denominators": counts}, exclusive=True)
    print(json.dumps({"status": "preflight_passed", "operations": len(operations), "real_model_requests": 0}))


def verify(output, manifest):
    assert str(output) == manifest["output"] and manifest["owner"] == OWNER
    assert digest(PACK / "dataset-freeze.json") == manifest["dataset_freeze_sha256"]
    assert frozen() == manifest["runtime"]
    for path, fingerprint in manifest["fingerprints"].items():
        assert digest(ROOT / path) == fingerprint, f"runner changed: {path}"
    for path, fingerprint in manifest["configs"].items():
        assert digest(output / path) == fingerprint, f"configuration changed: {path}"
    assert manifest["budgets"] == read(PACK / "budgets.json")
    authorization = read(output / "authorization.json")
    assert authorization["manifest_sha256"] == digest(output / "manifest.json")
    assert authorization["output"] == str(output) and authorization["max_physical_requests"] == manifest["budgets"]["total_max_physical_requests"]
    assert authorization["provider_base_url"] == manifest["budgets"]["provider_base_url"]
    assert authorization["automatic_retries"] == 0 and authorization["user_reply"]
    window = manifest["budgets"]["execution_window"]
    assert datetime.datetime.fromisoformat(window["start_inclusive"]) <= datetime.datetime.now(datetime.timezone.utc) < datetime.datetime.fromisoformat(window["end_exclusive"])


def apply_review(output, report):
    last = report["rows"][-1]
    raw_path = output / last["artifact"]
    review = read(raw_path.with_name(raw_path.stem + "-review.json"))
    assert review["artifact_sha256"] == digest(raw_path) and review["reason"].strip()
    assert review["id"] == last["id"] and review["phase"] == last["phase"]
    assert review["verdict"] in ("passed", "quality_gap", "safety_error")
    last["review"] = review
    last["status"] = review["verdict"]
    if last["status"] == "safety_error":
        report["status"] = "stopped"
    else:
        report["status"] = "ready"
        if last["phase"] == "intake" and last["status"] == "quality_gap":
            report["blocked_scenarios"].append(last["scenario"])


def step(output):
    with (output / "runner.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest, report = read(output / "manifest.json"), read(output / "report.json")
        verify(output, manifest)
        assert report["status"] in ("ready", "review_required"), "run already stopped or complete; do not restart"
        for index, row in enumerate(report["rows"]):
            assert all(row[key] == manifest["operations"][index][key] for key in ("id", "scenario", "phase"))
            if "artifact" in row:
                assert digest(output / row["artifact"]) == row["artifact_sha256"], "receipt changed"
        if report["status"] == "review_required":
            apply_review(output, report)
        if report["status"] == "stopped":
            save(output / "report.json", report)
            print(json.dumps(summary(report), ensure_ascii=False))
            return
        index = len(report["rows"])
        if index == len(manifest["operations"]):
            report["status"] = "passed" if all(r["status"] == "passed" for r in report["rows"]) else "quality_gaps"
            save(output / "report.json", report)
            print(json.dumps(summary(report), ensure_ascii=False))
            return
        op = manifest["operations"][index]
        inputs, labels = read(PACK / "inputs.json"), read(PACK / "labels.json")
        scenario = next(s for s in inputs["scenarios"] if s["id"] == op["scenario"])
        directory = output / scenario["id"]
        db = directory / "business.sqlite3"
        artifact = directory / f"{index:02d}-{op['id']}.json"
        row = {**op, "started_at_utc": now()}
        try:
            report["accounting"] = audit(output, manifest["budgets"])
            if op["phase"] != "intake" and scenario["id"] in report["blocked_scenarios"]:
                row["status"] = "blocked_by_upstream"
            else:
                policy = manifest["budgets"]["groups"][op["scenario"]][op["phase"]]
                cap = policy["max_operation_calls"]
                assert report["accounting"]["reserved_calls"] + cap <= manifest["budgets"]["total_max_physical_requests"], "insufficient remaining budget for this operation"
                if op["phase"] == "intake" and op["after_source_count"] == 1:
                    assert not db.exists(), "first source requires a fresh scenario database"
                else:
                    assert db.exists(), "never recreate a missing scenario database"
                env = {k: v for k, v in clean_env("live").items() if not k.startswith("ARK_")}
                env["PMR_TRACE_DIR"] = str(directory / "trace")
                # A crash after this marker can never replay the operation automatically.
                artifact.with_suffix(".started").touch(exist_ok=False)
                with contextlib.closing(LongClient(manifest["runtime"]["binary"]["path"], db, env, OWNER,
                                                   config=directory / f"{op['phase']}.json")) as client:
                    bootstrap = client.call("memory_recall", {"query": "全局回答要求", "bootstrap": True})
                    assert bootstrap["model_calls"] == []
                    if op["phase"] == "intake" and op["after_source_count"] == 1:
                        for entity in scenario["entities"]:
                            client.call("memory_entity_upsert", entity)
                    collection = {"intake": "sources", "recall": "queries", "validate": "validations"}[op["phase"]]
                    case = next(item for item in scenario[collection] if item["id"] == op["id"])
                    if op["phase"] == "intake":
                        request, method = source_request(scenario, case), "memory_prepare"
                    else:
                        query = case if op["phase"] == "recall" else next(q for q in scenario["queries"] if q["id"] == case["query_id"])
                        request, method = query_request(query), "memory_recall"
                        if op["phase"] == "validate":
                            request.update(draft_answer=case["draft_answer"], source_ids=[report["source_ids"][ref] for ref in case["source_refs"]])
                            method = "memory_validate_answer"
                    before = profile(db)
                    result = call(client, method, request, artifact)
                    state = profile(db)
                    client.errors.seek(0)
                    stderr = client.errors.read()
                    data = {"request": request, "result": result, "before_revision": before["revision"],
                            "before_source_revision": before["source_revision"], "completed_at_utc": now(), "stderr": stderr}
                    if op["phase"] == "intake":
                        data["profile"] = state
                    save(artifact, data, exclusive=True)
                row.update(artifact=str(artifact.relative_to(output)), artifact_sha256=digest(artifact))
                report["accounting"] = audit(output, manifest["budgets"])
                assert "evidence_incomplete" not in stderr, "trace write failed"
                runtime_checks(result)
                sources = {s["id"]: {**s, "label": labels["intake"][s["id"]]} for s in scenario["sources"]}
                if op["phase"] == "intake":
                    saved = receipt(result)
                    record = next(s for s in state["sources"].values() if s["turn_id"] == case["id"])
                    assert record["request"] == {**request["user_turn"], "task_id": request["user_turn"].get("task_id")}
                    report["source_ids"][case["id"]] = saved["source"]["source_id"]
                    memories = state["repository"]["memories"]
                    memories = list(memories.values()) if isinstance(memories, dict) else memories
                    score = score_intake(scenario, case, saved, memories, labels,
                                         source_record={**record["request"], "source_id": record["id"], "enrichment": record["enrichment"]})
                    assert result["recall"]["status"] == "completed" and result["recall"]["result"]["model_calls"] == []
                    check_intake_stages(saved, allow_coverage_repair=policy.get("allow_coverage_repair", False),
                                        allow_extraction_repair=policy.get("allow_extraction_repair", False))
                    row["observed"] = {"remember": saved, "memories": [m for m in memories if any(e["turn_id"] == case["id"] for e in m.get("evidence", []))]}
                else:
                    assert (before["revision"], before["source_revision"]) == (state["revision"], state["source_revision"]), "query changed profile"
                    snapshot = {"read_revision": state["revision"], "source_revision": state["source_revision"]}
                    if op["phase"] == "recall":
                        score = score_contract(result["answer_contract"], query, labels["queries"][case["id"]], report["source_ids"], sources, expected_revision=snapshot)
                    else:
                        prior = next(r for r in report["rows"] if r["id"] == case["query_id"])
                        prior_contract = read(output / prior["artifact"])["result"]["answer_contract"]
                        revision = {key: prior_contract[key] for key in ("read_revision", "source_revision")}
                        assert revision == snapshot, "validation not at the query's actual snapshot"
                        score = score_validate(result, case, query, labels["queries"][query["id"]], labels["validations"][case["id"]],
                                               report["source_ids"], sources, expected_revision=revision)
                    if check_task_visibility(result, query, sources, report["source_ids"]):
                        score["safety_error"].append("task_leak: out-of-scope source exposed outside answer_contract")
                    contract = result["answer_contract"] if op["phase"] == "recall" else result
                    row["observed"] = {key: contract.get(key) for key in ("status", "safe_answer", "safe_source_ids", "sources", "approved", "approved_source_ids")}
                row["score"] = score
                # Every actual result is inspected before the next frozen operation.
                row["status"] = "safety_error" if score["safety_error"] else "review_required"
                if row["status"] == "safety_error":
                    report["status"] = "stopped"
                elif row["status"] == "review_required":
                    report["status"] = "review_required"
                elif row["status"] == "quality_gap" and op["phase"] == "intake":
                    report["blocked_scenarios"].append(op["scenario"])
        except Exception as error:
            row["status"] = "runtime_error"
            row["error"] = type(error).__name__ + ": " + str(error)
            report["status"] = "stopped"
        finally:
            row["completed_at_utc"] = now()
            report["rows"].append(row)
            report["formal_baseline_unchanged"] = formal_state() == manifest["runtime"]["formal_baseline"]
            if not report["formal_baseline_unchanged"]:
                report["status"] = "stopped"
            try:
                report["accounting"] = audit(output, manifest["budgets"])
            except Exception as error:
                report["status"] = "stopped"
                report["accounting_error"] = str(error)
                if (output / "accounting.json").exists():
                    report["accounting"] = read(output / "accounting.json")
            save(output / "report.json", report)
        print(json.dumps(summary(report), ensure_ascii=False), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("preflight", "step", "status"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    output = args.output.resolve()
    if args.phase == "preflight":
        preflight(output)
    elif args.phase == "step":
        step(output)
    else:
        print(json.dumps(summary(read(output / "report.json")), ensure_ascii=False))
