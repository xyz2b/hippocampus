#!/usr/bin/env python3
"""Six isolated source intakes; manual temporal review before each next source."""
import argparse
import contextlib
import datetime
import fcntl
import json
import os
from collections import Counter
from pathlib import Path

import s5_current_regression_live as runner

runner.PACK = runner.ROOT / 'docs/refactor/s5/temporal-grounding-specialist-20260927'
runner.OWNER = 's5-temporal-grounding-specialist-20260927'
runner.DEPENDENCIES += ('tests/s5_temporal_grounding_live.py',)
INTAKE_MAX_CALLS = 2  # New specialist wrappers must explicitly opt into the four-stage contract.


def plan():
    assert INTAKE_MAX_CALLS in (2, 4)
    inputs, labels, budgets = [runner.read(runner.PACK / name) for name in ('inputs.json', 'labels.json', 'budgets.json')]
    scenarios = inputs['scenarios']
    assert inputs['version'] == labels['version'] == 1
    assert len(scenarios) == len({s['id'] for s in scenarios}) == 6
    source_ids, entity_ids = set(), set()
    for scenario in scenarios:
        assert set(scenario) == {'id', 'entities', 'sources'}
        assert len(scenario['sources']) == 1
        source, = scenario['sources']
        assert set(source) == {'id', 'text'} and source['id'] not in source_ids
        source_ids.add(source['id'])
        assert 0 < len(source['text'].encode()) <= 24000
        for entity in scenario['entities']:
            assert set(entity) == {'id', 'canonical_name', 'aliases', 'entity_type'}
            assert entity['id'] not in entity_ids
            entity_ids.add(entity['id'])
            assert entity['canonical_name'] in source['text'] and entity['aliases'] == []
        truth = labels['intake'][source['id']]
        assert truth['supporting_source'] == source['text'] and truth['expected_assertions']
        for assertion in truth['expected_assertions']:
            assert assertion['subject'] in {e['canonical_name'] for e in scenario['entities']}
            assert assertion['relation'] in source['text'] and assertion['value'] in source['text']
        policy = dict(group_id=f"{runner.OWNER}-{scenario['id']}-intake",
            max_calls=INTAKE_MAX_CALLS, max_operation_calls=INTAKE_MAX_CALLS)
        if INTAKE_MAX_CALLS == 4:
            policy['allow_coverage_repair'] = True
        extraction_repair = budgets['groups'][scenario['id']]['intake'].get('allow_extraction_repair', False)
        assert type(extraction_repair) is bool
        if extraction_repair:
            assert INTAKE_MAX_CALLS == 4
            policy['allow_extraction_repair'] = True
        assert budgets['groups'][scenario['id']] == {'intake': policy}
    assert source_ids == labels['intake'].keys() == {f'T{i:02d}' for i in range(1, 7)}
    assert budgets['groups'].keys() == {s['id'] for s in scenarios}
    total = 6 * INTAKE_MAX_CALLS
    assert budgets['total_max_physical_requests'] == budgets['phase_max_physical_requests']['intake'] == total
    assert budgets['max_reserved_output_tokens'] == total * 16384
    assert budgets['max_aggregate_input_bytes'] == total * 65536
    assert budgets['provider_base_url'] == 'https://ark.cn-beijing.volces.com/api/v3'
    assert budgets['timeout_ms'] == 180000 and budgets['automatic_retries'] == 0
    assert budgets['runtime_defaults']['utc_offset_seconds'] == labels['utc_offset_seconds'] == 28800
    policy = budgets['runtime_defaults']['model_budget']
    assert policy['max_input_bytes'] == 65536
    assert policy['extraction_output_tokens'] == policy['review_output_tokens'] == 16384
    assert budgets['phase_settings'] == {'intake': {'retrieval_mode': 'local', 'enrichment_mode': 'inline'}}
    return scenarios, budgets


def preflight(output):
    runtime = runner.frozen()
    scenarios, budgets = plan()
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    configs = {}
    for scenario in scenarios:
        directory = output / scenario['id']
        directory.mkdir(mode=0o700)
        (directory / 'trace').mkdir(mode=0o700)
        config = runner.settings(directory, 'intake', **runner.phase_config(budgets, scenario['id'], 'intake'))
        configs[str(config.relative_to(output))] = runner.digest(config)
    manifest = dict(created_at_utc=runner.now(), output=str(output), owner=runner.OWNER,
        runtime=runtime, dataset_freeze_sha256=runner.digest(runner.PACK / 'dataset-freeze.json'),
        budgets=budgets, configs=configs, operations=[dict(id=s['sources'][0]['id'], scenario=s['id'], phase='intake') for s in scenarios], preflight='passed')
    runner.save(output / 'manifest.json', manifest, exclusive=True)
    runner.save(output / 'report.json', dict(status='ready', rows=[], fixed_denominator=6), exclusive=True)
    print(json.dumps(dict(status='preflight_passed', operations=6, real_model_requests=0)))


def verify(output, manifest):
    assert manifest['output'] == str(output) and manifest['owner'] == runner.OWNER
    assert manifest['preflight'] == 'passed' and manifest['runtime'] == runner.frozen()
    scenarios, budgets = plan()
    assert manifest['budgets'] == budgets
    assert manifest['operations'] == [dict(id=s['sources'][0]['id'], scenario=s['id'], phase='intake') for s in scenarios]
    assert manifest['dataset_freeze_sha256'] == runner.digest(runner.PACK / 'dataset-freeze.json')
    assert manifest['configs'].keys() == {s['id'] + '/intake.json' for s in scenarios}
    for path, fingerprint in manifest['configs'].items():
        assert runner.digest(output / path) == fingerprint, 'configuration changed'
    auth = runner.read(output / 'authorization.json')
    assert auth['manifest_sha256'] == runner.digest(output / 'manifest.json')
    assert auth['output'] == str(output) and auth['user_reply'].strip()
    assert auth['max_physical_requests'] == budgets['total_max_physical_requests'] == 6 * INTAKE_MAX_CALLS
    assert auth['provider_base_url'] == budgets['provider_base_url'] and auth['automatic_retries'] == 0
    window = budgets['execution_window']
    assert (datetime.datetime.fromisoformat(window['start_inclusive']) <= datetime.datetime.now(datetime.timezone.utc)
            < datetime.datetime.fromisoformat(window['end_exclusive']))


def apply_review(output, report):
    last = report['rows'][-1]
    artifact = output / last['artifact']
    path = artifact.with_name(artifact.stem + '-review.json')
    review = runner.read(path)
    assert review['artifact_sha256'] == runner.digest(artifact) == last['artifact_sha256']
    assert review['id'] == last['id'] and review['phase'] == 'intake' and review['reason'].strip()
    assert review['verdict'] in ('passed', 'quality_gap', 'safety_error', 'runtime_error')
    # ponytail: semantic inspection stays manual; this runner only binds its decision to evidence.
    for field in ('source_and_evidence', 'assertion_coverage', 'candidate_fields', 'temporal_and_retention'):
        assert isinstance(review[field], str) and review[field].strip(), f'missing manual review: {field}'
    last.update(review=review, review_sha256=runner.digest(path), status=review['verdict'])
    report['status'] = 'stopped' if review['verdict'] in ('safety_error', 'runtime_error') else 'ready'


def inspect_repair_validations(saved, events, source_text, *, allow_extraction_repair=False, allow_coverage_repair=True):
    """Exact local semantic gates only; arbitrary schema, provider and budget errors stop."""
    calls, enrichment = saved['model_calls'], saved['enrichment']
    count = len(calls)
    stages = ['extraction', 'extraction_review', 'extraction_repair', 'extraction_repair_review']
    if allow_extraction_repair and any(c['stage'] == 'extraction_repair' for c in calls):
        validations = runner.check_extraction_correction_trace(calls, events,
            allow_coverage_repair=allow_coverage_repair, source_text=source_text)
        if enrichment['status'] == 'failed':
            assert validations[-1]['status'] == 'failed'
            if calls[-1]['stage'] == 'extraction_repair':
                assert enrichment.get('error') == 'invalid_candidates'
                allowed = {'invalid structured output: ' + reason for reason in runner.CALENDAR_CORRECTION_REASONS}
            else:
                allowed = ({'source_coverage_incomplete'} if enrichment.get('error') == 'source_coverage_incomplete' else
                    {'invalid structured output: temporal metadata lacks locally grounded USER support'}
                    if enrichment.get('error') == 'invalid_candidates' else set())
            assert validations[-1].get('error') in allowed, 'unclassified candidate failure after correction'
        else:
            assert calls[-1]['stage'] == 'extraction_repair_review' and validations[-1]['status'] == 'completed'
        return
    assert 1 <= count <= 4 and [c['stage'] for c in calls] == stages[:count]
    validations = [e['data'] for e in events if e['kind'] == 'validation'
        and e['run_id'] == calls[0]['operation_id']
        and e['data']['stage'] in ('extraction', 'extraction_repair', 'source_support')]
    expected = [('extraction', 'completed'), ('source_support', 'completed')]
    if count > 2:
        expected[1] = ('source_support', 'failed')
        expected += [('extraction_repair', 'completed'), ('source_support', 'completed')]
        assert len(validations) >= 2 and validations[1].get('error') == 'source_coverage_incomplete'
        first_reviews = [e['data']['parsed_result'] for e in events if e['kind'] == 'model_end'
            and e['run_id'] == calls[0]['operation_id'] and e['data']['call_id'] == calls[1]['call_id']]
        assert len(first_reviews) == 1, 'correction lacks its first independent review'
        first = first_reviews[0]
        missing = first['coverage']['missing_user_spans']
        assert first['coverage']['status'] == 'incomplete' and missing and len(set(missing)) == len(missing)
        assert all(span.strip() and span in source_text for span in missing)
        checks = {'subject', 'relation', 'value', 'polarity', 'scope', 'temporal', 'intent', 'source', 'metadata'}
        assert all(set(entry['checks']) == checks and set(entry['checks'].values()) == {'supported'}
            and entry.get('issues') == [] for entry in first['reviews']), 'rejected or uncertain review cannot trigger repair'
    if enrichment['status'] == 'failed':
        error = enrichment.get('error')
        if count % 2:
            assert error == 'invalid_candidates', 'runtime extraction failure'
            reasons = {
                'model temporal boundaries must be null; copy USER temporal_expression or expiry_expression',
                'relative day requires a model YYYY-MM-DD date',
                'model date does not match the USER expression and local anchor',
                'single-day expression must not provide an end date',
                'calendar interval requires model start and exclusive end dates',
                'model dates do not match the USER calendar interval',
                'calendar expiry requires a model inclusive YYYY-MM-DD date',
                'model expiry date does not match the USER calendar interval',
                'calendar dates require their original temporal expression',
                'expiry_date requires its original retention expression',
                'explicit memory retention requires explicit_memory_request=true',
                'retention requires a supported model-normalized calendar date',
                'expiry date must be a finite future deadline',
            }
            allowed = {'invalid structured output: ' + reason for reason in reasons}
        else:
            allowed = ({'source_coverage_incomplete'} if error == 'source_coverage_incomplete' else
                {'invalid structured output: temporal metadata lacks locally grounded USER support'} if error == 'invalid_candidates' else set())
        expected = expected[:count]
        expected[-1] = (stages[count-1] if count % 2 else 'source_support', 'failed')
        assert validations and validations[-1].get('error') in allowed, 'unclassified candidate failure'
    else:
        assert count in (2, 4), 'completed intake lacks independent review'
    assert [(v['stage'], v['status']) for v in validations] == expected, 'validation sequence differs'


def inspect_receipt(result, state, source, request, physical_requests, events=(), *,
                    allow_extraction_repair=False, allow_coverage_repair=True):
    """Mechanical checks only; safe admission refusals require manual raw-output diagnosis."""
    saved = runner.receipt(result)
    assert (saved['source']['status'], saved['index']['status']) == ('stored', 'ready')
    assert result['recall']['status'] == 'completed' and result['recall']['result']['model_calls'] == []
    record, = state['sources'].values()
    assert record['request'] == {**request['user_turn'], 'task_id': None}
    assert record['turn_id'] == source['id'] and record['id'] == saved['source']['source_id']
    calls = saved['model_calls']
    assert len(calls) == physical_requests, 'operation receipt/physical request counts differ'
    assert len({c['operation_id'] for c in calls}) == 1
    assert len({c['call_id'] for c in calls}) == len(calls)
    assert all(c['outcome'] == 'completed' for c in calls)
    enrichment = saved['enrichment']
    assert enrichment['status'] in ('committed', 'completed_no_facts', 'completed_rejected', 'failed')
    assert record['enrichment'] == enrichment['status'] and record['error'] == enrichment.get('error')
    if INTAKE_MAX_CALLS == 4:
        inspect_repair_validations(saved, events, source['text'],
            allow_extraction_repair=allow_extraction_repair, allow_coverage_repair=allow_coverage_repair)
    if enrichment['status'] == 'failed' and INTAKE_MAX_CALLS == 2:
        # Only these exact local admission gates, bound to this operation's trace,
        # may reach manual review. Broken coverage/schema responses remain fatal.
        gates = {
            ('invalid_candidates', 1): ('extraction', 'invalid structured output: model temporal boundaries must be null; copy USER temporal_expression or expiry_expression'),
            ('invalid_candidates', 2): ('source_support', 'invalid structured output: temporal metadata lacks locally grounded USER support'),
            ('source_coverage_incomplete', 2): ('source_support', 'source_coverage_incomplete'),
        }
        key = (enrichment.get('error'), len(calls))
        assert key in gates, 'runtime enrichment failure'
        stage, reason = gates[key]
        validations = [e['data'] for e in events if e['kind'] == 'validation'
            and e['run_id'] == calls[0]['operation_id']
            and e['data']['stage'] in ('extraction', 'source_support')]
        expected = [('extraction', 'completed')] if len(calls) == 2 else []
        assert [(v['stage'], v['status']) for v in validations] == expected + [(stage, 'failed')]
        assert validations[-1]['error'] == reason, 'unclassified candidate failure'
    if enrichment['status'] == 'failed':
        assert saved['status'] == 'failed' and saved['error'] == enrichment['error']
    else:
        assert saved['status'] == result['remember']['status'] == 'completed'
        assert enrichment.get('error') is None
    if INTAKE_MAX_CALLS == 2:
        stages = {'extraction': 1}
        if enrichment['status'] != 'failed' or len(calls) == 2:
            stages['extraction_review'] = 1
        assert Counter(c['stage'] for c in calls) == stages
    if enrichment['status'] != 'committed':
        assert not state['repository']['memories'] and not state['staging']['pending'], 'rejected source changed facts or staging'
    if enrichment['status'] == 'completed_rejected':
        assert saved['outcomes'] and all(
            isinstance(o.get('Ingested'), dict) and isinstance(o['Ingested'].get('Rejected'), str)
            for o in saved['outcomes'])
    elif enrichment['status'] in ('failed', 'completed_no_facts'):
        assert not saved.get('outcomes')
    # Keep recall/diagnostic failures fatal even when one known admission gate refused.
    runner.runtime_checks({k: v for k, v in saved.items() if k not in ('status', 'enrichment')})
    runner.runtime_checks(result['recall'])
    return saved


def step(output):
    with (output / 'runner.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest, report = [runner.read(output / name) for name in ('manifest.json', 'report.json')]
        verify(output, manifest)
        assert report['status'] in ('ready', 'review_required'), 'run already stopped or complete'
        assert len(report['rows']) <= 6
        for index, row in enumerate(report['rows']):
            assert all(row[k] == manifest['operations'][index][k] for k in ('id', 'scenario', 'phase'))
            artifact = output / row['artifact']
            assert runner.digest(artifact) == row['artifact_sha256'], 'receipt changed'
            if 'review_sha256' in row:
                assert runner.digest(artifact.with_name(artifact.stem + '-review.json')) == row['review_sha256']
        if report['status'] == 'review_required':
            apply_review(output, report)
        if report['status'] == 'stopped' or len(report['rows']) == 6:
            if report['status'] != 'stopped':
                report['status'] = 'passed' if all(r['status'] == 'passed' for r in report['rows']) else 'quality_gaps'
            runner.save(output / 'report.json', report)
            return status(output)
        index = len(report['rows'])
        op = manifest['operations'][index]
        scenarios, budgets = plan()
        scenario = scenarios[index]
        source, = scenario['sources']
        directory = output / scenario['id']
        db, artifact = directory / 'business.sqlite3', directory / f"{index:02d}-{source['id']}.json"
        row = dict(op, started_at_utc=runner.now())
        try:
            before_audit = runner.audit(output, budgets)
            assert before_audit['reserved_calls'] + INTAKE_MAX_CALLS <= budgets['total_max_physical_requests']
            assert not db.exists() and not artifact.exists(), 'fresh independent source database required'
            artifact.with_suffix('.started').touch(exist_ok=False)
            report['status'] = 'running'
            runner.save(output / 'report.json', report)
            env = {k: v for k, v in runner.clean_env('live').items() if not k.startswith('ARK_')}
            env['PMR_TRACE_DIR'] = str(directory / 'trace')
            with contextlib.closing(runner.LongClient(manifest['runtime']['binary']['path'], db, env,
                                      runner.OWNER, config=directory / 'intake.json')) as client:
                assert client.call('memory_recall', {'query': '全局回答要求', 'bootstrap': True})['model_calls'] == []
                for entity in scenario['entities']:
                    client.call('memory_entity_upsert', entity)
                request = runner.source_request(scenario, source)
                assert request['user_turn']['history'] == []
                result = runner.call(client, 'memory_prepare', request, artifact)
                state = runner.profile(db)
                client.errors.seek(0)
                stderr = client.errors.read()
                runner.save(artifact, dict(request=request, result=result, profile=state,
                    completed_at_utc=runner.now(), stderr=stderr), exclusive=True)
            row.update(artifact=str(artifact.relative_to(output)), artifact_sha256=runner.digest(artifact))
            accounting = runner.audit(output, budgets)
            physical_requests = accounting['physical_requests'] - before_audit['physical_requests']
            assert 'evidence_incomplete' not in stderr
            events = [json.loads(line) for path in sorted((directory / 'trace').glob('*.jsonl'))
                for line in path.read_text().splitlines()]
            policy = budgets['groups'][scenario['id']]['intake']
            saved = inspect_receipt(result, state, source, request, physical_requests, events,
                allow_extraction_repair=policy.get('allow_extraction_repair', False),
                allow_coverage_repair=policy.get('allow_coverage_repair', False))
            row['observed_enrichment'] = saved['enrichment']
            row['physical_requests'] = physical_requests
            row['status'] = report['status'] = 'review_required'
        except Exception as error:
            row.update(status='runtime_error', error=type(error).__name__ + ': ' + str(error))
            report['status'] = 'stopped'
        finally:
            row['completed_at_utc'] = runner.now()
            report['rows'].append(row)
            try:
                report['accounting'] = runner.audit(output, budgets)
            except Exception as error:
                report.update(status='stopped', accounting_error=str(error))
                if (output / 'accounting.json').exists():
                    report['accounting'] = runner.read(output / 'accounting.json')
            report['formal_baseline_unchanged'] = runner.formal_state() == manifest['runtime']['formal_baseline']
            if not report['formal_baseline_unchanged']:
                report['status'] = 'stopped'
            runner.save(output / 'report.json', report)
    status(output)


def status(output):
    report = runner.read(output / 'report.json')
    print(json.dumps(dict(status=report['status'], executed=len(report['rows']), total=6,
        counts=dict(Counter(row['status'] for row in report['rows'])),
        physical_requests=report.get('accounting', {}).get('physical_requests', 0),
        not_run=6-len(report['rows'])), ensure_ascii=False))


def check():
    """No-send guards plus network-disabled entity registration; no model acceptance."""
    import copy
    import tempfile
    from unittest.mock import patch
    scenarios, budgets = plan()
    checks = []
    def rejects(fn, error=AssertionError):
        try:
            fn()
        except error:
            return
        raise AssertionError('guard accepted forbidden action')
    archive = runner.ROOT / 'docs/refactor/s5/temporal-grounding-live-20260927/specialist'
    def inspect_fixture(artifact, count, events=()):
        turn = artifact['request']['user_turn']
        return inspect_receipt(artifact['result'], artifact['profile'],
            dict(id=turn['turn_id'], text=turn['text']), artifact['request'], count, events)
    for relative, fingerprint in (
            ('undated/00-T01.json', '5357707e18ace58b91bb5a1af9240c59f3625bbb2baeabb86bf2d77a1358e9f3'),
            ('iso_date/01-T02.json', '4dde90bdfcf070e3f54f6a0589445168c1272b06b7ba7dc3095ee033874e5dfa')):
        path = archive / relative
        assert runner.digest(path) == fingerprint
        inspect_fixture(runner.read(path), 2)
    rejected = runner.read(archive / 'iso_date/01-T02.json')
    assert inspect_fixture(rejected, 2)['enrichment']['status'] == 'completed_rejected'
    checks.append('archived_T01_commit_and_T02_completed_rejected_receipts')
    # Synthetic mutations of the real receipt exercise classification only; they
    # are not provider reruns and never modify the archived evidence.
    failed = copy.deepcopy(rejected)
    saved = runner.receipt(failed['result'])
    saved.update(status='failed', error='invalid_candidates',
        enrichment=dict(status='failed', error='invalid_candidates'))
    saved.pop('outcomes')
    saved['model_calls'] = saved['model_calls'][:1]
    failed['result']['remember'] = saved
    record, = failed['profile']['sources'].values()
    record.update(enrichment='failed', error='invalid_candidates', result=None)
    operation = saved['model_calls'][0]['operation_id']
    numeric_error = 'invalid structured output: model temporal boundaries must be null; copy USER temporal_expression or expiry_expression'
    gate = dict(kind='validation', run_id=operation,
        data=dict(stage='extraction', status='failed', error=numeric_error))
    inspect_fixture(failed, 1, [gate])
    rejects(lambda: inspect_fixture(failed, 2, [gate]))
    rejects(lambda: inspect_fixture(failed, 1))
    rejects(lambda: inspect_fixture(failed, 1, [{**gate, 'run_id': 'another-operation'}]))
    for error in ('invalid structured output: missing field `value`',
            'invalid structured output: source review repeats candidate index',
            'invalid DeepSeek response: invalid JSON'):
        rejects(lambda: inspect_fixture(failed, 1, [{**gate, 'data': {**gate['data'], 'error': error}}]))
    reviewed_failure = copy.deepcopy(failed)
    reviewed_failure['result']['remember']['model_calls'] = copy.deepcopy(runner.receipt(rejected['result'])['model_calls'])
    review_events = [dict(kind='validation', run_id=operation, data=dict(stage='extraction', status='completed')),
        dict(kind='validation', run_id=operation, data=dict(stage='source_support', status='failed',
            error='invalid structured output: temporal metadata lacks locally grounded USER support'))]
    inspect_fixture(reviewed_failure, 2, review_events)
    checks.append('one_request_numeric_gate_and_two_request_grounding_gate_require_exact_bound_diagnostics')
    coverage_failure = copy.deepcopy(reviewed_failure)
    saved = coverage_failure['result']['remember']
    saved['error'] = saved['enrichment']['error'] = 'source_coverage_incomplete'
    next(iter(coverage_failure['profile']['sources'].values()))['error'] = 'source_coverage_incomplete'
    coverage_events = copy.deepcopy(review_events)
    coverage_events[-1]['data']['error'] = 'source_coverage_incomplete'
    inspect_fixture(coverage_failure, 2, coverage_events)
    rejects(lambda: inspect_fixture(coverage_failure, 1, coverage_events))
    for fault in ('one_call', 'stage', 'operation', 'completed', 'schema', 'duplicate_index', 'prefix',
            'public_error', 'receipt_error', 'provider', 'source', 'facts', 'pending'):
        broken, events = copy.deepcopy(coverage_failure), copy.deepcopy(coverage_events)
        saved = broken['result']['remember']
        if fault == 'one_call':
            saved['model_calls'].pop()
        elif fault == 'stage':
            events[-1]['data']['stage'] = 'extraction'
        elif fault == 'operation':
            events[-1]['run_id'] = 'another-operation'
        elif fault == 'completed':
            events[-1]['data']['status'] = 'completed'
        elif fault in ('schema', 'duplicate_index', 'prefix'):
            events[-1]['data']['error'] = {
                'schema': 'invalid structured output: source coverage requires consistent exact current USER spans',
                'duplicate_index': 'invalid structured output: source review repeats candidate index',
                'prefix': 'invalid structured output: source_coverage_incomplete',
            }[fault]
        elif fault == 'public_error':
            saved['error'] = saved['enrichment']['error'] = 'invalid_candidates'
            next(iter(broken['profile']['sources'].values()))['error'] = 'invalid_candidates'
        elif fault == 'receipt_error':
            saved['error'] = 'invalid_candidates'
        elif fault == 'provider':
            saved['model_calls'][-1]['outcome'] = 'failed'
        elif fault == 'source':
            next(iter(broken['profile']['sources'].values()))['request']['text'] = 'changed source'
        else:
            container = broken['profile']['repository']['memories'] if fault == 'facts' else broken['profile']['staging']['pending']
            container['unexpected'] = {}
        rejects(lambda: inspect_fixture(broken, len(saved['model_calls']), events))
    checks.append('two_request_coverage_refusal_requires_exact_bound_diagnostic_and_empty_persistence')
    for fault in ('provider', 'enrichment', 'index', 'facts', 'pending', 'recall', 'diagnostics'):
        broken = copy.deepcopy(failed)
        value = broken['result']['remember']
        if fault == 'provider':
            value['model_calls'][0]['outcome'] = 'failed'
        elif fault == 'enrichment':
            value['enrichment']['error'] = 'model_http_500'
            next(iter(broken['profile']['sources'].values()))['error'] = 'model_http_500'
        elif fault == 'index':
            value['index']['status'] = 'failed'
        elif fault in ('facts', 'pending'):
            container = broken['profile']['repository']['memories'] if fault == 'facts' else broken['profile']['staging']['pending']
            container['unexpected'] = {}
        elif fault == 'recall':
            broken['result']['recall']['status'] = 'failed'
        else:
            value['diagnostics'] = {'status': 'evidence_incomplete'}
        rejects(lambda: inspect_fixture(broken, 1, [gate]))
    rejects(lambda: inspect_fixture(rejected, 1))
    committed = runner.read(archive / 'undated/00-T01.json')
    runner.receipt(committed['result'])['model_calls'].pop()
    rejects(lambda: inspect_fixture(committed, 1))
    checks.append('provider_schema_diagnostics_persistence_and_missing_review_fail_closed')
    with tempfile.TemporaryDirectory(prefix='s5-temporal-check-') as tmp:
        pack, output = Path(tmp) / 'pack', Path(tmp) / 'run'
        pack.mkdir()
        for name in ('inputs.json', 'labels.json'):
            (pack / name).write_bytes((runner.PACK / name).read_bytes())
        current = datetime.datetime.now(datetime.timezone.utc)
        budgets['execution_window'] = dict(start_inclusive=(current-datetime.timedelta(hours=1)).isoformat(),
            end_exclusive=(current+datetime.timedelta(hours=1)).isoformat())
        runner.save(pack / 'budgets.json', budgets)
        runner.save(pack / 'dataset-freeze.json', {'offline_fixture': True})
        with patch.object(runner, 'PACK', pack), patch.object(runner, 'frozen', return_value={'formal_baseline': {}}), \
                patch.object(runner, 'LongClient') as client, patch.object(runner, 'clean_env') as environment:
            preflight(output)
            manifest = runner.read(output / 'manifest.json')
            assert len(manifest['operations']) == 6 and not list(output.glob('*/business.sqlite3'))
            assert all(runner.read(output / path)['model_budget']['max_calls'] == 2 for path in manifest['configs'])
            checks.append('six_isolated_two_call_configs')
            rejects(lambda: step(output), FileNotFoundError)
            auth = dict(manifest_sha256=runner.digest(output / 'manifest.json'), output=str(output),
                user_reply='offline fixture', max_physical_requests=12, automatic_retries=0,
                provider_base_url=budgets['provider_base_url'])
            runner.save(output / 'authorization.json', auth)
            verify(output, manifest)
            for key, value in (('max_physical_requests', 13), ('manifest_sha256', 'changed'),
                    ('output', str(output)+'-other'), ('user_reply', ''), ('automatic_retries', 1)):
                runner.save(output / 'authorization.json', {**auth, key: value})
                rejects(lambda: verify(output, manifest))
            runner.save(output / 'authorization.json', auth)
            checks.append('authorization_binding_and_no_retries')
            artifact = output / scenarios[0]['id'] / '00-T01.json'
            runner.save(artifact, {'fixture': True})
            row = dict(manifest['operations'][0], artifact=str(artifact.relative_to(output)), artifact_sha256=runner.digest(artifact), status='review_required')
            report = dict(status='review_required', rows=[row], fixed_denominator=6)
            runner.save(output / 'report.json', report)
            rejects(lambda: step(output), FileNotFoundError)
            checks.append('missing_review_blocks_next_source')
            review = dict(id='T01', phase='intake', artifact_sha256=runner.digest(artifact), reason='offline fixture',
                source_and_evidence='checked', assertion_coverage='checked', candidate_fields='checked', temporal_and_retention='checked')
            review_path = artifact.with_name(artifact.stem + '-review.json')
            for verdict in ('safety_error', 'runtime_error'):
                runner.save(output / 'report.json', copy.deepcopy(report))
                runner.save(review_path, dict(review, verdict=verdict))
                step(output)
                assert runner.read(output / 'report.json')['status'] == 'stopped'
            checks.append('manual_safety_and_runtime_stop_without_send')
            all_rows = []
            for index, op in enumerate(manifest['operations']):
                path = output / op['scenario'] / f"{index:02d}-{op['id']}.json"
                runner.save(path, {'fixture': True})
                all_rows.append(dict(op, artifact=str(path.relative_to(output)), artifact_sha256=runner.digest(path), status='passed'))
            all_rows[0]['status'] = 'quality_gap'
            last = all_rows[-1]
            runner.save(output / 'report.json', dict(status='review_required', rows=all_rows, fixed_denominator=6))
            path = output / last['artifact']
            runner.save(path.with_name(path.stem+'-review.json'), dict(review, id=last['id'], artifact_sha256=last['artifact_sha256'], verdict='passed'))
            step(output)
            assert runner.read(output / 'report.json')['status'] == 'quality_gaps'
            checks.append('quality_gap_prevents_overall_pass')
            config = output / next(iter(manifest['configs']))
            config.write_text('{}')
            rejects(lambda: verify(output, manifest))
            client.assert_not_called()
            environment.assert_not_called()
            checks.append('configuration_hash_and_zero_clients')
            for obstruction in ('started', 'database'):
                guarded = Path(tmp) / obstruction
                preflight(guarded)
                runner.save(guarded / 'authorization.json', dict(auth, output=str(guarded),
                    manifest_sha256=runner.digest(guarded / 'manifest.json')))
                case_dir = guarded / scenarios[0]['id']
                (case_dir / ('00-T01.started' if obstruction == 'started' else 'business.sqlite3')).touch()
                with patch.object(runner, 'audit', return_value=dict(reserved_calls=0, physical_requests=0)), \
                        patch.object(runner, 'formal_state', return_value={}):
                    step(guarded)
                assert runner.read(guarded / 'report.json')['status'] == 'stopped'
                client.assert_not_called()
                environment.assert_not_called()
            checks.append('started_marker_and_existing_database_prevent_replay')
            guarded = Path(tmp) / 'audit-failure'
            preflight(guarded)
            runner.save(guarded / 'authorization.json', dict(auth, output=str(guarded),
                manifest_sha256=runner.digest(guarded / 'manifest.json')))
            with patch.object(runner, 'audit', side_effect=AssertionError('trace/ledger counts differ')), \
                    patch.object(runner, 'formal_state', return_value={}):
                step(guarded)
            assert runner.read(guarded / 'report.json')['status'] == 'stopped'
            client.assert_not_called()
            environment.assert_not_called()
            checks.append('audit_failure_stops_without_send')
            # Exercise the actual step wiring with a fake client/auditor. No live
            # environment is loaded and this cannot create a provider request.
            import io
            with patch.object(runner, 'frozen', return_value={'formal_baseline': {}, 'binary': {'path': 'offline-fixture'}}):
                for name, raw, events, count, error in (
                        ('one-call-refusal', failed, [gate], 1, 'invalid_candidates'),
                        ('two-call-coverage-refusal', coverage_failure, coverage_events, 2, 'source_coverage_incomplete')):
                    guarded = Path(tmp) / name
                    preflight(guarded)
                    runner.save(guarded / 'authorization.json', dict(auth, output=str(guarded),
                        manifest_sha256=runner.digest(guarded / 'manifest.json')))
                    fixture = copy.deepcopy(raw)
                    request = runner.source_request(scenarios[0], scenarios[0]['sources'][0])
                    record, = fixture['profile']['sources'].values()
                    record.update(request={**request['user_turn'], 'task_id': None},
                        turn_id=request['user_turn']['turn_id'])
                    (guarded / scenarios[0]['id'] / 'trace' / 'fixture.jsonl').write_text(
                        ''.join(json.dumps(event) + '\n' for event in events))
                    client.return_value.call.return_value = {'model_calls': []}
                    client.return_value.errors = io.StringIO('')
                    environment.return_value = {}
                    with patch.object(runner, 'audit', side_effect=[dict(reserved_calls=0, physical_requests=0),
                            dict(reserved_calls=count, physical_requests=count), dict(reserved_calls=count, physical_requests=count)]), \
                            patch.object(runner, 'call', return_value=fixture['result']), \
                            patch.object(runner, 'profile', return_value=fixture['profile']), \
                            patch.object(runner, 'formal_state', return_value={}):
                        step(guarded)
                    report = runner.read(guarded / 'report.json')
                    assert report['status'] == 'review_required' and report['accounting']['physical_requests'] == count
                    assert report['rows'][0]['physical_requests'] == count
                    assert report['rows'][0]['observed_enrichment'] == dict(status='failed', error=error)
            checks.append('mocked_step_records_one_request_and_requires_manual_review')
            checks.append('mocked_step_records_two_request_coverage_refusal_and_requires_manual_review')
        binary = runner.ROOT / 'target/frozen/s5-temporal-grounding-regression-20260927/pmr-mcp'
        env = {k: v for k, v in runner.clean_env('deny').items() if not k.startswith('ARK_')}
        for scenario in scenarios:
            directory = Path(tmp) / ('entity-' + scenario['id'])
            directory.mkdir()
            config = runner.settings(directory, 'off', retrieval_mode='local', enrichment_mode='off')
            db = directory / 'business.sqlite3'
            with contextlib.closing(runner.LongClient(binary, db, env, runner.OWNER, config=config)) as client:
                assert client.call('memory_recall', {'query': '全局回答要求', 'bootstrap': True})['model_calls'] == []
                for entity in scenario['entities']:
                    client.call('memory_entity_upsert', entity)
            state = runner.profile(db)
            assert not state['sources'] and not state['repository']['memories']
            import sqlite3
            with sqlite3.connect(db.as_uri() + '?mode=ro', uri=True) as connection:
                assert connection.execute('SELECT count(*) FROM model_calls').fetchone()[0] == 0
        checks.append('six_entity_catalogs_registered_with_network_disabled_zero_model_ledger')
    print(json.dumps(dict(status='checks_passed', real_model_requests=0, model_requests=0,
        guard_mcp_clients=0, mocked_mcp_clients=2, entity_only_mcp_clients=6, checks=checks)))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=('preflight', 'step', 'status', 'check'))
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    if args.phase == 'check':
        check()
    else:
        assert args.output is not None, '--output required'
        globals()[args.phase](args.output.resolve())
