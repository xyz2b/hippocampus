#!/usr/bin/env python3
"""Six isolated calendar/coverage-repair sources; no send without frozen authorization."""
import argparse
import os
from pathlib import Path

import s5_temporal_grounding_live as specialist

runner = specialist.runner
PACK = runner.ROOT / 'docs/refactor/s5/source-repair-calendar-specialist-20260928'
OWNER = 's5-source-repair-calendar-specialist-20260928'
runner.PACK, runner.OWNER = PACK, OWNER
runner.DEPENDENCIES += ('tests/s5_source_repair_calendar_live.py',)
specialist.INTAKE_MAX_CALLS = 4


def check():
    """No credentials/provider access: recorded receipt mutations, mocked steps, local ledgers."""
    import contextlib
    import copy
    import io
    import json
    import sqlite3
    import tempfile
    from unittest.mock import patch

    checks = []
    def rejects(fn, error=AssertionError):
        try:
            fn()
        except error:
            return
        raise AssertionError('guard accepted forbidden action')

    # Preserve the full existing two-call runner checks, including deny-network entity setup.
    # This environment contains no provider variables and never reads a credential value.
    def deny_env(mode):
        assert mode == 'deny'
        return {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'PMR_MODEL_NETWORK': 'deny'}
    with patch.object(specialist, 'INTAKE_MAX_CALLS', 2), \
            patch.object(runner, 'PACK', runner.ROOT / 'docs/refactor/s5/source-completeness-specialist-20260928'), \
            patch.object(runner, 'OWNER', 's5-source-completeness-specialist-20260928'), \
            patch.object(runner, 'clean_env', side_effect=deny_env), contextlib.redirect_stdout(io.StringIO()):
        specialist.check()
    checks.append('legacy_two_call_contract_and_all_existing_no_send_guards')

    scenarios, budgets = specialist.plan()
    archive = runner.ROOT / 'docs/refactor/s5/temporal-grounding-live-20260927/specialist'
    committed = runner.read(archive / 'undated/00-T01.json')
    rejected = runner.read(archive / 'iso_date/01-T02.json')
    assert runner.digest(archive / 'undated/00-T01.json') == '5357707e18ace58b91bb5a1af9240c59f3625bbb2baeabb86bf2d77a1358e9f3'
    assert runner.digest(archive / 'iso_date/01-T02.json') == '4dde90bdfcf070e3f54f6a0589445168c1272b06b7ba7dc3095ee033874e5dfa'
    stages = ['extraction', 'extraction_review', 'extraction_repair', 'extraction_repair_review']
    date_error = 'invalid structured output: model date does not match the USER expression and local anchor'

    def fixture(count, status, error=None, reason=None):
        raw = copy.deepcopy(committed if status == 'committed' else rejected)
        saved = runner.receipt(raw['result'])
        raw['result']['remember'] = saved
        original_calls = copy.deepcopy(saved['model_calls'])
        saved['model_calls'] = [{**original_calls[i % 2], 'call_id': f'offline-call-{i}', 'stage': stages[i]}
            for i in range(count)]
        saved['enrichment'] = dict(status=status, error=error)
        saved['status'] = 'failed' if status == 'failed' else 'completed'
        if status == 'failed':
            saved['error'] = error
            saved.pop('outcomes', None)
        else:
            saved.pop('error', None)
        record, = raw['profile']['sources'].values()
        record.update(enrichment=status, error=error)
        operation = saved['model_calls'][0]['operation_id']
        events = []
        for i in range(count):
            stage = stages[i] if i % 2 == 0 else 'source_support'
            data = dict(stage=stage, status='completed')
            if i == 1 and count > 2:
                data.update(status='failed', error='source_coverage_incomplete')
            if i == count - 1 and status == 'failed' and error != 'model_budget_rejected':
                data.update(status='failed', error=reason)
            events.append(dict(kind='validation', run_id=operation, data=data))
        if count > 2:
            # These are protocol-only diagnostics, not a semantic approval of the source.
            first_review = dict(coverage=dict(status='incomplete', missing_user_spans=[record['request']['text']]),
                reviews=[dict(checks=dict.fromkeys('subject relation value polarity scope temporal intent source metadata'.split(), 'supported'), issues=[])])
            events.append(dict(kind='model_end', run_id=operation,
                data=dict(call_id=saved['model_calls'][1]['call_id'], parsed_result=first_review)))
        return raw, events

    def inspect(raw, events, count=None):
        turn = raw['request']['user_turn']
        count = len(runner.receipt(raw['result'])['model_calls']) if count is None else count
        return specialist.inspect_receipt(raw['result'], raw['profile'],
            dict(id=turn['turn_id'], text=turn['text']), raw['request'], count, events)

    success = fixture(4, 'committed')
    incomplete = fixture(4, 'failed', 'source_coverage_incomplete', 'source_coverage_incomplete')
    negative = fixture(4, 'completed_rejected')
    wrong_date = fixture(1, 'failed', 'invalid_candidates', date_error)
    repair_date = fixture(3, 'failed', 'invalid_candidates', date_error)
    budget_error = fixture(3, 'failed', 'model_budget_rejected')
    for raw, events in (success, incomplete, negative, wrong_date, repair_date):
        inspect(raw, events)
    rejects(lambda: inspect(*budget_error))
    checks.append('four_stage_commit_incomplete_and_atomic_rejection_plus_one_or_three_call_date_refusal')
    checks.append('three_call_budget_exhaustion_is_runtime_error')
    for message in ('invalid structured output: missing field `value`',
            'invalid structured output: source review repeats candidate index',
            'invalid structured output: model calendar date must be a valid YYYY-MM-DD',
            'invalid DeepSeek response: invalid JSON'):
        raw, events = fixture(1, 'failed', 'invalid_candidates', message)
        rejects(lambda: inspect(raw, events))
    checks.append('exact_calendar_gate_does_not_whitelist_schema_or_arbitrary_invalid_candidates')
    for fault in ('uncertain', 'negative', 'fake_span', 'missing_first_review', 'stage', 'operation', 'provider', 'count', 'facts', 'pending'):
        raw, events = copy.deepcopy(negative)
        calls = runner.receipt(raw['result'])['model_calls']
        if fault == 'uncertain':
            events[-1]['data']['parsed_result']['coverage']['status'] = 'uncertain'
        elif fault == 'negative':
            events[-1]['data']['parsed_result']['reviews'][0]['checks']['value'] = 'contradicted'
        elif fault == 'fake_span':
            events[-1]['data']['parsed_result']['coverage']['missing_user_spans'] = ['invented source']
        elif fault == 'missing_first_review':
            events.pop()
        elif fault == 'stage':
            calls[2]['stage'] = 'extraction'
        elif fault == 'operation':
            calls[2]['operation_id'] = 'new-operation'
        elif fault == 'provider':
            calls[2]['outcome'] = 'failed'
        elif fault == 'count':
            calls.append({**calls[-1], 'call_id': 'fifth'})
        else:
            container = raw['profile']['repository']['memories'] if fault == 'facts' else raw['profile']['staging']['pending']
            container['unexpected'] = {}
        rejects(lambda: inspect(raw, events))
    checks.append('repair_trigger_binding_stage_limits_and_zero_partial_persistence')

    with tempfile.TemporaryDirectory(prefix='s5-repair-check-') as tmp:
        directory, pack = Path(tmp), Path(tmp) / 'pack'
        pack.mkdir()
        for name in ('inputs.json', 'labels.json', 'budgets.json'):
            (pack / name).write_bytes((PACK / name).read_bytes())
        runner.save(pack / 'dataset-freeze.json', {'offline_fixture': True})
        with patch.object(runner, 'PACK', pack), \
                patch.object(runner, 'frozen', return_value={'formal_baseline': {}, 'binary': {'path': 'never-start'}}), \
                patch.object(runner, 'LongClient') as client, patch.object(runner, 'clean_env') as environment, \
                patch.object(runner, 'formal_state', return_value={}), contextlib.redirect_stdout(io.StringIO()):
            output = directory / 'preflight'
            specialist.preflight(output)
            manifest = runner.read(output / 'manifest.json')
            assert len(manifest['configs']) == 6 and not list(output.glob('*/business.sqlite3'))
            assert all(runner.read(output / p)['model_budget']['max_calls'] == 4
                and runner.read(output / p)['model_budget']['allow_coverage_repair'] is True for p in manifest['configs'])
            rejects(lambda: specialist.step(output), FileNotFoundError)
            auth = dict(manifest_sha256=runner.digest(output / 'manifest.json'), output=str(output),
                user_reply='offline synthetic authorization fixture', max_physical_requests=24, automatic_retries=0,
                provider_base_url=budgets['provider_base_url'])
            runner.save(output / 'authorization.json', auth)
            specialist.verify(output, manifest)
            runner.save(output / 'authorization.json', {**auth, 'max_physical_requests': 12})
            rejects(lambda: specialist.verify(output, manifest))
            client.assert_not_called()
            environment.assert_not_called()
            checks.append('six_four_call_configs_require_new_bound_24_call_authorization')
            for name, (raw, events) in zip(('commit', 'coverage', 'negative', 'date', 'repair-date', 'budget'),
                    (success, incomplete, negative, wrong_date, repair_date, budget_error)):
                output = directory / name
                specialist.preflight(output)
                runner.save(output / 'authorization.json', {**auth, 'output': str(output),
                    'manifest_sha256': runner.digest(output / 'manifest.json')})
                raw, events = copy.deepcopy((raw, events))
                request = runner.source_request(scenarios[0], scenarios[0]['sources'][0])
                source, = raw['profile']['sources'].values()
                source.update(request={**request['user_turn'], 'task_id': None}, turn_id='T01')
                for event in events:
                    if event['kind'] == 'model_end':
                        event['data']['parsed_result']['coverage']['missing_user_spans'] = [source['request']['text']]
                (output / scenarios[0]['id'] / 'trace' / 'fixture.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
                count = len(runner.receipt(raw['result'])['model_calls'])
                client.return_value.call.return_value = {'model_calls': []}
                client.return_value.errors = io.StringIO('')
                environment.return_value = {}
                with patch.object(runner, 'call', return_value=raw['result']), patch.object(runner, 'profile', return_value=raw['profile']), \
                        patch.object(runner, 'audit', side_effect=[dict(reserved_calls=0, physical_requests=0),
                            dict(reserved_calls=count, physical_requests=count), dict(reserved_calls=count, physical_requests=count)]):
                    specialist.step(output)
                report = runner.read(output / 'report.json')
                assert report['accounting']['physical_requests'] == count
                assert report['status'] == ('stopped' if name == 'budget' else 'review_required'), report
            checks.append('mocked_real_step_preserves_actual_1_3_4_counts_and_stops_budget_failure')

        # Exercise the real auditor against disposable ledger/trace/tool receipt fixtures.
        output = directory / 'audit'
        scenario = scenarios[0]['id']
        work = output / scenario
        (work / 'trace').mkdir(parents=True)
        config = budgets['groups'][scenario]['intake']
        calls, events = [], []
        for index, stage in enumerate(stages):
            call = dict(call_id=f'audit-{index}', group_id=config['group_id'], owner_id=OWNER,
                operation_id='same-operation', stage=stage, outcome='completed', input_bytes=100,
                output_token_limit=16384, input_tokens=10, output_tokens=10, usage_status='known')
            calls.append(call)
            events.append(dict(kind='model_start', data=dict(call_id=call['call_id'], attempt=1,
                timeout_ms=180000, provider='ark.cn-beijing.volces.com', budget_group=config['group_id'],
                request=dict(max_output_tokens=16384, model=budgets['model'], reasoning={'effort': budgets['reasoning']}))))
            events.append(dict(kind='model_end', data=dict(call_id=call['call_id'], http_status=200,
                status='completed', response={'status': 'completed'}, parsing_status='valid_json', error=None)))
        db = work / 'business.sqlite3'
        with sqlite3.connect(db) as connection:
            connection.execute('CREATE TABLE model_calls (' + ','.join(calls[0]) + ')')
            connection.executemany('INSERT INTO model_calls VALUES (' + ','.join('?' for _ in calls[0]) + ')', [tuple(c.values()) for c in calls])
        (work / 'trace' / 'fixture.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
        runner.save(work / '00-T01-poll-0.json', dict(structuredContent=dict(model_calls=calls)))
        assert runner.audit(output, budgets)['physical_requests'] == 4
        for mutation in ('old_flag', 'three_budget', 'duplicate_stage'):
            changed = copy.deepcopy(budgets)
            if mutation == 'old_flag':
                changed['groups'][scenario]['intake'].pop('allow_coverage_repair')
            elif mutation == 'three_budget':
                changed['groups'][scenario]['intake']['max_calls'] = 3
            else:
                with sqlite3.connect(db) as connection:
                    connection.execute("UPDATE model_calls SET stage='extraction' WHERE call_id='audit-2'")
            rejects(lambda: runner.audit(output, changed))
        checks.append('real_ledger_trace_receipt_audit_requires_explicit_repair_flag_and_budget')
    print(json.dumps(dict(status='checks_passed', model_requests=0, real_provider_requests=0,
        guard_mcp_clients=0, mocked_step_clients=6, legacy_entity_only_deny_clients=6, checks=checks)))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=('check', 'preflight', 'step', 'status'))
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    if args.phase == 'check':
        check()
    else:
        assert args.output is not None, '--output required'
        getattr(specialist, args.phase)(args.output.resolve())
