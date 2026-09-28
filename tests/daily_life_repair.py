#!/usr/bin/env python3
"""Isolated backend probes and evidence checks for the daily-life web repair."""
import argparse, hashlib, json, os, sqlite3, sys, time, tomllib
from pathlib import Path
from mcp_smoke import Client

BASE = Path.home()/'.local/share/personal-memory-runtime/replay-datasets/2026-09-09/daily-life-01'
ROOT = Path.home()/'.local/share/personal-memory-runtime/replay-datasets/2026-09-09/daily-life-repair'

def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2)+'\n')
    path.chmod(0o600)

def fact_text(m):
    return str(m['value'])

def backend(binary, label, repeat):
    config=tomllib.loads((Path.home()/'.codex/config.toml').read_text())['mcp_servers']['personal_memory']
    env=dict(os.environ, **config.get('env', {}));env.update(PMR_OPENAI_TRACE='1',PMR_OPENAI_TRACE_CONTENT='1')
    prompt=json.loads((BASE/'C01.case.json').read_text())['prompt']
    cases=[('ambiguous',[],prompt,False),
           ('rental',[],'我租了一套公寓，每两周去住一个周末，也会自己做饭。',False),
           ('explicit',[],'我拥有一套公寓，产权在我名下，每两周去住一个周末。',True),
           ('hypothetical',[],'假设我拥有一套公寓，每两周去住一个周末，应该怎么打扫？',False),
           ('comparison',[],json.loads((BASE/'C04.case.json').read_text())['prompt'],False),
           ('no_save',[],'这条不要保存：我有一套公寓，产权在我名下。',False)]
    root=ROOT/label;root.mkdir(parents=True,exist_ok=False,mode=0o700)
    write(root/'version.json',{'binary':str(binary),'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()})
    outcomes=[]
    for n in range(repeat):
        for name,history,text,expected_owns in cases:
            client=Client(binary.resolve(),root/f'{name}-{n}.sqlite3',env,owner='repair-test-user')
            try:
                saved=client.call('memory_remember',dict(text=text,history=history,session_id=f'repair-{label}-{name}-{n}',turn_id='1'))
                listing=client.call('memory_list',{'limit':100})
                rows=client.stored_memories()
                owns=[m for m in rows if m['predicate'] in ('owns','owns_device')]
                passed=bool(owns)==expected_owns and (bool(rows) if name not in {'hypothetical','comparison','no_save'} else not rows and not listing.get('pending'))
                record=dict(case=name,repeat=n,text=text,save=saved,listing=listing,memories=rows,passed=passed,expected_owns=expected_owns)
                write(root/f'{name}-{n}.json',record)
                outcomes.append({'case':name,'repeat':n,'passed':passed,'memories':len(rows),'owns':[fact_text(m) for m in owns]})
                print(json.dumps(outcomes[-1],ensure_ascii=False),flush=True)
            except Exception as error:
                record={'case':name,'repeat':n,'passed':False,'error':str(error)}
                write(root/f'{name}-{n}.json',record)
                outcomes.append(record)
                print(json.dumps(record,ensure_ascii=False),flush=True)
            finally:
                client.errors.seek(0);(root/f'{name}-{n}.stderr').write_text(client.errors.read());(root/f'{name}-{n}.stderr').chmod(0o600);client.close()
    write(root/'summary.json',outcomes)
    return 0 if all(x['passed'] for x in outcomes) else 1

def baseline():
    missing=[]
    for name in ['R01','C02']:
        data=json.loads((BASE/f'web-{name}.result.json').read_text())
        present=any(c['tool']=='memory_remember' and c['status']=='completed' for c in data['calls'])
        if not present: missing.append(name)
    state=json.loads((BASE/'final-isolated-state.json').read_text())
    # The recorded faulty assertion is read from the actual captured SQLite state.
    rows=state['repository']['memories'].values()
    unsupported=[m['id'] for m in rows if m['predicate']=='owns']
    result=dict(missing_saves=missing,unsupported_ownership=unsupported,passed=not missing and not unsupported)
    write(ROOT/'baseline-recorded.json',result);print(json.dumps(result,ensure_ascii=False))
    return 0 if result['passed'] else 1

def selfcheck():
    import copy
    from auto_memory_acceptance import evaluate
    state={'repository':{'memories':{}},'receipts':{},'staging':{'pending':{}}}
    case={'readonly':True,'allow_prepare_readonly':True,'tools':['memory_prepare']}
    args={'user_turn':{'text':'这是什么？','session_id':'test','turn_id':'1'}}
    def call(value):
        return {'type':'item.completed','item':{'type':'mcp_tool_call','tool':'memory_prepare','status':'completed','arguments':copy.deepcopy(args),'result':{'structured_content':value}}}
    pending=call({'status':'in_progress'})
    finished=call({'remember':{'status':'completed','result':{'outcomes':[]}},'recall':{'status':'completed','result':{}}})
    end={'type':'turn.completed'}
    assert evaluate(case,[pending,finished,end],state,state,0)['status']=='passed'
    assert evaluate(case,[pending,end],state,state,0)['status']=='failed'
    wrong=copy.deepcopy(finished);wrong['item']['arguments']['user_turn']['turn_id']='2'
    assert evaluate(case,[pending,wrong,end],state,state,0)['status']=='failed'
    assert evaluate({**case,'allow_prepare_readonly':False},[pending,finished,end],state,state,0)['status']=='failed'
    print('continuation checker passed: missing final result, changed parameters and forbidden writes are detected')
    return 0

def verify(number):
    failures=[]
    def need(condition, message):
        if not condition: failures.append(message)
    def read(path):
        if not path.exists():
            failures.append('missing evidence: '+str(path.relative_to(ROOT)))
            return {}
        return json.loads(path.read_text())
    web=ROOT/'web-v4'
    manifest=read(ROOT/'final-source-manifest.json')
    for name, digest in manifest.get('files',{}).items():
        need(Path(name).exists() and hashlib.sha256(Path(name).read_bytes()).hexdigest()==digest,
             'source changed after evidence inventory: '+name)
    need(bool(manifest.get('files')), 'source inventory absent')
    if number==1:
        for case in ['R01','C02']:
            result=read(web/f'web-{case}.result.json')
            after=read(web/f'web-{case}.after.json')
            need(result.get('status')=='passed', case+' web save check failed')
            quote=result.get('case',{}).get('prompt')
            rows=after.get('repository',{}).get('memories',{}).values()
            need(any(m['status']=='Active' and any(e['quote']==quote for e in m['evidence']) for m in rows),case+' original evidence absent')
            need(any(c['tool']=='memory_prepare' and c['arguments']['user_turn']['text']==quote for c in result.get('calls',[])),case+' verbatim prepare absent')
        for case in ['P01','P03']:
            result=read(web/f'web-{case}.result.json')
            need(result.get('status')=='passed',case+' cross-chat recall not verified')
            recalled=[]
            for call in result.get('calls',[]):
                content=call.get('result',{}).get('structured_content',{})
                if call['tool']=='memory_recall' and not call['arguments'].get('bootstrap'):
                    context=content.get('result',{})
                elif call['tool']=='memory_prepare' and content.get('recall',{}).get('status')=='completed':
                    context=content['recall']['result'].get('result',{})
                else:
                    continue
                for section in ['mandatory','constraints','preferences','facts','episodes']:
                    recalled.extend(context.get(section,[]))
            expected={11} if case=='P01' else {14,15,16,17,20}
            need(expected.issubset({m['id'] for m in recalled}),case+' expected saved facts absent from actual semantic tool result')

    elif number==2:
        result=read(web/'web-C01.result.json');after=read(web/'web-C01.after.json')
        need(result.get('status')=='passed','C01 web result not passed')
        quote=result.get('case',{}).get('prompt')
        current=[m for m in after.get('repository',{}).get('memories',{}).values() if any(e['quote']==quote for e in m['evidence'])]
        need(bool(current) and not any(m['predicate'] in ('owns','owns_device') for m in current),'C01 missing facts or unsupported ownership')
        need(read(ROOT/'offline-final-policy-run.json').get('exit_code')==0,'offline suite not passed')
        need(read(ROOT/'live32-release-run.json').get('exit_code')==0,'latest full 32 live run did not pass (31 passed; PMV1-17 TLS failure; retry blocked by API balance)')
        boundary=read(ROOT/'backend-final/summary.json')
        need(isinstance(boundary,list) and len(boundary)==18 and all(x['passed'] for x in boundary),'18 boundary probes not all passed (6 passed; 12 API balance errors)')
    elif number==3:
        restored=read(ROOT/'restoration.json')
        need(restored.get('formal_state_unchanged') is True,'formal state restoration unverified')
        need(restored.get('profile_restored') is True,'formal route restoration unverified')
        need(restored.get('remote_health',{}).get('owner')=='local-user','formal remote health missing')
        need(restored.get('ui',{}).get('builtin_memory_on') is True,'builtin memory restoration unverified')
        need(restored.get('ui',{}).get('policy_exact') is True,'final account policy not verified')
        profile=Path.home()/'.config/tunnel-client/personal-memory.yaml'
        need(profile.read_bytes()==(web/'profile-before.json').read_bytes(),'current route differs from original formal profile')
        with sqlite3.connect('file:'+str(Path.home()/'.local/share/personal-memory-runtime/memory.sqlite3')+'?mode=ro',uri=True) as conn:
            actual=conn.execute("select state_json from memory_profiles where owner_id='local-user'").fetchone()[0]
        need(actual==(web/'formal-state-before.json').read_text(),'current formal state differs from baseline')
        for path in web.glob('web-*.result.json'):
            result=json.loads(path.read_text())
            need(result.get('surface')=='ChatGPT web via native CUA' and result.get('observation',{}).get('chat_url','').startswith('https://chatgpt.com/c/'),'missing real UI provenance: '+path.name)
        final=read(web/'final-isolated-state.json')
        need(all(m['owner_id']=='auto-test-user' for m in final.get('repository',{}).get('memories',{}).values()),'non-test owner in isolated state')
    observed='; '.join(failures) if failures else 'Recorded evidence passed within the listed samples and versions; this is not independent semantic or universal model-behavior proof.'
    print(json.dumps({'acf_assertion':{'id':f'repair-{number}','outcome':'fail' if failures else 'pass','observed':observed}},ensure_ascii=False))
    return int(bool(failures))

if __name__=='__main__':
    os.umask(0o077)
    parser=argparse.ArgumentParser();parser.add_argument('mode',choices=['baseline','backend','verify','selfcheck']);parser.add_argument('number',type=int,nargs='?',choices=[1,2,3]);parser.add_argument('--binary',type=Path,default=Path('target/debug/pmr-mcp'));parser.add_argument('--label',default=str(time.time_ns()));parser.add_argument('--repeat',type=int,default=1);args=parser.parse_args()
    sys.exit(selfcheck() if args.mode=='selfcheck' else verify(args.number) if args.mode=='verify' else baseline() if args.mode=='baseline' else backend(args.binary,args.label,args.repeat))
