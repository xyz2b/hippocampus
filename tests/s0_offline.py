#!/usr/bin/env python3
"""S0: read-only validation of the historical Inspector evidence package."""
import hashlib
import json
import sys
from pathlib import Path

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

    print(json.dumps({'status': 'passed', 'recorded_replays': 2, 'fixture_sha256': digest,
                      'provider_send_attempts': 0,
                      'scope': 'historical Inspector package only; not current pipeline quality'}))


if __name__ == '__main__':
    main()
