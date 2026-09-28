#!/usr/bin/env python3
"""S0: replay recorded evidence read-only; exercise MCP gates on disposable databases."""
import hashlib
import json
import os
import sqlite3
import sys
import tempfile
from pathlib import Path

from mcp_smoke import Client
from inspector_support import schema_check


def main():
    # The evidence replay itself may never open a network socket.
    def no_network(name, args):
        if name.startswith('socket.'):
            raise RuntimeError('network forbidden during S0 replay')
    sys.addaudithook(no_network)
    fixture = Path('inspector/web/recorded-4096-failure.json')
    digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
    for _ in range(2):
        bundle = json.loads(fixture.read_bytes())
        checked = schema_check(bundle)
        assert checked.returncode == 0 and checked.stdout.strip() == '[]', checked.stderr
        assert bundle['meta']['data_kind'] == 'recorded'
        a, b = bundle['runs']
        assert a['save_status'] == 'not_committed' and a['recall_status'] == 'planning_incomplete'
        assert b['recall_status'] == 'completed_empty' and b['retrieval']['selected_refs'] == []
        assert bundle['memories'] == [] and bundle['memory_snapshot']['state']['receipts'] == {}
        review = bundle['llm_calls'][1]
        assert review['status'] == 'incomplete' and review['finish_reason'] == 'max_output_tokens'
        assert review['usage']['output_tokens'] == review['usage']['reasoning_tokens'] == 4096
        assert all(call['request'] is None and call['cost']['amount'] is None for call in bundle['llm_calls'])
    assert hashlib.sha256(fixture.read_bytes()).hexdigest() == digest

    env = {k: v for k, v in os.environ.items()
           if not any(word in k.upper() for word in ('API_KEY', 'TOKEN', 'PROXY'))}
    env.pop('PMR_MODEL_NETWORK', None)  # Prove the default, not just an explicit setting.
    env.pop('PMR_OPENAI_TRACE_CONTENT', None)
    env.update(DEEPSEEK_BASE_URL='https://provider-must-not-be-contacted.invalid/v1', PMR_OPENAI_TRACE='1')
    binary = Path('target/debug/pmr-mcp').resolve()
    with tempfile.TemporaryDirectory(prefix='pmr-s0-') as directory:
        for mode in ('missing-key', 'default-offline', 'deny'):
            child_env = dict(env)
            if mode != 'missing-key':
                child_env['DEEPSEEK_API_KEY'] = 'synthetic-non-secret-key'
            if mode == 'deny':
                child_env.update(PMR_MODEL_NETWORK='deny', DEEPSEEK_BASE_URL='http://127.0.0.1:1')
            db = Path(directory) / (mode + '.sqlite3')
            client = Client(binary, db, child_env)
            try:
                def snapshot():
                    with sqlite3.connect(db) as conn:
                        return conn.execute('SELECT owner_id,state_json FROM memory_profiles ORDER BY owner_id').fetchall()
                before = snapshot()
                for turn in ('one', 'two'):
                    result = client.call('memory_prepare', {'user_turn': {
                        'text': '这是隔离测试陈述。', 'session_id': 's0', 'turn_id': turn}})
                    for lane in ('remember', 'recall'):
                        assert result[lane]['status'] == 'failed', result
                        reason = 'DEEPSEEK_API_KEY is not set' if mode == 'missing-key' else 'model network disabled'
                        assert reason in result[lane]['error'], result
                    assert snapshot() == before, 'failed model processing must not change the baseline'
                bootstrap = client.call('memory_recall', {'query': '全局回答要求', 'bootstrap': True})
                assert bootstrap['result']['mandatory'] == []
                client.errors.flush()
                client.errors.seek(0)
                assert '[openai-attempt]' not in client.errors.read(), 'blocked calls must send zero requests'
            finally:
                client.close()
    print(json.dumps({'status': 'passed', 'recorded_replays': 2, 'fixture_sha256': digest,
                      'provider_send_attempts': 0, 'cases': ['missing-key', 'default-offline', 'deny', 'offline-bootstrap'],
                      'scope': 'recorded evidence replay and transport failure; not semantic success'}))


if __name__ == '__main__':
    main()
