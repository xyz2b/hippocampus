#!/usr/bin/env python3
"""Grouped subject/time contract via four MCP intake paths; synthetic loopback only."""
import argparse
import contextlib
import copy
from datetime import date, timedelta
import hashlib
import http.server
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
from pathlib import Path

import source_review_cardinality_process as process
from temporal_grounding_process import day_start


grounding = process.grounding
TRACE = grounding.ROOT / 'docs/refactor/s5/source-repair-calendar-live-20260928/specialist/iso_date/trace/trace-29871-1790562808992776000-1.jsonl'
TRACE_SHA = '7005033e011204eb992bb4107521680b04969e35eb5b8f2a8fd1079247689b1b'
RAW_SHA = 'ac88428f92c00b3d58d708a3fab05ce940ac3a73fbf1d762993566b68cc6211c'
ENTITIES = {
    'person_linsong': ('林松', 'Person'),
    'device_wulan': ('雾岚打印机', 'Device'),
    'event_yinxing': ('银杏工作坊', 'Other'),
    'org_bailu': ('白鹭书社', 'Organization'),
    'event_chisha': ('赤砂工作坊', 'Other'),
    'item_bronze': ('青铜样品', 'Other'),
}
PAPER = '雾岚打印机的默认纸张规格是A4。'
ISO = '2026-10-02银杏工作坊的报名费为58元。'
TTL = '赤砂工作坊的报名费为68元，保留到明天。'
OBJECT = '青铜样品存放到明天。'
# text, intended entity, expected state after extraction/review, physical calls.
CASES = {
    'person': ('林松喜欢围棋。', 'person_linsong', 'committed', 2),
    'device': (PAPER, 'device_wulan', 'committed', 2),
    'event_date': (ISO, 'event_yinxing', 'committed', 2),
    'organization': ('白鹭书社的借阅上限是3册。', 'org_bailu', 'committed', 2),
    'remember': ('请记住：' + PAPER, 'device_wulan', 'committed', 2),
    'retention': (TTL, 'event_chisha', 'committed', 2),
    'object_storage': (OBJECT, 'item_bronze', 'committed', 2),
    'pending_person': ('他喜欢围棋。', 'person_linsong', 'completed_pending', 2),
    'pending_device': ('它的默认纸张规格是A4。', 'device_wulan', 'completed_pending', 2),
    'pending_event': ('这个活动的报名费为58元。', 'event_yinxing', 'completed_pending', 2),
    'pending_implicit': ('喜欢围棋。', 'person_linsong', 'completed_pending', 2),
    'empty_subject': (PAPER, 'device_wulan', 'failed', 1),
    'blank_subject': (PAPER, 'device_wulan', 'failed', 1),
    'mixed_subject': (PAPER, 'device_wulan', 'failed', 1),
    'orphan_legacy_expiry': (PAPER, 'device_wulan', 'failed', 1),
    'mixed_fact_time': (ISO, 'event_yinxing', 'failed', 1),
    'fact_as_expiry': ('明天银杏工作坊的报名费为58元。', 'event_yinxing', 'failed', 2),
    'object_as_expiry': (OBJECT, 'item_bronze', 'failed', 2),
    'recorded_T02_exact': (ISO, 'event_yinxing', 'failed', 1),
    'recorded_T02_subject_fixed': (ISO, 'event_yinxing', 'failed', 1),
}
OLD_FIELDS = {'subject_id', 'unresolved_subject', 'temporal_expression', 'temporal_date',
    'temporal_end_date', 'expiry_expression', 'expiry_date', 'valid_from', 'valid_to', 'expires_at',
    'explicit_memory_request'}


def recording():
    assert hashlib.sha256(TRACE.read_bytes()).hexdigest() == TRACE_SHA
    events = [json.loads(line) for line in TRACE.read_text().splitlines()]
    envelopes = [event['data']['response'] for event in events if event['kind'] == 'model_end']
    assert len(envelopes) == 1 and envelopes[0]['status'] == 'completed'
    text = process.output_block(envelopes[0])['text']
    assert hashlib.sha256(text.encode()).hexdigest() == RAW_SHA
    raw, = json.loads(text)['candidates']
    assert raw['subject_id'] is None and raw['unresolved_subject'] == ''
    assert raw['expiry_date'] == '2026-10-02' and raw['expiry_expression'] is None
    assert raw['temporal_expression'] is raw['temporal_date'] is raw['temporal_end_date'] is None
    return envelopes[0]


def candidate(name, payload):
    text, entity, _, _ = CASES[name]
    raw = grounding.candidate(entity, text, predicate='fact')
    for field in OLD_FIELDS:
        raw.pop(field, None)
    raw.update(subject={'entity_id': entity}, fact_time=None, memory_request={'kind': 'not_requested'})
    tomorrow = (date.fromisoformat(payload['current_date']) + timedelta(days=1)).isoformat()
    if name in ('event_date', 'mixed_fact_time'):
        raw['fact_time'] = dict(expression='2026-10-02', date='2026-10-02', end_date=None)
    if name == 'remember':
        raw['memory_request'] = {'kind': 'remember'}
    if name in ('retention', 'fact_as_expiry', 'object_as_expiry'):
        raw['memory_request'] = dict(kind='retain_until', expression='明天', date=tomorrow)
    if name.startswith('pending_'):
        reference = {'pending_person': '他', 'pending_device': '它', 'pending_event': '这个活动',
            'pending_implicit': '未说明是谁'}[name]
        raw['subject'] = {'unresolved_reference': reference}
    if name in ('empty_subject', 'blank_subject'):
        raw['subject'] = {'unresolved_reference': '' if name == 'empty_subject' else ' \t '}
    if name == 'mixed_subject':
        raw['subject_id'] = entity
    if name == 'orphan_legacy_expiry':
        raw['expiry_date'] = '2026-10-02'
    if name == 'mixed_fact_time':
        raw['temporal_expression'] = '2026-10-02'
    return raw


class Fixture(grounding.Fixture):
    bodies = []
    sent = []
    errors = []
    selected = None
    start = 0
    archived = None

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert self.path == '/v1/responses' and self.headers['Authorization'] == 'Bearer local-fixture-key'
            type(self).bodies.append(request)
            assert len(self.bodies) - self.start <= CASES[self.selected][3], 'unexpected repair/retry'
            payload = json.loads(request['input'])
            assert payload['current_user_turn'] == CASES[self.selected][0] and payload['conversation_history'] == []
            assert not grounding.active(self.db), 'facts active before review'
            name = request['text']['format']['name']
            if name == grounding.EXTRACTION:
                if self.selected.startswith('recorded_T02_'):
                    envelope = copy.deepcopy(self.archived)
                    if self.selected.endswith('subject_fixed'):
                        block = process.output_block(envelope)
                        raw = json.loads(block['text'])
                        raw['candidates'][0].update(subject_id='event_yinxing', unresolved_subject=None)
                        block['text'] = json.dumps(raw, ensure_ascii=False)
                else:
                    envelope = process.Fixture.completed(dict(omission_reason=None, candidates=[candidate(self.selected, payload)]))
            else:
                assert name == grounding.REVIEW
                assert len(payload['frozen_candidates']) == 1
                envelope = process.Fixture.completed(grounding.source_support_review(request))
            type(self).sent.append(envelope)
            self.respond(200, envelope)
        except Exception as error:
            type(self).errors.append(str(error))
            self.respond(500, {'error': {'message': 'fixture assertion failed'}})


def run_case(binary, directory, env, mode, name):
    text, entity, expected, call_count = CASES[name]
    capture = mode != 'legacy'
    work = directory / f'{mode}-{name}'
    work.mkdir()
    (work / 'trace').mkdir(mode=0o700)
    db = work / 'state.sqlite3'
    Fixture.selected, Fixture.db, Fixture.start = name, db, len(Fixture.bodies)
    start, error_start = Fixture.start, len(Fixture.errors)
    config = grounding.settings(work, 'config', retrieval_mode='local', fact_index_enabled=True,
        enrichment_mode={'explicit': 'off', 'legacy': 'inline'}.get(mode, mode),
        source_capture_enabled=capture, utc_offset_seconds=28800, memory_types=[], value_kinds=[],
        predicates=[dict(predicate='fact', cardinality='Multiple')],
        model_budget=dict(group_id=f'{mode}-{name}', max_calls=4, max_operation_calls=4,
            allow_coverage_repair=True, max_input_bytes=65536, extraction_output_tokens=16384,
            review_output_tokens=16384, query_output_tokens=8192))
    failures = []
    def check(value, message):
        if not value:
            failures.append(message)
    with contextlib.closing(grounding.Client(binary, db, dict(env, PMR_TRACE_DIR=str(work/'trace')), grounding.OWNER, config=config)) as client:
        client.call('memory_recall', dict(query='全局回答要求', bootstrap=True))
        for id, (canonical, kind) in ENTITIES.items():
            client.call('memory_entity_upsert', dict(id=id, canonical_name=canonical, aliases=[], entity_type=kind))
        before_entities = copy.deepcopy(grounding.snapshot(db)['entities'])
        turn = dict(text=text, history=[], session_id='subject-time-contract', turn_id=name, capture_source=capture)
        if mode == 'inline':
            prepared = grounding.tool(client, 'memory_prepare', dict(user_turn=turn))
            receipt = prepared['remember'].get('result', prepared['remember'])
            check(prepared['recall']['status'] == 'completed', 'local recall failed')
        else:
            receipt = grounding.tool(client, 'memory_remember', turn)
        if mode in ('deferred', 'explicit'):
            check(len(Fixture.bodies) == start and not grounding.active(db), 'capture sent a request')
            args = ['--drain-enrichment', '--max-jobs', '1'] if mode == 'deferred' else ['--enrich-source', receipt['source']['source_id']]
            batch = subprocess.run([str(binary), '--db', str(db), '--owner', grounding.OWNER,
                '--config', str(config), *args], env=dict(env, PMR_TRACE_DIR=str(work/'trace')), capture_output=True, text=True, timeout=30)
            if mode == 'deferred':
                check(batch.returncode == 0 and json.loads(batch.stdout)['attempted'] == 1, 'deferred batch failed')
            elif expected != 'failed':
                check(batch.returncode == 0, 'positive explicit enrichment failed')
            receipt = grounding.tool(client, 'memory_remember', turn)
            check(receipt['replayed'], 'missing batch replay')
        state = grounding.snapshot(db)
        initial_pending = len(state['staging']['pending'])
        initial_facts = len(state['repository']['memories'])
        check(state['entities'] == before_entities, 'extraction created or changed entities')
        check(initial_facts == int(expected == 'committed'), f'facts={initial_facts}, expected state {expected}')
        check(initial_pending == int(expected == 'completed_pending'), f'pending={initial_pending}, expected state {expected}')
        if capture:
            source, = state['sources'].values()
            check(source['request']['text'] == text, 'original source changed')
            check(receipt['source']['status'] == 'stored' and receipt['index']['status'] == 'ready', 'source/index missing')
            check(receipt['enrichment']['status'] == expected, 'unexpected enrichment: '+str(receipt['enrichment']))
            if expected == 'failed':
                check(receipt['enrichment']['error'] == 'invalid_candidates', 'unexpected rejection kind')
            before_calls = len(Fixture.bodies)
            replay = grounding.tool(client, 'memory_remember', turn)
            check(replay['replayed'] and len(Fixture.bodies) == before_calls, 'receipt replay sent request')
            with sqlite3.connect(db) as connection:
                check((text,) in connection.execute('SELECT text FROM source_segments').fetchall(), 'source index lost original')
        else:
            check(not state['sources'], 'legacy captured a source')
        if expected == 'completed_pending' and initial_pending == 1:
            pending, = state['staging']['pending'].values()
            staged_id = pending['id']
            canonical = ENTITIES[entity][0]
            clarification = dict(staged_id=staged_id, entity_reference=canonical, text='我说的是'+canonical+'。',
                session_id='subject-time-contract', turn_id=name+'-clarified')
            # Empty clarification must not change facts, pending evidence, entities or receipts.
            before = grounding.snapshot(db)
            client.call('memory_clarify', {**clarification, 'entity_reference':' \t '}, error=True)
            check(grounding.snapshot(db) == before, 'empty clarification had persistent side effects')
            calls_before = len(Fixture.bodies)
            result = client.call('memory_clarify', clarification)
            check('Applied' in result['outcome'].get('Ingested', {}), 'explicit subject clarification was not applied')
            clarified = grounding.snapshot(db)
            check(not clarified['staging']['pending'], 'resolved candidate remains pending')
            memory, = clarified['repository']['memories'].values()
            check(memory['subject_id'] == entity and memory['value'] == {'Text':text}, 'clarification changed assertion/subject')
            check({e['quote'] for e in memory['evidence']} == {text, clarification['text']}, 'clarification lost evidence chain')
            check(client.call('memory_clarify', clarification)['replayed'], 'clarification is not idempotent')
            check(len(Fixture.bodies) == calls_before, 'clarification sent model requests')
        requests, sent = Fixture.bodies[start:], Fixture.sent[start:]
        check(not Fixture.errors[error_start:], 'fixture errors: '+str(Fixture.errors[error_start:]))
        check(len(requests) == call_count, f'requests={len(requests)}, expected {call_count}')
        check([r['text']['format']['name'] for r in requests] == [grounding.EXTRACTION, grounding.REVIEW][:call_count], 'unexpected model sequence')
        with sqlite3.connect(db) as connection:
            ledger = connection.execute('SELECT operation_id,stage,outcome FROM model_calls ORDER BY rowid').fetchall()
        check([r[1] for r in ledger] == ['extraction','extraction_review'][:call_count]
            and len({r[0] for r in ledger}) == 1 and all(r[2]=='completed' for r in ledger), 'ledger changed operation/stages/outcome')
        if requests:
            payload = json.loads(requests[0]['input'])
            schema = requests[0]['text']['format']['schema']
            item = schema['properties']['candidates']['items']
            if '$ref' in item:
                item = schema['$defs'][item['$ref'].removeprefix('#/$defs/')]
            branches = item.get('anyOf', [item])
            check(all({'subject','fact_time','memory_request'} <= branch.get('properties', {}).keys()
                and not OLD_FIELDS.intersection(branch.get('properties', {})) for branch in branches),
                'model schema still exposes flat fields')
            if len(requests) == 2:
                review = json.loads(requests[1]['input'])
                check(review['now'] == payload['now'] and review['current_user_turn'] == text, 'review changed source anchor')
            tomorrow = (date.fromisoformat(payload['current_date']) + timedelta(days=1)).isoformat()
            for memory in grounding.active(db):
                expected_time = dict(observed_at=payload['now'], valid_from=None, valid_to=None, expires_at=None, reference=None)
                if name == 'event_date':
                    expected_time.update(valid_from=dict(at=day_start('2026-10-02'), source='Explicit'),
                        valid_to=dict(at=day_start('2026-10-02')+86400, source='Explicit'))
                if name == 'retention':
                    expected_time['expires_at'] = day_start(tomorrow)+86400
                check(memory['temporal'] == expected_time, 'fact date/storage deadline/retention roles mixed')
                check(memory['explicit_memory_request'] == (name in ('remember','retention')), 'memory intent differs from model proposal')
        if name == 'recorded_T02_exact' and sent:
            check(sent[0] == Fixture.archived and hashlib.sha256(process.output_block(sent[0])['text'].encode()).hexdigest() == RAW_SHA,
                'archived invalid response was changed')
        if name == 'recorded_T02_subject_fixed' and sent:
            original = json.loads(process.output_block(Fixture.archived)['text'])
            actual = json.loads(process.output_block(sent[0])['text'])
            original['candidates'][0].update(subject_id='event_yinxing', unresolved_subject=None)
            check(actual == original, 'derived archival negative changed more than subject fields')
    events = [json.loads(line) for path in (work/'trace').glob('*.jsonl') for line in path.read_text().splitlines()]
    return dict(mode=mode, case=name, passed=not failures, failures=failures, initial_facts=initial_facts,
        initial_pending=initial_pending, physical_loopback_requests=len(requests), ledger_stages=[r[1] for r in ledger],
        enrichment=receipt.get('enrichment'),
        validation_failures=[e['data'] for e in events if e['kind']=='validation' and e['data']['status']=='failed'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=grounding.ROOT/'target/debug/pmr-mcp')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cases', nargs='+', choices=list(CASES), default=list(CASES))
    parser.add_argument('--modes', nargs='+', choices=('inline','deferred','explicit','legacy'), default=['inline','deferred','explicit','legacy'])
    args = parser.parse_args()
    binary = args.binary.resolve()
    fingerprint = hashlib.sha256(binary.read_bytes()).hexdigest()
    Fixture.archived = recording()
    Fixture.bodies, Fixture.sent, Fixture.errors = [], [], []
    rows = []
    with contextlib.ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='pmr-subject-time-')))
        server = http.server.ThreadingHTTPServer(('127.0.0.1',0),Fixture)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
        env = {k:os.environ[k] for k in ('PATH','HOME','TMPDIR','LANG') if k in os.environ}
        env.update(PMR_MODEL_NETWORK='offline', DEEPSEEK_API_KEY='local-fixture-key',
            DEEPSEEK_BASE_URL=f'http://127.0.0.1:{server.server_port}/v1', NO_PROXY='127.0.0.1,localhost')
        for mode in args.modes:
            for name in args.cases:
                before = len(Fixture.bodies)
                try:
                    rows.append(run_case(binary,directory,env,mode,name))
                except (AssertionError,KeyError,TypeError,ValueError,sqlite3.Error) as error:
                    rows.append(dict(mode=mode,case=name,passed=False,failures=[str(error)],physical_loopback_requests=len(Fixture.bodies)-before))
    unchanged = hashlib.sha256(binary.read_bytes()).hexdigest() == fingerprint
    report = dict(passed=unchanged and all(r['passed'] for r in rows),cases=len(rows),results=rows,
        binary_sha256=fingerprint,binary_unchanged=unchanged,physical_loopback_requests=len(Fixture.bodies),real_provider_requests=0,
        archived_trace_sha256=TRACE_SHA,archived_output_text_sha256=RAW_SHA,
        evidence_note='T02 exact replays its original failed response unchanged. The separately named subject_fixed negative changes only its two legacy subject fields. All other extraction and review responses are synthetic protocol fixtures, not model semantic acceptance.')
    args.output.parent.mkdir(parents=True,exist_ok=True)
    if Fixture.bodies:
        request_path = args.output.with_name(args.output.stem+'-extraction-request.json')
        request_path.write_text(json.dumps(Fixture.bodies[0],ensure_ascii=False,indent=2)+'\n')
        report['extraction_request_sample'] = str(request_path)
    args.output.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps({k:report[k] for k in ('passed','cases','physical_loopback_requests','real_provider_requests','binary_sha256')}))
    for row in rows:
        if not row['passed']:
            print(row['mode'],row['case'],row['failures'])
    assert report['passed'], 'subject/time contract process regression failed'


if __name__ == '__main__':
    main()
