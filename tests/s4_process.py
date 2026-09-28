#!/usr/bin/env python3
"""Frozen S4 development evaluation through real stdio MCP; no provider requests."""
import argparse
import json
import os
import sqlite3
import tempfile
from pathlib import Path
from mcp_smoke import Client
from inspector_support import export_runtime, schema_check


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=Path('target/debug/pmr-mcp'))
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    root = Path('docs/refactor/s4')
    inputs = json.loads((root / 'inputs.json').read_text())
    # These labels are read by the evaluator only; none are passed to MCP.
    labels = {x['id']: x for x in json.loads((root / 'labels.json').read_text())}
    results, all_events = [], []
    with tempfile.TemporaryDirectory(prefix='s4-') as temp:
        directory = Path(temp)
        config = directory / 'config.json'
        config.write_text(json.dumps({'local_recall_enabled': True}))
        trace = directory / 'trace'
        trace.mkdir(mode=0o700)
        env = {k: v for k, v in os.environ.items() if not k.startswith(('DEEPSEEK_', 'OPENAI_', 'PMR_'))}
        env.update(PMR_MODEL_NETWORK='deny', PMR_TRACE_DIR=str(trace), DEEPSEEK_API_KEY='synthetic-unused', DEEPSEEK_BASE_URL='http://127.0.0.1:1/v1')
        for case in inputs:
            db = directory / (case['id'] + '.sqlite3')
            c = Client(args.binary.resolve(), db, env, case['owner'], config=config)
            try:
                for name in ['松石47', '赤铜82']:
                    c.call('memory_entity_upsert', dict(id=name, canonical_name=name, aliases=[], entity_type='Other'))
                ids = {}
                for src in case['sources']:
                    captured = c.call('memory_remember', dict(text=src['text'], session_id=case['id'], turn_id=src['id'], capture_source=True), error=True)
                    assert captured['source']['status'] == 'stored', captured
                    assert captured['index']['status'] == 'ready', captured
                    assert captured['enrichment']['error'] == 'model_network_disabled', captured
                    ids[captured['source']['source_id']] = src['id']
                with sqlite3.connect(db) as connection:
                    before = list(connection.iterdump())
                r = c.call('memory_recall', {'query':case['query'], 'context':case['context']})
                with sqlite3.connect(db) as connection:
                    assert list(connection.iterdump()) == before, 'normal local recall must not write counters or state'
                assert r['backend_generation_calls_in_recall'] == 0 and r['retrieval_mode'] == 'local'
                assert r['budget']['content_bytes'] <= 24000
                final = r['source_evidence'] + [v['source'] for v in r['historical_or_conflicting'] if 'source' in v]
                found = {ids[s['source_id']] for s in final}
                label = labels[case['id']]
                points = {sid: all(point in next(s['text'] for s in final if ids[s['source_id']] == sid)
                                   for point in label['required_points'].get(sid, [])) for sid in found}
                needed = set(label['required_sources'])
                complete = needed <= found and all(points[x] for x in needed)
                clarification = r['reference_resolution'].get('needs_clarification') is True and not found
                negative_ok = (r['evidence_status'] == 'ambiguous' or clarification) and not found if label['answerability'] == 'ambiguous' else (not found if label['answerability'] == 'absent' else r['evidence_status'] == 'sufficiency_not_verified' or clarification)
                row = dict(id=case['id'], category=label['category'], answerability=label['answerability'], query=case['query'], context=case['context'],
                           required_sources=sorted(needed), final_sources=sorted(found), complete_evidence=complete if label['answerability'] == 'answerable' else None, available_required_evidence_complete=complete,
                           unnecessary_sources=sorted(found - needed), forbidden_found=sorted(found & set(label['forbidden_sources'])),
                           safety_behavior_passed=negative_ok, recall=r, mcp_result_bytes=len(json.dumps(r, ensure_ascii=False).encode()),
                           failure_type='unrelated_evidence' if found & set(label['forbidden_sources']) else None if complete and negative_ok else 'missing_evidence' if not complete else 'ambiguity_or_noise', host_answer='not_observed')
                results.append(row)
                if case['id'] in ('N06-no-context', 'N06-ambiguous'):
                    assert r['evidence_status'] == 'ambiguous' and not found, r
                # Local prepare can recall a newly captured source even though enrichment is denied.
                if case['id'] == 'N01':
                    prepared = c.call('memory_prepare', {'user_turn':dict(text='合成验收雪松65使用 SQLite。<synthetic-only>',session_id='prepare',turn_id='new',capture_source=True)})
                    assert prepared['remember']['status'] == 'failed' and prepared['recall']['status'] == 'completed', prepared
                    assert any('雪松65' in s['text'] for s in prepared['recall']['result']['source_evidence'])
            finally:
                c.close()
        for path in trace.glob('*.jsonl'):
            all_events.extend(json.loads(line) for line in path.read_text().splitlines())
        assert not any(e['kind'] == 'model_start' for e in all_events), 'zero actual model sends including capture failures'
        recalls = {e['run_id']: e['data'] for e in all_events if e['kind'] == 'local_recall'}
        lexical = {e['run_id']: e['data'] for e in all_events if e['kind'] == 'lexical_raw'}
        for row in results:
            run = next(run for run, r in recalls.items() if all(row['recall'].get(k) == v for k, v in r.items()))
            raw = lexical[run]
            row['query_phrases'] = raw.get('query_phrases')
            row['candidate_sources'] = sorted({s['turn_id'] for s in raw['candidates']})
            row['candidate_complete'] = (set(row['required_sources']) <= set(row['candidate_sources'])) if row['answerability'] == 'answerable' else None
            row['actual_query_at'] = row['recall']['conditions_provenance'][0]['value']['at']
            row['baseline'] = dict(mode='structured-only raw-only snapshot; no planner run', evidence_sources=[], note='not the S6 A baseline')
        raw_path = directory / 'events.jsonl'
        raw_path.write_text(''.join(json.dumps(e, ensure_ascii=False) + '\n' for e in all_events))
        bundle = export_runtime(raw_path)
        checked = schema_check(bundle)
        assert checked.returncode == 0 and checked.stdout.strip() == '[]', checked.stdout + checked.stderr
        import subprocess
        render = subprocess.run(['node', '-e', r"""
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const nodes=new Map();let network=0;
const document={getElementById(id){if(!nodes.has(id))nodes.set(id,{style:{},addEventListener(){}});return nodes.get(id);},querySelectorAll(){return [];},addEventListener(){}};
const context=vm.createContext({document,window:{scrollTo(){},addEventListener(){}},location:{protocol:'file:'},clearTimeout(){},setTimeout(){},fetch(){network++;throw Error('network forbidden');},fixture:JSON.parse(fs.readFileSync(0,'utf8'))});
const html=fs.readFileSync('inspector/web/index.html','utf8');
vm.runInContext(html.split('<script>')[1].split('</script>')[0],context);
const rendered=vm.runInContext(`load(fixture);bundle.runs.filter(run=>run.local_recall).map(run=>runDetail(run)).join('')`,context);
for(const text of ['本地双通道召回','查询条件来源','选择／排除原因','后台生成调用','&lt;synthetic-only&gt;'])assert.ok(rendered.includes(text),text);
assert.ok(!rendered.includes('<synthetic-only>'));assert.equal(network,0);
console.log(JSON.stringify({status:'passed',network_requests:network,scope:'shipped Node rendering harness; not browser automation'}));
"""],input=json.dumps(bundle),capture_output=True,text=True,check=True)
        report = dict(mode='local', data_kind='synthetic development; actual MCP binaries', model_network='deny', model_start_events=0,
                      cases=results, schema_warnings=[], inspector_render=json.loads(render.stdout), host_input_and_answer='not_observed', release_acceptance=False)
        if args.output:
            args.output.mkdir(parents=True,exist_ok=True)
            (args.output / 'development-results.json').write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n')
            (args.output / 'runtime-events.jsonl').write_text(raw_path.read_text())
            (args.output / 'trace-bundle.json').write_text(json.dumps(bundle, ensure_ascii=False, indent=2)+'\n')
        for row in results:
            print(json.dumps({k:row[k] for k in ['id','answerability','candidate_complete','complete_evidence','final_sources','unnecessary_sources','safety_behavior_passed','failure_type']},ensure_ascii=False))
        print(json.dumps({'contract_status':'passed','real_model_calls':0,'case_count':len(results),'schema_warnings':[]}))

if __name__ == '__main__':
    main()
