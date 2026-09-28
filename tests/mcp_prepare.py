#!/usr/bin/env python3
"""Actual MCP/SQLite orchestration regression; fixture does not prove model semantic quality."""
import argparse, contextlib, http.server, json, os, sqlite3, tempfile, threading, time
from pathlib import Path
from mcp_smoke import Client, Responses
from inspector_support import export_runtime, schema_check

class PrepareResponses(Responses):
    fail_query = False
    slow_extraction = False
    def output(self, body):
        schema = body['text']['format']['name']
        if schema == 'personal_memory_source_support_v1':
            return super().output(body)
        is_query = 'runtime_context' in schema
        if not is_query and type(self).slow_extraction:
            type(self).slow_extraction=False
            time.sleep(46)
        # These are deliberate transport fixtures, never semantic test verdicts.
        source = json.loads(body['input']) if isinstance(body['input'],str) else body['input']
        current = source.get('current_user_turn','')
        if is_query and self.fail_query or not is_query and current == 'EXTRACT_FAIL':
            return {'invalid_fixture_output': True}
        if not is_query and current == '我要求使用什么语言回答？':
            return {'candidates': [], 'omission_reason': 'no_personal_assertion'}
        return super().output(body)

def state(db):
    with sqlite3.connect(db) as conn:
        return json.loads(conn.execute("select state_json from memory_profiles where owner_id='smoke-user'").fetchone()[0])

def main():
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,default=Path('target/debug/pmr-mcp'));args=p.parse_args()
    with contextlib.ExitStack() as stack:
        root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='pmr-prepare-')))
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),PrepareResponses)
        threading.Thread(target=server.serve_forever,daemon=True).start();stack.callback(server.server_close);stack.callback(server.shutdown)
        trace=root/'trace';trace.mkdir(mode=0o700)
        env=dict(os.environ,PMR_MODEL_NETWORK='offline',PMR_TRACE_DIR=str(trace),DEEPSEEK_API_KEY='local-fixture-key',DEEPSEEK_BASE_URL=f'http://127.0.0.1:{server.server_port}/v1')
        db=root/'memory.sqlite3';c=Client(args.binary.resolve(),db,env,budget_calls=100);stack.callback(c.close)
        turn={'text':'以后无论聊什么，回答都必须使用中文。','session_id':'prepare','turn_id':'1'}
        result=c.call('memory_prepare',{'user_turn':turn})
        assert result['remember']['status']=='completed' and result['recall']['status']=='completed', result
        assert len(state(db)['repository']['memories'])==1
        assert result['recall']['result']['result']['mandatory'][0]['value']=={'Text':'中文'},result
        counts=state(db)['repository']['memories']['1']['reinforcement_count']
        replay=c.call('memory_prepare',{'user_turn':turn})
        assert replay['remember']['result']['replayed'] is True,replay
        assert state(db)['repository']['memories']['1']['reinforcement_count']==counts
        assert len(state(db)['receipts'])==1
        before=state(db)
        question=c.call('memory_prepare',{'user_turn':{**turn,'text':'我要求使用什么语言回答？','turn_id':'2'}})
        assert question['remember']['result']['outcomes']==[],question
        after=state(db)
        assert before['receipts']==after['receipts'] and before['staging']==after['staging']
        assert question['recall']['result']['result']['mandatory'][0]['id']==1
        # Never extract an assistant-produced search query as personal evidence.
        c.call('memory_prepare',{'query':'助手改写','user_turn':turn},error=True)
        c.call('memory_prepare',{'user_turn':turn,'limit':0},error=True)
        PrepareResponses.fail_query=True
        partial=c.call('memory_prepare',{'user_turn':{**turn,'turn_id':'3'}})
        assert partial['remember']['status']=='completed' and partial['recall']['status']=='failed',partial
        assert len(state(db)['receipts'])==2 and state(db)['repository']['memories']['1']['reinforcement_count']==counts+1
        PrepareResponses.fail_query=False
        partial=c.call('memory_prepare',{'user_turn':{**turn,'text':'EXTRACT_FAIL','turn_id':'4'}})
        assert partial['remember']['status']=='failed' and partial['recall']['status']=='completed',partial
        assert len(state(db)['receipts'])==2
        # Planner is now capped at 15s; use extraction to exercise the 45s MCP continuation.
        PrepareResponses.slow_extraction=True
        args_slow={'user_turn':{**turn,'turn_id':'slow'}}
        started=time.monotonic()
        pending=c.call('memory_prepare',args_slow)
        assert pending['status']=='in_progress' and time.monotonic()-started < 50,pending
        calls=PrepareResponses.calls
        resumed=c.call('memory_prepare',args_slow)
        assert resumed['remember']['status']=='completed' and resumed['recall']['status']=='completed',resumed
        assert PrepareResponses.calls==calls+2, 'continuation must only finish source review and recall for the original extraction'
        assert len(state(db)['receipts'])==3
        bundle=export_runtime(next(trace.glob('trace-*.jsonl')))
        slow=next(r for r in bundle['runs'] if r['input']['arguments'].get('user_turn',{}).get('turn_id')=='slow')
        responses=[s['output']['response']['structuredContent'] for s in slow['spans'] if s['name']=='rpc_response']
        assert len(responses)==2 and responses[0]['status']=='in_progress' and responses[1]['remember']['status']=='completed'
        slow_calls=[call for call in bundle['llm_calls'] if call['run_id']==slow['id']]
        assert [call['request']['text']['format']['name'] for call in slow_calls]==[
            'personal_memory_candidates_v1','personal_memory_source_support_v1','personal_memory_runtime_context_v1']
        checked=schema_check(bundle)
        assert checked.returncode==0 and checked.stdout.strip()=='[]',checked.stderr+checked.stdout
        print(json.dumps({'status':'passed','checks':['save before recall','retry without reinforcement','pure question leaves no receipt','source/limit validation','recall failure preserves save','save failure still allows recall','slow call returns pending and resumes without duplicate model work']}))

if __name__=='__main__': main()
