#!/usr/bin/env python3
"""Offline S1 acceptance: actual stdio/SQLite, synthetic loopback Responses only."""
import argparse
import contextlib
import http.server
import http.client
import json
import os
import socket
import tempfile
import threading
from datetime import datetime, timezone
from pathlib import Path

from mcp_smoke import Client, Responses
from inspector_support import export_runtime, schema_check, start_inspector


class Fixture(Responses):
    mode = 'success'
    requests = []

    def do_POST(self):
        request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        type(self).requests.append(request)
        schema=request['text']['format']['name']
        output=self.output(request)
        if self.mode=='incomplete' and schema=='personal_memory_candidates_v1':
            output={'candidates':[], 'omission_reason':'no_personal_assertion'}
        if self.mode=='other_fact' and schema=='personal_memory_candidates_v1':
            output['candidates'][0].update(memory_type='fact',predicate='fact',retention_tier='long_term')
            output['candidates'][0]['value']['text']='现在我的测试属性为固定值。'
            # Preserve the diagnostic unbounded-range check with an actual USER time qualifier.
            output['candidates'][0]['temporal_expression']='现在'
        if self.mode=='rejected' and schema=='personal_memory_candidates_v1':
            output['candidates'][0]['source']='assistant_inference'
        incomplete=self.mode=='incomplete' and ('source_support' in schema or 'runtime_context' in schema)
        data=dict(id=f'fixture-{len(self.requests)}', model='synthetic-local-fixture',status='incomplete' if incomplete else 'completed',
            usage=dict(input_tokens=123,output_tokens=4096 if incomplete else 10,
                output_tokens_details={'reasoning_tokens':4096 if incomplete else 0}),
            output=[{'type':'reasoning','summary':[{'text':'HIDDEN_REASONING_MUST_NOT_BE_COLLECTED'}],
                     'content':[{'type':'reasoning_text','text':'HIDDEN_REASONING_MUST_NOT_BE_COLLECTED'}]},
                    {'type':'message','content':[{'type':'output_text','text':json.dumps(output,ensure_ascii=False)}]}])
        if incomplete:data['incomplete_details']={'reason':'max_output_tokens'}
        if self.mode=='http_error':data={'error':{'message':'Authorization: Bearer sensitive-fixture-token'}}
        raw=(b'not-json' if self.mode=='bad_json' else json.dumps(data).encode())
        self.send_response(503 if self.mode=='http_error' else 200)
        self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)))
        self.end_headers();self.wfile.write(raw)


def checked(directory):
    files=list(directory.glob('trace-*.jsonl'))
    assert len(files)==1,files
    bundle=export_runtime(files[0])
    validation=schema_check(bundle)
    assert validation.returncode==0 and validation.stdout.strip()=='[]',validation.stderr+validation.stdout
    return files[0],bundle


def main():
    parser=argparse.ArgumentParser();parser.add_argument('output',nargs='?',type=Path);parser.add_argument('--binary',type=Path,default=Path('target/debug/pmr-mcp'))
    arguments=parser.parse_args();binary=arguments.binary.resolve()
    with contextlib.ExitStack() as stack:
        root=Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='pmr-s1-')))
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Fixture)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        stack.callback(server.server_close);stack.callback(server.shutdown)
        trace=root/'trace';trace.mkdir(mode=0o700)
        env=dict(os.environ,PMR_MODEL_NETWORK='offline',PMR_TRACE_DIR=str(trace),
            DEEPSEEK_API_KEY='local-fixture-key',DEEPSEEK_BASE_URL=f'http://127.0.0.1:{server.server_port}/v1')
        env.pop('PMR_OPENAI_TRACE_CONTENT',None)
        c=Client(binary,root/'memory.sqlite3',env,budget_calls=100);stack.callback(c.close)
        args={'user_turn':{'text':'以后所有回答都必须使用中文。','session_id':'s1-synthetic','turn_id':'failure'}}
        Fixture.mode='incomplete'
        failed=c.call('memory_prepare',args)
        assert failed['remember']['status']==failed['recall']['status']=='failed'
        Fixture.mode='success'
        c.call('memory_recall',{'query':'使用什么语言回答？'})
        args['user_turn']['turn_id']='success'
        saved=c.call('memory_prepare',args)
        assert saved['remember']['status']==saved['recall']['status']=='completed'
        expected_saved={'outcomes':[{'Ingested':{'Applied':{'action':'New','memory_id':1,'target':None}}}], 'replayed':False}
        assert {k:saved['remember']['result'][k] for k in expected_saved}==expected_saved,'diagnostic traces must not change public outcomes'
        assert saved['remember']['result']['enrichment_mode']=='inline'
        count=len(Fixture.requests)
        c.call('memory_recall',{'query':'全局回答要求','bootstrap':True})
        assert len(Fixture.requests)==count
        replayed=c.call('memory_prepare',args)  # persisted receipt replay: query only
        assert replayed['remember']['result']['original_result']==expected_saved
        assert replayed['remember']['result']['replayed'] is True
        assert replayed['remember']['result']['enrichment_mode']=='inline'
        assert len(Fixture.requests)==count+1
        c.call('memory_clarify',dict(staged_id=999,entity_reference='self',text='fixture',session_id='s1',turn_id='bad'),error=True)
        for mode in ('http_error','bad_json'):
            Fixture.mode=mode
            before=len(Fixture.requests)
            c.call('memory_recall',{'query':'fixture'},error=True)
            assert len(Fixture.requests)==before+1,'HTTP/body failures must not retry'
        Fixture.mode='other_fact'
        c.call('memory_remember',dict(text='现在我的测试属性为固定值。',session_id='s1',turn_id='other'))
        Fixture.mode='success'
        c.call('memory_recall',{'query':'语言要求'})
        Fixture.mode='rejected'
        rejected_response=c.call('memory_remember',dict(text='仅助手猜测',session_id='s1',turn_id='rejected'))
        assert rejected_response['outcomes']==[{'Ingested':{'Rejected':'source is not user evidence'}}]
        trace_path,bundle=checked(trace)
        assert len(bundle['llm_calls'])==len(Fixture.requests)
        for run in bundle['runs']:
            model_spans=[span for span in run['spans'] if '第 ' in span['name']]
            assert len(model_spans)==len([call for call in bundle['llm_calls'] if call['run_id']==run['id']])
            assert all(span['status']!='in_progress' and span['duration_ms']>=0 and span['output'] for span in model_spans)
        a,b,success,bootstrap,replay,rollback,http_error,bad_json,other_save,filtered_recall,rejected=bundle['runs']
        passed_checks=[dict(rule=rule,status=status,reason=reason) for rule,status,reason in [
            ('evidence_complete','Passed','evidence is complete'),
            ('subject_resolved','Passed','subject is resolved'),
            ('extraction_confidence','Passed','extraction confidence is not low'),
            ('user_evidence','Passed','source is user evidence'),
            ('episode_value','NotApplicable','candidate is not an episode'),
            ('fact_value_representation','NotApplicable','generic fact retains a complete USER sentence; dedicated predicates keep typed values'),
            ('source_support','Passed','candidate matches source-reviewed snapshot')]]
        rejected_checks=passed_checks[:3]+[{
            'rule':'user_evidence','status':'Failed','reason':'source is not user evidence'}]
        rejected_decision=next(s for s in rejected['spans'] if s['name'].startswith('候选规则裁决'))
        assert rejected_decision['output']['result']['Ingested']['Rejected']
        assert rejected_decision['output']['decision_trace']=={
            'admission':{'Reject':'source is not user evidence'},'admission_reason':'source is not user evidence',
            'admission_checks':rejected_checks,
            'existing_memory_ids':[],'matched_memory_ids':[],
            'reconciliation':None,'reconciliation_reason':None}
        assert not rejected['write_changes']
        assert any(x['outcome']=='filtered' and 'Predicate' in x['reason_codes'][0] for x in filtered_recall['retrieval']['candidates'])
        dated=next(m for m in bundle['memories'] if m['raw']['predicate']=='fact')
        observed=datetime.fromtimestamp(dated['raw']['temporal']['observed_at'],timezone.utc).isoformat()
        assert dated['valid_from']==observed and dated['valid_to'] is None and dated['valid_to_unbounded']
        assert a['save_status']=='not_committed' and a['recall_status']=='planning_incomplete'
        assert a['retrieval']['candidates'] is None
        assert [x['status'] for x in bundle['llm_calls'][:3]]==['completed','incomplete','incomplete']
        assert bundle['llm_calls'][1]['finish_reason']=='max_output_tokens'
        assert b['recall_status']=='completed_empty' and b['retrieval']['candidates']==[]
        assert success['save_status']=='committed' and success['write_changes'][0]['before'] is None
        assert success['write_changes'][0]['after']['predicate']=='response_language'
        assert success['write_changes'][0]['candidate']['predicate']=='response_language'
        assert success['write_changes'][0]['receipt_id'] and success['write_changes'][0]['llm_call_ids']
        decisions=[s for s in success['spans'] if s['name'].startswith('候选规则裁决')]
        assert decisions and decisions[0]['input']['predicate']=='response_language'
        assert decisions[0]['output']['result']['Ingested']['Applied']['action']=='New'
        assert decisions[0]['output']['decision_trace']=={
            'admission':'Admit','admission_reason':'admission policy admitted candidate',
            'admission_checks':passed_checks,
            'existing_memory_ids':[],'matched_memory_ids':[],
            'reconciliation':{'action':'New','target':None,'reason':None},
            'reconciliation_reason':'no active equal-scope memory matched'}
        committed=next(s for s in success['spans'] if s['name']=='transaction_committed')
        receipt=committed['output']['after']['receipts']['["memory_remember","s1-synthetic","success"]']
        assert receipt['result']==expected_saved,'persisted receipts must keep the original outcome shape'
        rejected_commit=next(s for s in rejected['spans'] if s['name']=='transaction_committed')
        rejected_receipt=rejected_commit['output']['after']['receipts']['["memory_remember","s1","rejected"]']
        assert rejected_receipt['result']=={'outcomes':rejected_response['outcomes'],'replayed':False}
        assert all(s['duration_ms']>=0 for s in success['spans'] if s['name'] in ('retrieval','transaction_committed','global_context') or 'Rust 校验' in s['name'])
        assert success['retrieval']['selected_refs'] and bootstrap['retrieval']['selected_refs']
        assert isinstance(success['read_revision'],str) and isinstance(success['committed_revision'],str)
        assert int(success['committed_revision']) > int(success['read_revision']) >= 0
        assert all(x['cost']['amount'] is None for x in bundle['llm_calls'])
        assert not [x for x in bundle['llm_calls'] if x['run_id']==bootstrap['id']]
        assert len([x for x in bundle['llm_calls'] if x['run_id']==replay['id']])==1
        assert any(s['name']=='transaction_rollback' for s in rollback['spans'])
        assert not [s for s in rollback['spans'] if s['name']=='transaction_committed']
        assert all(r['evidence_status']=='complete' for r in bundle['runs'])
        text=trace_path.read_text()
        candidate_events={r['event_id']:r['data'] for r in map(json.loads,text.splitlines()) if r['kind']=='candidate_decision'}
        for span in (decisions[0],rejected_decision):
            assert span['output']==candidate_events[span['id']],'export must preserve the actual decision trace and result'
        assert 'HIDDEN_REASONING_MUST_NOT_BE_COLLECTED' not in text
        assert 'sensitive-fixture-token' not in text and 'local-fixture-key' not in text
        assert trace_path.stat().st_mode & 0o777==0o600
        for call,actual in zip(bundle['llm_calls'],Fixture.requests):
            assert call['request']==actual, 'record actual outgoing JSON, not reconstructed template'
        # Actual Rust HTTP reads merge S1 evidence without model calls.
        live_trace=root/'live-trace';live_trace.mkdir(mode=0o700)
        live_file=live_trace/trace_path.name;live_file.write_text(text)
        live_log=root/'inspector.log';live_log.write_text('')
        inspector=stack.enter_context(start_inspector(root/'memory.sqlite3','smoke-user',[live_log],live_trace))
        count=len(Fixture.requests)
        def read_live():
            conn=http.client.HTTPConnection('127.0.0.1',inspector.server_port)
            conn.request('GET','/api/live',headers={'Cookie':f'{inspector.cookie_name}={inspector.token}'})
            response=conn.getresponse();assert response.status==200
            data=json.loads(response.read());conn.close()
            check=schema_check(data)
            assert check.returncode==0 and check.stdout.strip()=='[]',check.stderr+check.stdout
            return data
        live=read_live()
        assert live['live']['trace_status']=='available' and len(live['llm_calls'])==len(bundle['llm_calls'])
        assert live['memory_snapshot']['completeness']=='complete'
        assert live['runs'][0]['id']==rejected['id']
        assert {r['id'] for r in bundle['runs']} <= {r['id'] for r in live['runs']}
        for recorded_run,span in ((success,decisions[0]),(rejected,rejected_decision)):
            live_run=next(r for r in live['runs'] if r['id']==recorded_run['id'])
            assert next(s['output'] for s in live_run['spans'] if s['id']==span['id'])==span['output'], \
                'live must preserve ordered admission checks, including failed and not-applicable rules'
        live_file.write_text(text+'{"partial":')
        partial_live=read_live()
        assert partial_live['live']['status']=='partial' and partial_live['llm_calls']==live['llm_calls']
        live_file.write_text(text+'{broken}\n')
        corrupt_live=read_live()
        assert corrupt_live['live']['trace_status']=='evidence_incomplete'
        assert corrupt_live['memory_snapshot']['completeness']=='complete'
        assert len(Fixture.requests)==count,'Inspector reads must never call models'
        # Missing completion is incomplete evidence, never a successful empty result.
        rows=[json.loads(line) for line in text.splitlines()]
        partial=root/'partial.jsonl'
        partial.write_text('\n'.join(json.dumps(r) for r in rows[:3])+'\n')
        assert export_runtime(partial)['runs'][0]['evidence_status']=='evidence_incomplete'
        partial.write_text(text+'{broken}\n')
        try:export_runtime(partial)
        except ValueError:pass
        else:raise AssertionError('corrupt JSONL accepted')
        # Diagnostic failures must preserve successful writes and report the missing evidence.
        Fixture.mode='success'
        broken_env=dict(env,PMR_TRACE_DIR=str(root/'absent'))
        broken=Client(binary,root/'broken-diagnostics.sqlite3',broken_env,budget_calls=100);stack.callback(broken.close)
        result=broken.call('memory_prepare',args)
        assert result['diagnostics']['status']=='evidence_incomplete'
        assert result['remember']['status']=='completed' and result['recall']['result']['result']['mandatory']
        # Connection failure: S5 permits one physical send, with no automatic retry.
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        retries=root/'retries';retries.mkdir(mode=0o700)
        retry_env=dict(env,PMR_TRACE_DIR=str(retries),DEEPSEEK_BASE_URL=f'http://127.0.0.1:{port}/v1')
        retry=Client(binary,root/'retry.sqlite3',retry_env,budget_calls=100);stack.callback(retry.close)
        retry.call('memory_recall',{'query':'fixture'},error=True)
        _,retry_bundle=checked(retries)
        assert len(retry_bundle['llm_calls'])==1
        assert [x['attempt'] for x in retry_bundle['llm_calls']]==[1]
        assert all(x['status']=='failed' for x in retry_bundle['llm_calls'])
        # Optional artifact output, contains synthetic fixture text only.
        if arguments.output:
            out=arguments.output;out.mkdir(parents=True,exist_ok=True)
            bundle['meta']['notes']='实际 MCP/SQLite 执行 + 本地合成 Responses 桩；不证明真实模型语义质量。'
            (out/'runtime-fixture.json').write_text(json.dumps(bundle,ensure_ascii=False,indent=2)+'\n')
            (out/'retry-fixture.json').write_text(json.dumps(retry_bundle,ensure_ascii=False,indent=2)+'\n')
            (out/'runtime-events.jsonl').write_text(text)
        print(json.dumps(dict(status='passed',checks=['actual requests','stable physical attempts','incomplete review',
            'empty recall','commit snapshots','candidate decision traces','ordered admission checks and rejection short-circuit','unchanged outcome and receipt shapes','retrieval before clear','receipt replay','rollback','HTTP/body errors',
            'diagnostic failure isolation','private permissions','redaction/no reasoning bodies','schema/links','partial/corrupt evidence','Rust realtime backend merges traces without model calls'],
            provider_calls=0,fixture_calls=len(Fixture.requests)),ensure_ascii=False))


if __name__=='__main__':main()
