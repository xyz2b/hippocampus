#!/usr/bin/env python3
"""Rust loopback HTTP + isolated SQLite WAL/log changes; no model calls."""
import hashlib
import http.client
import json
import sqlite3
import subprocess
import sys
import tempfile
from contextlib import ExitStack
from pathlib import Path

from inspector_support import UI, event, schema_check, start_inspector


def main():
    with tempfile.TemporaryDirectory(prefix="pmr-inspector-live-") as directory, ExitStack() as stack:
        root = Path(directory)
        db, log = root / "memory.sqlite3", root / "service.log"
        log.write_text('initial log\n')
        writer = sqlite3.connect(db)
        writer.execute("PRAGMA journal_mode=WAL")
        writer.execute("CREATE TABLE memory_profiles(owner_id TEXT PRIMARY KEY,state_json TEXT NOT NULL)")
        state = dict(repository=dict(memories={}), staging=dict(pending={}), receipts={})
        def commit():
            writer.execute("INSERT OR REPLACE INTO memory_profiles VALUES (?,?)", ('test-user', json.dumps(state)))
            writer.commit()
        commit()
        server = stack.enter_context(start_inspector(db, 'test-user', [log]))
        def request(path='/api/live', method='GET', authenticated=True, headers=None, target=None):
            target = target or server
            cookie = f'{target.cookie_name}={target.token}'
            conn = http.client.HTTPConnection('127.0.0.1', target.server_port, timeout=3)
            conn.request(method, path, headers={**({'Cookie':cookie} if authenticated else {}), **(headers or {})})
            response = conn.getresponse()
            data, status, response_headers = response.read(), response.status, dict(response.getheaders())
            conn.close()
            return status, data, response_headers
        try:
            unauthorized = request(authenticated=False)[0]
            wrong_host = request(headers={'Host':'evil.example'})[0]
            wrong_origin = request(headers={'Origin':'https://evil.example'})[0]
            cross_site = request(headers={'Sec-Fetch-Site':'cross-site'})[0]
            no_post = request(method='POST')[0]
            no_path = request('/api/../../memory.sqlite3')[0]
            login = request('/?token='+server.token, authenticated=False)
            page = request('/')[1].decode()
            first = json.loads(request()[1])
            assert first['meta']['channels']['source_capture'] == 'unknown'
            state['sources'] = {}
            commit()
            empty_source = json.loads(request()[1])
            assert empty_source['meta']['channels']['source_capture'] == 'unknown'
            memory = dict(id=1, owner_id='test-user', subject_id='self', evidence=[dict(id='e1',quote='fixture alpha')],
                          memory_type='Fact',predicate='fact',value={'Text':'alpha'},status='Active',scope={},
                          retention_tier='LongTerm',temporal=dict(observed_at=1789917000,
                              valid_from={'at':1789917000,'source':'Explicit'},
                              valid_to={'at':1789917060,'source':'Inferred'},expires_at=1789917120),superseded_by=None,supersedes=None)
            state['repository']['memories']['1'] = memory
            state['receipts']['test-receipt'] = dict(request=dict(text='fixture alpha',session_id='test'),
                result=dict(outcomes=[{'Ingested':{'Applied':{'memory_id':1,'action':'New'}}}]))
            commit()
            with log.open('a') as f:f.write('appended\nAuthorization: Bearer do-not-leak-this-secret\n{"api_key":"also-secret"}\n')
            before = {p:hashlib.sha256(p.read_bytes()).hexdigest() for p in (db,Path(str(db)+'-wal'))}
            status, body, _ = request()
            assert status == 200, f'DatedValue snapshot returned HTTP {status}'
            second = json.loads(body)
            assert second['memories'][0]['valid_from'] == '2026-09-20T15:10:00+00:00'
            assert second['memories'][0]['valid_to'] == '2026-09-20T15:11:00+00:00'
            assert second['memories'][0]['raw']['temporal']['valid_from']['source'] == 'Explicit'
            after = {p:hashlib.sha256(p.read_bytes()).hexdigest() for p in before}
            memory['temporal']['valid_to'] = {'at':2**63 - 1,'source':'Inferred'}
            commit()
            status, body, _ = request()
            assert status == 200, f'Open-ended DatedValue returned HTTP {status}'
            unbounded = json.loads(body)['memories'][0]
            assert unbounded['valid_to'] is None and unbounded['valid_to_unbounded'] is True
            assert unbounded['raw']['temporal']['valid_to']['at'] == 2**63 - 1
            log.rename(root/'old.log');log.write_text('rotated log\n')
            third = json.loads(request()[1])
            log.unlink()
            missing_log = json.loads(request()[1])
            with start_inspector(root/'missing.sqlite3', 'test-user', [log]) as missing_server:
                missing_db = json.loads(request(target=missing_server)[1])
            for bundle in (first, empty_source, second, third, missing_log, missing_db):
                validated = schema_check(bundle)
                assert validated.returncode == 0, validated.stderr
            boundary = (unauthorized == 401 and wrong_host == wrong_origin == cross_site == 403
                        and no_post == 405 and no_path == 404 and login[0] == 303
                        and 'HttpOnly' in login[2]['Set-Cookie'] and 'SameSite=Strict' in login[2]['Set-Cookie']
                        and before == after and not (root/'missing.sqlite3').exists()
                        and missing_db['memory_snapshot']['completeness'] == 'not_captured'
                        and all(r['mcp_response']['recorded'] is False and r['host']['input_observability']=='unavailable' for r in second['runs'])
                        and 'do-not-leak' not in json.dumps(second) and 'also-secret' not in json.dumps(second))
            live = (first['memories'] == [] and second['memories'][0]['raw']['value'] == {'Text':'alpha'}
                    and second['runs'][0]['save_status'] == 'committed'
                    and second['runs'][0]['write_changes'][0]['after'] is None
                    and 'appended' in second['live']['logs'][0]['lines']
                    and second['live']['revision'] != first['live']['revision']
                    and third['live']['logs'][0]['lines'] == ['rotated log']
                    and third['live']['logs'][0]['identity'] != second['live']['logs'][0]['identity']
                    and missing_log['live']['status'] == 'partial'
                    and "connect-src 'self'" in page and "connect-src 'none'" in UI.read_text())
            # Execute the shipped polling functions with controlled responses/timers.
            # Check repeated renders, stale-on-error retention, refresh timing and pause.
            js = page[page.index('let liveMode='):page.index("$('live-connect').addEventListener")]
            harness = '''const assert=require('node:assert/strict');
let bundle=null,warnings=[],selectedMemory=null,renders=0,responses=INPUT,fetches=0;
const label={textContent:''};const $=()=>label,sanitize=x=>x,validate=()=>[],ref=m=>m.id+'@'+m.version;
const document={querySelectorAll:()=>[]};const window={scrollY:42,scrollTo:()=>{}};
const navigateDetailHash=()=>false;const render=()=>{renders++};const fetch=async()=>{fetches++;const x=responses.shift();if(x instanceof Error)throw x;return{ok:true,json:async()=>x};};
const timers=[];const setTimeout=(fn,ms)=>{timers.push({fn,ms});return timers.length;};const clearTimeout=()=>{};
'''.replace('INPUT', json.dumps([first, second, second]))
            checks = '''
(async()=>{liveMode=true;await pollLive();assert.equal(bundle.memories.length,0);
await pollLive();assert.equal(bundle.memories.length,1);assert.equal(renders,2);
await pollLive();assert.equal(renders,2);assert.ok(timers.some(x=>x.ms===2000));
responses.push(new Error('offline'));await pollLive();assert.equal(bundle.memories.length,1);assert.match(label.textContent,/连接失败/);
stopLive('paused');await pollLive();assert.equal(fetches,4);assert.equal(label.textContent,'paused');
console.log('polling passed');})().catch(e=>{console.error(e);process.exitCode=1});
'''
            node = subprocess.run(['node','-e',harness+js+checks],capture_output=True,text=True)
            if node.returncode:print(node.stderr)
            live &= node.returncode == 0
            a = event('live-update',live,json.dumps(dict(memory_counts=[len(first['memories']),len(second['memories'])],
                appended='appended' in second['live']['logs'][0]['lines'],rotated=third['live']['logs'][0]['lines'],
                missing_log=missing_log['live']['status'],polling_exit=node.returncode)))
            b = event('readonly-boundary',boundary,json.dumps(dict(unauthorized=unauthorized,wrong_host=wrong_host,
                wrong_origin=wrong_origin,cross_site=cross_site,post=no_post,path=no_path,login=login[0],
                db_wal_unchanged=before==after,missing_snapshot=missing_db['memory_snapshot']['completeness'],
                mcp_recorded=[r['mcp_response']['recorded'] for r in second['runs']])))
            return 0 if a and b else 1
        finally:
            writer.close()


if __name__=='__main__':
    sys.exit(main())
