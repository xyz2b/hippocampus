// Exercise the shipped Inspector renderer and navigation with synthetic exported versions.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const html = fs.readFileSync('inspector/web/index.html', 'utf8');
const nodes = new Map(), handlers = {}, windowHandlers = {};
const location = {protocol:'file:', hash:''};
const history = {replaceState(_state, _title, url){location.hash=String(url).includes('#')?'#'+String(url).split('#').slice(1).join('#'):'';}};
const document = {
  getElementById(id){
    if(!nodes.has(id))nodes.set(id,{style:{},handlers:{},addEventListener(name,fn){this.handlers[name]=fn;}});
    return nodes.get(id);
  },
  querySelectorAll(){return [];},
  addEventListener(name,fn){handlers[name]=fn;}
};
let network = 0, scrolls = 0;
const context = vm.createContext({document,location,history,AbortController,
  window:{location,history,scrollY:0,scrollTo(){scrolls++;},addEventListener(name,fn){windowHandlers[name]=fn;}},
  clearTimeout(){},setTimeout(){},fetch(){network++;throw Error('network forbidden');}});
const evaluate = code => vm.runInContext(code,context);
vm.runInContext(html.split('<script>')[1].split('</script>')[0],context);
evaluate(`
  const fixtureRaw=(id,value)=>({id,owner_id:'synthetic-owner',subject_id:'self',predicate:'fixture',value:{Text:value},status:'Active',evidence:[]});
  const fixtureMemory=(id,version,value)=>({id:String(id),version,content:value,status:'active',owner:'synthetic-owner',source_ids:[],raw:fixtureRaw(id,value)});
  const fixtureOld=fixtureMemory(1,'old','synthetic old'),fixtureNew=fixtureMemory(1,'new','synthetic new');
  const fixtureSingle=fixtureMemory(2,'only','synthetic single'),fixtureAdded=fixtureMemory(3,'added','synthetic added');
  const fixtureHostile=fixtureMemory('x"><svg onload=alert(1)>','hostile','synthetic escaped');
  const fixture={schema_version:'0.1.0',meta:{title:'Memory links synthetic fixture',data_kind:'demo'},memory_snapshot:{completeness:'complete',scope:'synthetic-owner'},sources:[],memories:[fixtureOld,fixtureNew,fixtureSingle,fixtureAdded,fixtureMemory(0,'zero','zero id'),fixtureHostile],runs:[],llm_calls:[]};
  load(fixture);
`);
const field = (markup,label) => {
  const match=markup.match(new RegExp('<label>'+label+'</label><div>([\\s\\S]*?)</div>'));
  assert.ok(match,'field missing: '+label);
  return match[1];
};
const exactHref = reference => '#memory='+encodeURIComponent(reference);
const idHref = id => '#memory-id='+encodeURIComponent(id);
const runHref = id => '#run='+encodeURIComponent(id);
const link = (markup,href) => assert.ok(markup.includes('href="'+href+'"'),'missing link '+href+' in '+markup);
const click = (dataset,modifiers={}) => {
  const target={dataset};let prevented=false;
  handlers.click({target:{closest(selector){return selector.includes('button')||selector.includes('a')?target:null;}},
    button:0,ctrlKey:false,metaKey:false,shiftKey:false,altKey:false,...modifiers,preventDefault(){prevented=true;}});
  return prevented;
};
const navigate = hash => {location.hash=hash;evaluate('navigateMemoryHash()');};
const selection = () => JSON.parse(evaluate('JSON.stringify({view,selectedMemory,memoryIdFilter})'));
const content = () => nodes.get('content').innerHTML;
const runSelection = () => JSON.parse(evaluate('JSON.stringify({view,selectedRun,runIdFilter})'));
const runRows = () => (content().match(/<button\b[^>]*data-run=/g)||[]).length;
const renderedListItems = key => [...content().matchAll(/<button\b([^>]*)>/g)].flatMap(([,attributes])=>{
  const className=/\bclass="([^"]*)"/.exec(attributes)?.[1]||'';
  if(!className.split(/\s+/).includes('item'))return [];
  const dataset=Object.fromEntries([...attributes.matchAll(/\bdata-([\w-]+)="([^"]*)"/g)].map(([,name,value])=>[name.replace(/-([a-z])/g,(_,letter)=>letter.toUpperCase()),value]));
  return dataset[key]===undefined?[]:[{dataset,tagName:'BUTTON',className,classList:{contains:name=>className.split(/\s+/).includes(name)},matches:selector=>selector.includes('button')||selector.includes('.item')}];
});
const clickListItem = (key,id) => {
  const item=renderedListItems(key).find(item=>item.dataset[key]===id);
  assert.ok(item,'actual rendered list button missing: '+key+'='+id);
  handlers.click({target:{closest:selector=>selector.includes('button')||selector.includes('.item')?item:null},button:0,preventDefault(){}});
};

const basic=evaluate(`({exact:linkMem('1@old'),bare:linkMemoryId(1),zero:linkMemoryId(0),missing:linkMemoryId(404),snapshot:linkMemoryId(1,fixtureOld.raw),otherOwner:linkMemoryId(1,{...fixtureOld.raw,owner_id:'another-owner'}),hostile:linkMemoryId(fixtureHostile.id),matches:memoryIdMatches(1).length})`);
link(basic.exact,exactHref('1@old'));
assert.match(basic.exact,/<a\b[^>]*data-memory="1@old"/);
link(basic.bare,idHref(1));link(basic.zero,idHref(0));
assert.match(basic.bare,/data-memory-id="1"/);
assert.doesNotMatch(basic.bare,/#memory=1%40(?:old|new)/,'bare ID must not pretend to identify an old version');
assert.equal(basic.matches,2);
assert.doesNotMatch(basic.missing,/<a\b/);
assert.match(basic.missing,/未.*导出|不可用|不存在/);
link(basic.snapshot,exactHref('1@old'));assert.match(basic.snapshot,/记忆 #1/);
assert.doesNotMatch(basic.otherOwner,/#memory=1%40old/,'wrong owner cannot become an exact historical match');
link(basic.hostile,idHref('x"><svg onload=alert(1)>'));
assert.match(basic.hostile,/&lt;svg/);assert.doesNotMatch(basic.hostile,/<svg|data-memory-id="x">/);

const candidate=evaluate(`(()=>{
  const span={input:{value:{Text:'synthetic candidate'},intent:{Supersede:{target:1}},evidence:[]},status:'completed',output:{result:{Ingested:{Applied:{action:'Supersede',memory_id:1,target:2}}},decision_trace:{admission:'Admit',existing_memory_ids:[1,2,404],matched_memory_ids:[2],reconciliation_reason:'fixture'}}};
  return {applied:renderCandidateDetail(span),duplicate:renderCandidateDetail({...span,output:{result:{Ingested:{Duplicate:{memory_id:2,evidence_id:'evidence-not-memory'}}}}}),conflict:renderCandidateDetail({...span,output:{result:{Ingested:{Conflict:{target:1}}}}})};
})()`);
link(field(candidate.applied,'检查过的旧记忆'),idHref(1));
link(field(candidate.applied,'检查过的旧记忆'),idHref(2));
assert.doesNotMatch(field(candidate.applied,'检查过的旧记忆'),/href="#memory-id=404"/);
link(field(candidate.applied,'匹配的有效记忆'),idHref(2));
link(field(candidate.applied,'结果记忆编号'),idHref(1));
link(field(candidate.applied,'目标记忆编号'),idHref(2));
link(field(candidate.applied,'候选提出的更新意图'),idHref(1));
link(field(candidate.duplicate,'已有记忆编号'),idHref(2));
assert.doesNotMatch(field(candidate.duplicate,'重复证据编号'),/<a\b/);
link(field(candidate.conflict,'冲突目标记忆'),idHref(1));

const transaction=evaluate(`renderTransactionDetail({status:'completed',output:{before:{repository:{memories:{1:fixtureOld.raw,2:fixtureSingle.raw}}},after:{repository:{memories:{1:fixtureNew.raw,3:fixtureAdded.raw}}}}})`);
link(transaction,exactHref('1@new'));link(transaction,exactHref('2@only'));link(transaction,exactHref('3@added'));
assert.doesNotMatch(transaction,/href="#memory-id=[123]"/,'transaction snapshots identify the exact exported versions');
const related=evaluate(`({override:retrievalReason({Filtered:{OverriddenBy:1}}),conflict:retrievalReason({Conflict:{with:2,reason:'EqualSpecificity'}}),lexical:renderLexicalDetail({candidates:[{source_id:'synthetic-source',text:'synthetic',known_memories:[{memory_id:1,supersedes:2,superseded_by:3}]}]}),versions:memoryDetail({...fixtureOld,supersedes_id:2,superseded_by_id:3})})`);
link(related.override,idHref(1));link(related.conflict,idHref(2));
for(const id of [1,2,3])link(related.lexical,idHref(id));
link(related.versions,idHref(2));link(related.versions,idHref(3));

click({memoryId:'1'});
assert.deepEqual(selection(),{view:'memories',selectedMemory:null,memoryIdFilter:'1'});
assert.match(content(),/版本|选择/);
assert.doesNotMatch(content(),/<div class="memory-body">synthetic (?:old|new)<\/div>/);
const scrollsBeforeVersion=scrolls;
click({memory:'1@old'});
assert.equal(selection().selectedMemory,'1@old');
assert.equal(scrolls,scrollsBeforeVersion,'opening a version in the filtered view must not jump to the page top');
assert.equal(selection().memoryIdFilter,'1','selecting a version must retain the memory ID scope');
assert.doesNotMatch(content(),/<button[^>]*data-memory="[023]@/,'the version list must not expand to unrelated memories');
assert.match(content(),/<div class="memory-body">synthetic old<\/div>/);
const contentVersions=content().split('<h3>不同的记忆版本</h3>')[1]?.split('</div>')[0];
assert.ok(contentVersions,'different business content retains the version section');
assert.equal((contentVersions.match(/data-memory=/g)||[]).length,2);
link(contentVersions,exactHref('1@old'));link(contentVersions,exactHref('1@new'));
assert.match(content(),/不使用最新版本替代历史运行实际读取的旧版本/);
navigate(idHref(2));assert.equal(selection().selectedMemory,'2@only');
navigate(idHref(1));assert.equal(selection().selectedMemory,null);
assert.equal(selection().memoryIdFilter,'1');
navigate(exactHref('1@new'));assert.equal(selection().selectedMemory,'1@new');
assert.match(content(),/<div class="memory-body">synthetic new<\/div>/);
navigate(exactHref('1@missing'));assert.equal(selection().selectedMemory,'1@missing');
assert.doesNotMatch(content(),/<div class="memory-body">synthetic (?:old|new|single)<\/div>/);
assert.match(content()+nodes.get('message').textContent,/未.*导出|不可用|不存在|缺失|找不到/);
navigate(idHref(404));assert.equal(selection().selectedMemory,null);
assert.doesNotMatch(content(),/<div class="memory-body">/,'missing ID cannot silently show another memory');
navigate(exactHref('1@old'));
navigate('#memory=%E0%A4%A');
assert.match(nodes.get('message').textContent,/无效|错误/);
assert.doesNotMatch(content(),/<div class="memory-body">/,'an invalid URL must not appear to identify the previous memory');
navigate('#memory=');assert.match(nodes.get('message').textContent,/无效|错误/);
assert.doesNotMatch(content(),/<div class="memory-body">/);
assert.equal(typeof windowHandlers.hashchange,'function','hash changes must support browser navigation');
location.hash=exactHref('1@old');windowHandlers.hashchange();
assert.equal(selection().selectedMemory,'1@old');
location.hash=exactHref('1@new');evaluate('load(fixture)');
assert.equal(selection().selectedMemory,'1@new','initial imported data load honors the requested exact version');
assert.equal(selection().memoryIdFilter,'1','loading the version URL restores its memory ID scope');
location.hash=idHref(1);evaluate('load(fixture)');
assert.equal(selection().memoryIdFilter,'1');assert.equal(selection().selectedMemory,null);
location.hash=exactHref('1@missing');evaluate('load(fixture)');
assert.equal(selection().selectedMemory,'1@missing');assert.doesNotMatch(content(),/<div class="memory-body">/);
navigate(exactHref('1@new'));
for(const modifiers of [{metaKey:true},{ctrlKey:true},{shiftKey:true},{altKey:true},{button:1}]){
  assert.equal(click({memory:'1@old'},modifiers),false,'modified links retain native browser behavior');
  assert.equal(location.hash,exactHref('1@new'));
  assert.equal(selection().selectedMemory,'1@new');
}
const statistics=evaluate(`(()=>{
  const first={...fixtureMemory(9,'stats-before','synthetic same fact'),raw:{...fixtureRaw(9,'synthetic same fact'),retrieval_count:0,last_retrieved_at:null}};
  const later={...first,version:'stats-after',raw:{...first.raw,retrieval_count:7,last_retrieved_at:1750000000}};
  const original=JSON.stringify([first,later]);
  selectedMemory=null;
  const plain=memoryVersions([first,later]).map(ref);
  selectedMemory='9@stats-after';
  const selected=memoryVersions([first,later]).map(ref);
  const deltas=[{value:{Text:'changed fact'}},{status:'Superseded'},{evidence:[{id:'additional-evidence'}]},{scope:{task_id:'changed-scope'}},{reinforcement_count:2},{owner_id:'another-owner'}];
  const separate=deltas.map(delta=>memoryVersions([first,{...later,version:'changed',raw:{...later.raw,...delta}}]).length);
  const otherId=memoryVersions([first,{...later,id:'10',raw:{...later.raw,id:10}}]).length;
  const otherOwner=memoryVersions([first,{...later,owner:'another-owner'}]).length;
  const noRaw=memoryVersions([{...first,raw:undefined},{...later,raw:undefined}]).length;
  const run=(id,reference)=>({id,tool:'memory_recall',status:'completed',spans:[],retrieval:{selected_refs:[reference]},mcp_response:{recorded:true,returned_refs:[reference]},host:{input_observability:'unavailable'}});
  globalThis.statisticsFixture={...fixture,memories:[first,later,fixtureSingle],runs:[run('statistics-before-run','9@stats-before'),run('statistics-after-run','9@stats-after')]};
  return {plain,selected,separate,otherId,otherOwner,noRaw,unchanged:JSON.stringify([first,later])===original};
})()`);
assert.equal(JSON.stringify(statistics.plain),'["9@stats-before"]','statistics-only variants share one display version');
assert.equal(JSON.stringify(statistics.selected),'["9@stats-after"]','the selected exact snapshot represents its display group');
assert.ok(statistics.separate.every(count=>count===2),'content, status, evidence, scope, reinforcement and owner differences remain distinct');
assert.equal(statistics.otherId,2,'different memory IDs do not merge');
assert.equal(statistics.otherOwner,2,'different exported owners do not merge');
assert.equal(statistics.noRaw,2,'missing raw evidence must not be guessed equivalent');
assert.equal(statistics.unchanged,true,'display grouping cannot rewrite exported snapshots');
location.hash='';evaluate('load(statisticsFixture);activate("memories")');
assert.equal(Number(nodes.get('memories-count').textContent),2,'sidebar counts display versions, not retrieval-statistics fingerprints');
assert.match(nodes.get('metrics').innerHTML,/记忆[^<]*<\/div><div class="value"[^>]*>2<\/div>/,'top-level count uses the same display grouping');
assert.equal((content().match(/<button class="item /g)||[]).length,2,'the global list contains one row for each business version');
navigate(idHref(9));
assert.ok(['9@stats-before','9@stats-after'].includes(selection().selectedMemory),'a bare ID with only statistics differences opens its single content version');
assert.equal((content().match(/<button class="item /g)||[]).length,1);
for(const [version,count,run,otherRun] of [['stats-before',0,'statistics-before-run','statistics-after-run'],['stats-after',7,'statistics-after-run','statistics-before-run']]){
  navigate(exactHref('9@'+version));
  assert.equal(selection().selectedMemory,'9@'+version);
  assert.equal(selection().memoryIdFilter,'9');
  assert.equal((content().match(/<button class="item /g)||[]).length,1,'an exact statistics snapshot remains one row');
  assert.ok(content().includes('&quot;retrieval_count&quot;: '+count),'exact URL shows the requested raw retrieval statistics');
  assert.ok(content().includes('data-run="'+run+'"'),'exact historical retrieval associations stay attached');
  assert.ok(!content().includes('data-run="'+otherRun+'"'),'display grouping cannot borrow another snapshot\'s historical associations');
  assert.doesNotMatch(content(),/<h3>不同的记忆版本<\/h3>/,'a single business version needs no version-switching section');
  assert.doesNotMatch(content(),/不使用最新版本替代历史运行实际读取的旧版本/,'single-version explanatory text is hidden with the section');
}
assert.equal(evaluate('bundle.memories.length'),3,'the raw exported snapshots remain intact');
assert.equal(evaluate('memory("9@stats-before").raw.retrieval_count'),0);
assert.equal(evaluate('memory("9@stats-after").raw.retrieval_count'),7);
location.hash='';evaluate('load(fixture)');
evaluate(`
  const linkedRun=(id,reference,text)=>({id,tool:'memory_recall',status:'completed',spans:[],retrieval:{selected_refs:[reference]},mcp_response:{recorded:true,returned_refs:[reference],body:{text}},input:{mcp_text:text},host:{input_observability:'unavailable'}});
  const runFixture={...fixture,runs:[linkedRun('reverse-target','1@old','synthetic target request'),linkedRun('reverse-other','1@new','synthetic body quotes reverse-target; this is a different request')]};
  load(runFixture);
`);
const reverse=evaluate(`({detail:memoryDetail(fixtureOld),link:linkRun('reverse-target')})`);
link(reverse.detail,runHref('reverse-target'));link(reverse.link,runHref('reverse-target'));
assert.match(reverse.link,/<a\b[^>]*data-run="reverse-target"/);
assert.match(reverse.link,/data-run-link="true"/);
click({run:'reverse-target',runLink:'true'});
assert.deepEqual(runSelection(),{view:'runs',selectedRun:'reverse-target',runIdFilter:'reverse-target'});
assert.equal(runRows(),1,'reverse association opens only the target request');
assert.doesNotMatch(content(),/<button\b[^>]*data-run="reverse-other"/,'a request mentioning the target ID is not the target request');
click({run:'reverse-target'});
assert.equal(runSelection().runIdFilter,'reverse-target','ordinary list selection preserves the current run filter');
assert.equal(runRows(),1);
nodes.get('search').handlers.input({target:{value:'different request'}});
assert.equal(runSelection().runIdFilter,null,'new search text clears a request-link filter');
assert.doesNotMatch(location.hash,/^#run=/,'new search text clears the exact request URL');
assert.equal(runRows(),1);
assert.equal(runSelection().selectedRun,'reverse-other','search can find a different request outside the previously linked scope');
assert.match(content(),/<button\b[^>]*data-run="reverse-other"/);
click({run:'reverse-target',runLink:'true'});
assert.equal(runSelection().runIdFilter,'reverse-target','opening a detail link without a new search still establishes its scope');
click({view:'runs'});assert.equal(runSelection().runIdFilter,null);assert.equal(runRows(),2);
assert.doesNotMatch(location.hash,/^#run=/,'the sidebar returns to the unfiltered request list');
location.hash=runHref('reverse-target');evaluate('navigateDetailHash()');
assert.equal(runSelection().selectedRun,'reverse-target');assert.equal(runRows(),1);
location.hash=runHref('reverse-other');evaluate('load(runFixture)');
assert.equal(runSelection().runIdFilter,'reverse-other');assert.equal(runRows(),1,'initial direct URL load honors the requested run');
for(const modifiers of [{metaKey:true},{ctrlKey:true},{shiftKey:true},{altKey:true},{button:1}]){
  assert.equal(click({run:'reverse-target',runLink:'true'},modifiers),false);
  assert.equal(location.hash,runHref('reverse-other'),'modified reverse links retain native browser behavior');
}
location.hash=runHref('reverse-target');windowHandlers.hashchange();
assert.equal(runSelection().runIdFilter,'reverse-target');assert.equal(runRows(),1,'hashchange dispatches request URLs');
location.hash=runHref('missing-run');evaluate('navigateRunHash()');
assert.equal(runSelection().runIdFilter,'missing-run');assert.equal(runRows(),0);
assert.doesNotMatch(content(),/<h2>memory_recall<\/h2>/,'missing run never borrows another request detail');
assert.match(content(),/未.*导出|不存在|没有匹配|缺失/);
click({view:'runs'});assert.equal(runRows(),2);
navigate(exactHref('1@old'));
assert.equal(selection().selectedMemory,'1@old');assert.equal(selection().memoryIdFilter,'1');
assert.match(content(),/<div class="memory-body">synthetic old<\/div>/,'run routes preserve existing memory navigation');
for(const route of [idHref(1),exactHref('1@old')]){
  navigate(route);
  assert.equal(selection().memoryIdFilter,'1');
  nodes.get('search').handlers.input({target:{value:'synthetic single'}});
  assert.equal(selection().memoryIdFilter,null,'new search text clears both bare-ID and exact-version memory scopes');
  assert.doesNotMatch(location.hash,/^#memory(?:-id)?=/,'new search text clears the memory URL');
  assert.equal(selection().selectedMemory,'2@only','search finds another memory outside the previous ID scope');
  assert.match(content(),/<div class="memory-body">synthetic single<\/div>/);
  assert.doesNotMatch(content(),/<button\b[^>]*data-memory="1@/);
}
location.hash='';evaluate('load(fixture)');
assert.equal(network,0);
evaluate(`
  const listMemory=(id,text,type)=>({...fixtureMemory(id,'list',text),memory_type:type});
  const listCall=(id,text,status)=>({id,run_id:'list-run-a',stage:text,status,usage:{},cost:{amount:null,currency:null,source:'unknown'}});
  const listFixture={...fixture,
    runs:[linkedRun('list-run-a','31@list','match-scope request A'),linkedRun('list-run-b','32@list','match-scope request B'),linkedRun('list-run-excluded','33@list','unrelated request'),{...linkedRun('list-run-filtered','34@list','match-scope wrong status'),status:'failed'}],
    memories:[listMemory(31,'match-scope memory A','Fact'),listMemory(32,'match-scope memory B','Fact'),listMemory(33,'unrelated memory','Fact'),listMemory(34,'match-scope wrong type','Preference')],
    llm_calls:[listCall('list-call-a','match-scope call A','failed'),listCall('list-call-b','match-scope call B','incomplete'),listCall('list-call-excluded','unrelated call','failed'),listCall('list-call-filtered','match-scope wrong status','completed')]};
`);
const listCases=[
  {view:'runs',key:'run',first:'list-run-a',second:'list-run-b',filter:'completed',selected:'selectedRun'},
  {view:'memories',key:'memory',first:'31@list',second:'32@list',type:'Fact',selected:'selectedMemory'},
  {view:'calls',key:'call',first:'list-call-a',second:'list-call-b',filter:'problem',selected:'selectedCall'}
];
const listState = spec => ({...JSON.parse(evaluate('JSON.stringify({view,query,filter,memFilter,runIdFilter,memoryIdFilter})')),
  search:nodes.get('search').value,status:nodes.get('status-filter').value,hash:location.hash,
  type:spec.type?nodes.get('memory-type-filter').value:null,
  rows:renderedListItems(spec.key).map(item=>item.dataset[spec.key]).sort()});
const filteredListSelection = spec => {
  location.hash='';evaluate('load(listFixture)');click({view:spec.view});
  nodes.get('search').value='match-scope';nodes.get('search').handlers.input({target:{value:'match-scope'}});
  const control=nodes.get(spec.type?'memory-type-filter':'status-filter');
  control.value=spec.type||spec.filter;control.handlers.change({target:{value:control.value}});
  assert.deepEqual(renderedListItems(spec.key).map(item=>item.dataset[spec.key]).sort(),[spec.first,spec.second].sort());
  assert.equal(evaluate(spec.selected),spec.first);
  const before=listState(spec),beforeScrolls=scrolls;
  clickListItem(spec.key,spec.second);
  assert.equal(evaluate(spec.selected),spec.second,'the requested list detail opens: '+spec.view);
  assert.deepEqual(listState(spec),before,'list click preserves search, controls, filters and visible rows: '+spec.view);
  assert.equal(scrolls,beforeScrolls,'ordinary list detail selection must not scroll: '+spec.view);
  if(spec.type)assert.match(content(),/<option value="Fact" selected>/,'the rebuilt type control preserves its selected option');
  return before;
};
const listFailures=[];
for(const spec of listCases){try{filteredListSelection(spec);}catch(error){listFailures.push(spec.view+': '+error.message);}}
assert.equal(listFailures.length,0,listFailures.join('\n'));
location.hash='';evaluate('load(fixture)');
// Saved originals exclude query inputs and snippets; links identify full records exactly.
evaluate(`load({...fixture,sources:[
 {id:'source/一',kind:'persisted_source',text:'原文<&>'.repeat(100)+'全文结束',session_id:'session-one',message_id:'turn-one'},
 {id:'source-two',kind:'persisted_source',text:'第二条原文'},
 {id:'query-only',kind:'mcp_arguments',text:'只查询'},
 {id:'snippet-only',kind:'persisted_evidence',text:'只摘录'}
]});`);
click({view:'sources'});
assert.equal(nodes.get('sources-count').textContent,2);
assert.match(content(),/全文结束/);
assert.match(content(),/原文&lt;&amp;&gt;/);
assert.doesNotMatch(content(),/只查询|只摘录|展开全文/);
assert.match(evaluate("renderLexicalDetail({candidates:[{source_id:'source/一',text:'片段'}]})"),/href="#source=source%2F%E4%B8%80"/);
nodes.get('search').handlers.input({target:{value:'第二条'}});
assert.equal(evaluate('selectedSource'),'source-two');
click({source:'source/一'});
assert.equal(evaluate('selectedSource'),'source/一');
assert.equal(nodes.get('search').value,'');
assert.match(content(),/全文结束/);
for(const hash of ['#source=missing','#source=%E0%A4%A','#source=']){
 location.hash=hash;windowHandlers.hashchange();
 assert.doesNotMatch(content(),/全文结束/);
 assert.match(content(),/所链接的原文未随当前数据源导出/);
}
location.hash='#source=source-two';windowHandlers.hashchange();
assert.equal(evaluate('selectedSource'),'source-two');
assert.equal(click({source:'source/一'},{metaKey:true}),false);
assert.equal(evaluate('selectedSource'),'source-two');
evaluate('load(bundle)');
assert.equal(evaluate('selectedSource'),'source-two','loading preserves the original deep link');
location.hash='';evaluate('load(fixture)');
(async()=>{
  let simulatedReads=0;
  context.liveFixture=JSON.parse(evaluate('JSON.stringify(fixture)'));
  context.liveFixture.live={revision:'live-1',status:'connected',captured_at:'2026-09-22T00:00:00Z',errors:[],logs:[]};
  context.liveFixture.memories.reverse();
  context.fetch=async()=>{simulatedReads++;return {ok:true,json:async()=>context.liveFixture};};
  navigate(exactHref('1@old'));
  evaluate("liveMode=true;lastLiveRevision='live-0';");
  await evaluate('pollLive()');
  assert.equal(selection().selectedMemory,'1@old','live refresh cannot replace an exact old version with the first version sharing its ID');
  assert.equal(selection().memoryIdFilter,'1','live refresh retains the filtered version list');
  assert.match(content(),/<div class="memory-body">synthetic old<\/div>/);
  context.liveFixture={...context.liveFixture,live:{...context.liveFixture.live,revision:'live-2'},memories:context.liveFixture.memories.filter(memory=>memory.version!=='old')};
  await evaluate('pollLive()');
  assert.equal(selection().selectedMemory,'1@old','disappearing exact version stays missing, not replaced');
  assert.doesNotMatch(content(),/<div class="memory-body">/);
  assert.match(content(),/未随当前数据源导出/);
  context.liveFixture={...context.liveFixture,live:{...context.liveFixture.live,revision:'live-3'},memories:JSON.parse(evaluate('JSON.stringify(fixture.memories)'))};
  location.hash=exactHref('1@old');evaluate('lastLiveRevision=null;selectedMemory=null;');
  await evaluate('pollLive()');
  assert.equal(selection().selectedMemory,'1@old','first live read honors a deep link before data was loaded');
  context.liveFixture=JSON.parse(evaluate('JSON.stringify(runFixture)'));
  context.liveFixture.live={revision:'run-live-1',status:'connected',captured_at:'2026-09-22T00:00:00Z',errors:[],logs:[]};
  location.hash=runHref('reverse-target');evaluate('lastLiveRevision=null;selectedRun=null;');
  await evaluate('pollLive()');
  assert.equal(runSelection().runIdFilter,'reverse-target');assert.equal(runRows(),1,'first live read honors a run URL');
  context.liveFixture={...context.liveFixture,live:{...context.liveFixture.live,revision:'run-live-2'},runs:[...context.liveFixture.runs].reverse()};
  await evaluate('pollLive()');
  assert.equal(runSelection().selectedRun,'reverse-target');assert.equal(runRows(),1,'refresh preserves the exact run filter');
  context.liveFixture={...context.liveFixture,live:{...context.liveFixture.live,revision:'run-live-3'},runs:context.liveFixture.runs.filter(run=>run.id!=='reverse-target')};
  await evaluate('pollLive()');
  assert.equal(runSelection().runIdFilter,'reverse-target');assert.equal(runRows(),0,'a removed run stays missing despite its ID occurring in another body');
  assert.doesNotMatch(content(),/<h2>memory_recall<\/h2>/);
  click({view:'runs'});assert.equal(runSelection().runIdFilter,null);assert.equal(runRows(),1,'sidebar restores all remaining requests');
  for(const spec of listCases){
    const before=filteredListSelection(spec);
    context.liveFixture=JSON.parse(evaluate('JSON.stringify(listFixture)'));
    context.liveFixture.live={revision:'filtered-list-'+spec.view,status:'connected',captured_at:'2026-09-22T00:00:00Z',errors:[],logs:[]};
    evaluate("lastLiveRevision='filtered-list-previous';");
    await evaluate('pollLive()');
    assert.deepEqual(listState(spec),before,'automatic refresh preserves the selected search result list: '+spec.view);
    assert.equal(evaluate(spec.selected),spec.second,'automatic refresh preserves the opened matching detail: '+spec.view);
  }
  context.liveFixture=JSON.parse(evaluate('JSON.stringify(fixture)'));
  context.liveFixture.live={revision:'logs-before',status:'connected',captured_at:'2026-09-22T00:00:00Z',errors:[],logs:[{name:'synthetic.log',path:'/synthetic/log',status:'available',lines:['match-scope log A','unrelated log line','match-scope log B']}]};
  evaluate('load(liveFixture)');click({view:'logs'});
  nodes.get('search').value='match-scope';nodes.get('search').handlers.input({target:{value:'match-scope'}});
  assert.match(content(),/match-scope log A/);assert.match(content(),/match-scope log B/);
  assert.doesNotMatch(content(),/unrelated log line/);
  assert.equal((content().match(/<button\b/g)||[]).length,0,'logs has no selectable detail rows');
  context.liveFixture={...context.liveFixture,live:{...context.liveFixture.live,revision:'logs-after',logs:context.liveFixture.live.logs.map(log=>({...log,lines:[...log.lines,'match-scope log C','unrelated fresh line']}))}};
  evaluate("lastLiveRevision='logs-before';");
  await evaluate('pollLive()');
  assert.equal(evaluate('view'),'logs');assert.equal(evaluate('query'),'match-scope');
  assert.equal(nodes.get('search').value,'match-scope');
  assert.match(content(),/match-scope log C/);assert.doesNotMatch(content(),/unrelated (?:log|fresh) line/);
  evaluate('liveMode=false;');
  assert.equal(simulatedReads,10);assert.equal(network,0);
  const factIndex=evaluate(`renderFactIndex({status:'unavailable',renderer_version:'fact-v1',reason:'<stale>',coverage:{exact:0}})`);
assert.match(factIndex,/不可用，使用原文索引/);
assert.match(factIndex,/&lt;stale&gt;/);
assert.doesNotMatch(factIndex,/<stale>/);
assert.match(factIndex,/返回的证据仍是原文/);
assert.equal(evaluate('renderFactIndex(null)'),'');
console.log(JSON.stringify({status:'passed',network_requests:network,simulated_live_reads:simulatedReads,checks:['candidate references and screenshot fields','transaction exact snapshots','HTML and URL escaping','missing/owner/version boundaries','bare ID version selection','click and hash navigation','initial URL and native modified links','statistics-only display grouping with consistent counts','exact raw snapshots and historical associations preserved','live refresh preserves exact version','reverse run links, direct URLs, exact filtering and refresh','user input clears detail scopes for request and memory search','rendered request/memory/call list selection preserves search, filters, URL and scroll','list and log searches survive automatic refresh','fact index fallback status and escaping'],scope:'shipped script in Node VM; no browser automation'}));
})().catch(error=>{console.error(error);process.exitCode=1;});
