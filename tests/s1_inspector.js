// Offline rendering/import checks against the shipped script; this is not a browser test.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const html = fs.readFileSync('inspector/web/index.html', 'utf8');
const nodes = new Map(), handlers = {};
const document = {
  getElementById(id) {
    if (!nodes.has(id)) nodes.set(id, {style:{}, handlers:{}, addEventListener(k,f){this.handlers[k]=f;}});
    return nodes.get(id);
  },
  querySelectorAll: () => [],
  addEventListener: (name, fn) => { handlers[name] = fn; }
};
let network = 0, pendingHelpClose;
const context = vm.createContext({document, window:{scrollTo(){},addEventListener(){},innerWidth:400,innerHeight:300}, location:{protocol:'file:'},
  clearTimeout(){pendingHelpClose=null;}, setTimeout(fn){pendingHelpClose=fn;return 1;},
  fetch(){network++; throw Error('network forbidden');}});
vm.runInContext(html.split('<script>')[1].split('</script>')[0], context);
const click = dataset => handlers.click({preventDefault(){},target:{closest:()=>({dataset})}});
const content = () => nodes.get('content').innerHTML;
(async () => {
  for (const file of ['inspector/web/recorded-4096-failure.json','docs/refactor/s1/runtime-fixture.json','docs/refactor/s1/deployed/runtime-fixture.json']) {
    const text = fs.readFileSync(file,'utf8'), bundle = JSON.parse(text);
    await nodes.get('file-input').handlers.change({target:{files:[{size:Buffer.byteLength(text),text:async()=>text}]}});
    assert.equal(nodes.get('message').style.display,'none');
    assert.match(nodes.get('banner').innerHTML,/导入记录/);
    const review = bundle.llm_calls.find(c=>c.status==='incomplete');
    click({call:review.id});
    assert.match(content(),/4,096/);
    assert.match(content(),/max_output_tokens/);
    click({run:bundle.runs[1].id});
    assert.match(content(),/完成 · 空结果/);
    nodes.get('search').handlers.input({target:{value:'no-such-fixture-948571'}});
    assert.match(content(),/没有匹配的运行记录/);
    click({view:'memories'});
    click({view:'calls'});
    assert.match(content(),/费用未知/);
    click({run:bundle.runs[0].id});
    if (bundle.meta.channels) assert.match(content(),/该版本未启用/);
    if (file.includes('/deployed/')) {
      click({run:bundle.runs[2].id});
      assert.match(content(),/执行候选准入与写入决策/);
      assert.match(content(),/读取执行前快照/);
      assert.equal((content().match(/aria-label="[^"]+：步骤说明"/g)||[]).length,bundle.runs[2].spans.length);
      assert.match(content(),/排序未记录/);
      assert.doesNotMatch(content(),/未入选/);
    }
  }
  vm.runInContext("bundle.runs[0].evidence_status='evidence_incomplete';selectedRun=bundle.runs[0].id;render();",context);
  assert.match(content(),/诊断证据不完整/);
  const sourceReceipt=vm.runInContext(`(()=>{
    const run={...bundle.runs[0],source:{status:'stored',source_id:'source-one',provenance:'mcp_received',message_id_trust:'client_supplied',received_sequence:2},index:{status:'not_requested'},enrichment:{status:'review_incomplete',error:'<source-failure>'}};
    const span={id:'receipt',name:'source_receipt',status:'completed',output:{source:run.source,index:run.index,enrichment:run.enrichment}};
    return {html:runDetail({...run,spans:[span]}),extraction:runDetail({...run,enrichment:{status:'extraction_incomplete'}}),legacy:runDetail(bundle.runs[0]),info:stageInfo(span),section:executionSections({...run,spans:[span]})[0].key};
  })()`,context);
  assert.match(sourceReceipt.html,/原文接收：.*已留存/);
  assert.match(sourceReceipt.html,/索引：.*未请求/);
  assert.match(sourceReceipt.html,/结构化整理：.*复核未完成/);
  assert.match(sourceReceipt.html,/接收序号：2/);
  assert.match(sourceReceipt.extraction,/结构化整理：.*抽取未完成/);
  assert.match(sourceReceipt.html,/&lt;source-failure&gt;/);
  assert.doesNotMatch(sourceReceipt.html,/<source-failure>/);
  assert.match(sourceReceipt.legacy,/原文接收：.*未记录/,'records without source receipts must keep the status row visible');
  assert.match(sourceReceipt.legacy,/索引：.*未记录/);
  assert.match(sourceReceipt.legacy,/结构化整理：.*未记录/);
  assert.match(sourceReceipt.legacy,/此记录未提供独立原文回执/);
  assert.doesNotMatch(sourceReceipt.legacy,/原文接收：<span[^>]*>已留存<\/span>/,'missing receipts must not imply successful capture');
  assert.match(sourceReceipt.info.description,/模型失败不撤销/);
  assert.equal(sourceReceipt.section,'source');
  const sourceTransactions=vm.runInContext(`(()=>{
    const record={id:'saved-source',request:{text:'<原文>保留正文'},enrichment:'pending',index:{status:'ready',source_revision:1}};
    const snapshot=sources=>({sources,repository:{memories:{}}});
    const span=(before,after,status='completed')=>({name:'transaction_committed',status,recorded_at:'2026-09-23T04:09:07.592575Z',output:{before:snapshot(before),after:snapshot(after)}});
    const saved=span({}, {key:record}),failed=span({key:record},{key:{...record,enrichment:'failed',error:'model_network_disabled',index:{status:'ready',source_revision:2}}}),deleted=span({key:record},{key:{...record,request:null}});
    const baseline=JSON.stringify(saved);
    const resultOnly=span({key:record},{key:{...record,enrichment:'failed',error:'model_network_disabled'}});
    const captureWithIndex=span({old:record},{old:{...record,index:{...record.index,source_revision:2}},key:{...record,id:'new-source'}});
    return {compact:renderTransactionDetail(resultOnly),compactName:stageInfo(resultOnly).name,compactSummary:stageResult(resultOnly),captureName:stageInfo(captureWithIndex).name,saved:renderTransactionDetail(saved),summary:stageResult(saved),name:stageInfo(saved).name,failed:renderTransactionDetail(failed),failedName:stageInfo(failed).name,deleted:renderTransactionDetail(deleted),pending:renderTransactionDetail({...saved,status:'in_progress'}),missing:renderSourceTransaction({status:'completed',output:{}}),unchanged:JSON.stringify(saved)===baseline,
      header:runDetail({...bundle.runs[0],source:{status:'stored'},save_status:'not_committed'}),group:executionSections({...bundle.runs[0],spans:[saved,{name:'source_receipt',status:'completed'},failed]})};
  })()`,context);
  assert.match(sourceTransactions.saved,/新增原文 1 条/);
  assert.match(sourceTransactions.saved,/0 → 1 条/);
  assert.match(sourceTransactions.saved,/12:09:07.592（北京时间）/);
  assert.match(sourceTransactions.saved,/&lt;原文&gt;保留正文/);
  assert.match(sourceTransactions.saved,/href="#source=saved-source"/);
  assert.doesNotMatch(sourceTransactions.saved,/<原文>|其他状态已提交/);
  assert.match(sourceTransactions.summary,/新增原文 1 条/);
  assert.match(sourceTransactions.name,/保存原文/);
  assert.equal(sourceTransactions.failedName,'提交原文变更');
  assert.equal(sourceTransactions.name,'保存原文并建立索引');
  assert.equal(sourceTransactions.captureName,'保存原文并建立索引');
  assert.equal(sourceTransactions.compactName,'记录结构化整理状态');
  assert.equal(sourceTransactions.compactSummary,'已记录整理状态');
  for(const value of ['关联原文','记录时间','待处理 → 失败','后台模型网络被禁用','href="#source=saved-source"'])assert.ok(sourceTransactions.compact.includes(value),value);
  assert.doesNotMatch(sourceTransactions.compact,/保留正文|原文总数|索引状态|正文变化|结构化记忆提交变更/);
  assert.match(sourceTransactions.failed,/1 → 1 条/);
  assert.match(sourceTransactions.failed,/索引关联原文修订<\/label><div>1 → 2/);
  assert.match(sourceTransactions.failed,/正文未变|model_network_disabled/);
  assert.match(sourceTransactions.failed,/→ 失败/);
  assert.doesNotMatch(sourceTransactions.deleted,/保留正文/);
  assert.match(sourceTransactions.deleted,/1 → 0 条/);
  assert.doesNotMatch(sourceTransactions.pending,/首次保存|新增原文 1 条/);
  assert.match(sourceTransactions.missing,/无法判断原文变化/);
  assert.match(sourceTransactions.header,/原文保存状态<\/label><div>已留存/);
  assert.match(sourceTransactions.header,/结构化记忆保存状态<\/label><div>未提交/);
  assert.equal(sourceTransactions.group.length,1);
  assert.equal(sourceTransactions.group[0].key,'source');
  assert.equal(sourceTransactions.unchanged,true);
  const deferred=vm.runInContext(`(()=>{
    const record={id:'s5-source',request:{text:'记住中文回答'},index:{status:'ready'},enrichment:'queued',task:{attempt_count:0,accepted_mode:'deferred'}};
    const next={...record,enrichment:'running',task:{attempt_count:1,accepted_mode:'deferred'}};
    const span={id:'claim',name:'transaction_committed',status:'completed',output:{before:{sources:{key:record},repository:{memories:{}}},after:{sources:{key:next},repository:{memories:{}}}}};
    const capture={...bundle.runs[0],id:'capture',tool:'memory_remember',source:{status:'stored',source_id:'s5-source'},index:{status:'ready'},enrichment:{status:'queued'},save_status:'queued',enrichment_task:record.task,spans:[]};
    const process={...capture,id:'process',tool:'source_enrichment',entrypoint:'internal_cli',enrichment:{status:'committed'},save_status:'committed',enrichment_task:next.task,input:{mcp_text:null,arguments:{source_id:'s5-source'}}};
    bundle.runs.push(capture,process);
    const result={kind:sourceTransactionKind(span),transaction:renderTransactionDetail(span),capture:runDetail(capture),process:runDetail(process),stale:state('stale_requires_review')};
    bundle.runs.pop();bundle.runs.pop();
    return result;
  })()`,context);
  assert.equal(deferred.kind,'enrichment');
  assert.match(deferred.transaction,/已排队 → 整理中/);
  assert.match(deferred.transaction,/整理尝试次数<\/label><div>1/);
  assert.doesNotMatch(deferred.transaction,/原文提交变更|保留正文/);
  assert.match(deferred.capture,/结构化记忆保存状态<\/label><div>已排队/);
  assert.match(deferred.capture,/同一原文的其他记录.*#run=process/);
  assert.match(deferred.process,/内部原文整理参数/);
  assert.match(deferred.process,/整理尝试次数<\/label><div>1/);
  assert.match(deferred.process,/内部 CLI 操作不构造 MCP 响应/);
  assert.doesNotMatch(deferred.process,/服务端记录的 MCP 响应/);
  assert.match(deferred.stale,/旧结果待复核/);
  const lexical=vm.runInContext(`(()=>{
    const result={channel:'lexical_raw',query:'合成项目 SQLite',status:'completed',processing_version:'fixture-v1',generation:2,source_revision:3,candidate_count:1,duration_ms:2,coverage_complete:true,candidates:[{source_id:'fixture-source',segment_id:'fixture-segment',segment_version:'fixture-version',byte_start:0,byte_end:6,text:'<img src=x onerror=alert(1)>',session_id:'fixture-session',turn_id:'fixture-turn',enrichment:'review_incomplete',mapping_uncertain:true,known_memories:[{status:'superseded',superseded_by:2}]}]};
    const span={id:'fixture-lexical',name:'lexical_raw',status:'completed',output:result};
    const before=JSON.stringify(result);
    return {html:stageBody(span),empty:renderLexicalDetail({...result,candidates:[],candidate_count:0}),incomplete:renderLexicalDetail({...result,status:'incomplete',coverage_complete:false,candidates:[],reason:'pending_index'}),missing:renderLexicalDetail({}),summary:stageResult(span),section:executionSections({...bundle.runs[0],spans:[span]})[0].key,unchanged:JSON.stringify(result)===before};
  })()`,context);
  for(const text of ['lexical_raw','fixture-v1','fixture-segment','fixture-version','UTF-8 字节范围','对应关系不确定','已知事实状态与纠正关系','superseded','内部只读检索记录'])assert.ok(lexical.html.includes(text),text);
  assert.match(lexical.html,/&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.doesNotMatch(lexical.html,/<img|data-memory=|已记录分数/);
  assert.match(lexical.empty,/已记录 0 条原文候选/);
  assert.match(lexical.incomplete,/空候选不能解释为没有相关原文/);
  assert.match(lexical.incomplete,/pending_index/);
  assert.match(lexical.missing,/未采集原文候选明细/);
  assert.match(lexical.summary,/原文候选 1 条/);
  assert.equal(lexical.section,'recall');
  assert.equal(lexical.unchanged,true);
  const method=vm.runInContext(`(()=>{
    const data={processing_version:'cjk-position-v1',alias_policy:'natural-v1',query_phrases:['c9752 c6986','w000037000033000031000039','w00003c00007300006300007200006900007000007400003e'],match_mode:'any_phrase',match_expression:'<fts>',owner:'<owner>',task_id:null,generation:1,limit:30,backend_generation_calls:0,sort_basis:['received_sequence_desc','source_id','byte_start'],coverage_complete:true,candidates:[{text:'青榆7319',matched_query_phrases:['c9752 c6986']}]};
    const ranked={...data,alias_policy:'natural-v2',ranking_policy:'literal-hints-v1',primary_query:'<当前问题>',query_entity_ids:['<当前实体>'],query_context_entity_ids:['<当前实体>','上文实体'],sort_basis:['query_entity_matches_desc','query_phrase_matches_desc','query_context_entity_matches_desc',...data.sort_basis],candidates:[{...data.candidates[0],ranking_signals:{query_entity_matches:1,query_phrase_matches:3,query_context_entity_matches:2}}]};
    const before=JSON.stringify(ranked);
    return {html:renderLexicalDetail(data),ranked:renderLexicalDetail(ranked),unchanged:JSON.stringify(ranked)===before,legacy:renderLexicalDetail({...data,candidates:[{text:'青榆7319'}],match_expression:undefined}),unknown:renderLexicalDetail({query_phrases:['c9752 c6986']}),invalid:lexicalPhrase('cffffff','cjk-position-v1'),other:lexicalPhrase('c9752 c6986','unknown')};
  })()`,context);
  for(const text of ['如何检索','中文相邻双字','青榆','7319','词项组之间 OR','原文接收序号降序','候选上限','本片段命中的词项','实际 FTS MATCH 表达式'])assert.ok(method.html.includes(text),text);
  assert.match(method.html,/&lt;script&gt;/);
  assert.match(method.html,/&lt;fts&gt;/);
  assert.match(method.html,/&lt;owner&gt;/);
  assert.doesNotMatch(method.html,/<script>|<fts>|<owner>/);
  assert.match(method.legacy,/旧记录不推测命中词项/);
  assert.match(method.unknown,/未采集可解释的查询策略/);
  assert.doesNotMatch(method.unknown,/SQLite FTS5|中文相邻双字/);
  assert.equal(method.invalid,'cffffff');
  assert.equal(method.other,'c9752 c6986');
  for(const text of ['natural-v2','中文相邻双字','名称按索引词项边界匹配','literal-hints-v1','先在授权的 owner／任务范围内排序，再按候选上限截取','名称仅影响排序','不证明语义相关','当前问题的登记实体命中数降序','当前问题的词项组命中数降序','查询及上文的登记实体命中数降序','&lt;当前问题&gt;','&lt;当前实体&gt;'])assert.ok(method.ranked.includes(text),text);
  assert.match(method.ranked,/<details class="raw retrieval-section"><summary>如何检索/);
  assert.match(method.ranked,/当前问题的词项组命中数<\/label><div>3<\/div>/);
  assert.doesNotMatch(method.ranked,/<当前问题>|<当前实体>|置信度/);
  assert.doesNotMatch(method.legacy,/literal-hints-v1|名称仅影响排序|当前问题的词项组命中数|名称按索引词项边界匹配/);
  assert.equal(method.unchanged,true);
  const localSummary=vm.runInContext(`(()=>{
    const data={recall_status:'completed',result:{mandatory:[],constraints:[],preferences:[],facts:[],episodes:[]},source_evidence:[{}],historical_or_conflicting:[],backend_generation_calls_in_recall:0,read_revision:2,evidence_status:'sufficiency_not_verified',degraded_reasons:['structured_enrichment_incomplete'],budget:{config:{candidate_k:30,source_limit:10,content_bytes:24000},content_bytes:763,coverage_complete:true,excluded_count:0,excluded:[]}};
    const before=JSON.stringify(data);
    const reference={status:'unconfirmed',semantic_resolution:'not_performed',query_entity_ids:['<当前实体>','第二实体'],context_entity_ids:[],task_scope:'<任务>'};
    return {html:renderLocalRecall(data),unconfirmed:renderLocalRecall({...data,reference_resolution:reference}),missing:renderLocalRecall({}),limited:renderLocalRecall({...data,reference_resolution:{...reference,status:'ambiguous'},evidence_status:'ambiguous',degraded_reasons:['candidate_quota_reached','<future>'],budget:{coverage_complete:false}}),unchanged:JSON.stringify(data)===before};
  })()`,context);
  for(const text of ['本地召回结果','0 条','1 个片段','0 次','尚未验证这些证据能否完整回答问题','部分原文尚未完成结构化整理','763 字节 / 24,000 字节','未因容量或数量限制裁剪内容','状态原始记录','容量原始记录'])assert.ok(localSummary.html.includes(text),text);
  assert.doesNotMatch(localSummary.html.split('<details')[0],/sufficiency_not_verified|structured_enrichment_incomplete|content_bytes/);
  assert.match(localSummary.missing,/未采集/);
  assert.doesNotMatch(localSummary.missing,/0 条|0 次|未因容量或数量限制裁剪内容/);
  assert.match(localSummary.limited,/问题指代不明确/);
  assert.match(localSummary.limited,/部分内容因容量或数量限制未返回/);
  assert.match(localSummary.limited,/&lt;future&gt;/);
  assert.doesNotMatch(localSummary.limited,/<future>/);
  for(const text of ['对象尚未确认（仅做字面匹配）','语义／指代判断</label><div>未执行','&lt;当前实体&gt; ｜ 第二实体','未匹配到登记实体（不等于没有对象）','不自动要求追问','任务范围也不代表指代已确认','&lt;任务&gt;'])assert.ok(localSummary.unconfirmed.includes(text),text);
  assert.doesNotMatch(localSummary.unconfirmed,/<当前实体>|<任务>|已检测到有限规则歧义|需要结合上下文确认/);
  assert.match(localSummary.limited,/已检测到有限规则歧义/);
  assert.match(localSummary.html,/对象识别状态<\/label><div><span class="muted">未采集/);
  assert.doesNotMatch(localSummary.html,/对象尚未确认|语义／指代判断<\/label><div>未执行/);
  assert.equal(localSummary.unchanged,true);
  const previousBundle=vm.runInContext('JSON.stringify(bundle)',context);
  const unobservedSource=JSON.parse(previousBundle);
  unobservedSource.meta.channels.source_capture='unknown';
  const unobservedText=JSON.stringify(unobservedSource);
  await nodes.get('file-input').handlers.change({target:{files:[{size:Buffer.byteLength(unobservedText),text:async()=>unobservedText}]}});
  assert.equal(nodes.get('message').style.display,'none','unobserved live sources must pass actual import schema validation');
  assert.match(content(),/独立原文留存：尚未观测到/);
  assert.doesNotMatch(content(),/独立原文留存：该版本未启用/,'missing live source receipts do not prove the feature is disabled');
  await nodes.get('file-input').handlers.change({target:{files:[{size:Buffer.byteLength(previousBundle),text:async()=>previousBundle}]}});
  const longSources=vm.runInContext(`(()=>{
    const original=bundle,text='原文🙂<&>'.repeat(60)+'全文结尾';
    try{
      bundle={...original,sources:[{id:'long-source',kind:'host_capture',text}]};
      const run={...original.runs[0],source_ids:['long-source'],input:{mcp_text:text},
        write_changes:[{source_ids:['long-source'],commit_status:'not_committed'}]};
      return {run:runDetail(run),memory:memoryDetail({...original.memories[0],source_ids:['long-source']}),
        candidate:renderCandidateDetail({input:{evidence:[{quote:text}]}}),full:esc(text),
        short:sourceText('短原文'),missing:sourceText(null),
        boundary:sourceText('🙂'.repeat(200)),long:sourceText('🙂'.repeat(201)),
        fourLines:sourceText('一\\r\\n二\\r\\n三\\r\\n四'),fiveLines:sourceText('一\\n二\\n三\\n四\\n五')};
    }finally{bundle=original;}
  })()`,context);
  for(const [markup,count] of [[longSources.run,3],[longSources.memory,1],[longSources.candidate,1]]){
    assert.equal((markup.match(/<details class="source-text">/g)||[]).length,count,'long source text is collapsed in every source view');
    assert.ok(markup.includes('<div class="text-box">'+longSources.full),'full source text remains available and escaped');
    assert.match(markup,/展开全文/);assert.match(markup,/收起全文/);
  }
  for(const markup of [longSources.short,longSources.missing,longSources.boundary,longSources.fourLines])assert.doesNotMatch(markup,/<details/);
  assert.match(longSources.missing,/未采集/);
  assert.match(longSources.long,/201 字符/);
  assert.ok(longSources.long.includes('🙂'.repeat(200)+'…'),'preview must not split Unicode code points');
  assert.match(longSources.fiveLines,/class="source-text"/);
  assert.equal(vm.runInContext("[DEMO_FAILURE,DEMO_VERSIONS,bundle].flatMap(b=>b.runs.flatMap(r=>r.spans)).every(s=>!stageInfo(s).description.startsWith('此阶段由导入记录定义'))",context),true);
  assert.match(vm.runInContext("stageInfo({name:'run_end'}).description",context),/总耗时/);
  const grouping=vm.runInContext(`(()=>{
    const candidate=bundle.runs[2].spans.find(s=>s.name==='候选规则裁决（提交结果见事务）');
    const a={...candidate,id:'candidate-a',duration_ms:2};
    const b={...candidate,id:'candidate-b',duration_ms:3,status:'failed',error:{reason:'fixture rejection'}};
    const commit={id:'commit',name:'transaction_committed',status:'completed'};
    const spans=[a,b,commit,a],before=JSON.stringify(spans),groups=stageGroups(spans);
    return {sizes:groups.map(g=>g.length),html:renderStageGroup(groups[0],7),
      unknown:renderStageGroup([a,{...b,duration_ms:null}],7),unchanged:JSON.stringify(spans)===before,
      order:groups.flat().map(s=>s.id)};
  })()`,context);
  assert.equal(JSON.stringify(grouping.sizes),'[2,1,1]','do not merge across a transaction');
  assert.equal(JSON.stringify(grouping.order),'["candidate-a","candidate-b","commit","candidate-a"]');
  assert.equal(grouping.unchanged,true,'display grouping must preserve exported evidence');
  assert.match(grouping.html,/候选准入与写入决策 · 2 条候选/);
  assert.match(grouping.html,/累计 5 ms/);
  assert.match(grouping.html,/候选 1/);assert.match(grouping.html,/候选 2/);
  assert.match(grouping.html,/决策结果与原因/);assert.match(grouping.html,/fixture rejection/);
  assert.match(grouping.html,/class="stage" open/,'failed candidates must open the group');
  assert.match(grouping.unknown,/累计 耗时未采集/);
  const phases=vm.runInContext(`(()=>{
    const base=bundle.runs[2],before={repository:{memories:{}},receipts:{}};
    const spans=[
      {id:'entry',name:'run_start',status:'completed'},
      {id:'extract',name:'模型抽取候选 · 第 1 次发送',status:'completed',output:{parsed_result:{candidates:[],omission_reason:'no_personal_assertion'}}},
      {id:'save-notice',name:'rpc_response',status:'in_progress'},
      {id:'review',name:'extraction_review / Rust 校验',status:'completed',output:{stage:'extraction_review',parsed_result:[]}},
      {id:'save',name:'transaction_committed',status:'completed',output:{before,after:{receipts:{},repository:{memories:{}}}}},
      {id:'query',name:'模型规划查询 · 第 1 次发送',status:'completed'},
      {id:'notice',name:'rpc_response',status:'in_progress'},
      {id:'retrieve',name:'retrieval',status:'completed'},
      {id:'counter',name:'transaction_committed',status:'completed',output:{before,after:{repository:{memories:{}},receipts:{changed:true}}}},
      {id:'global',name:'global_context',status:'completed'},
      {id:'end',name:'run_end',status:'completed'},
      {id:'response',name:'rpc_response',status:'completed'}
    ];
    const run={...base,spans,mcp_response:{body:{structuredContent:{remember:{status:'completed',result:{outcomes:[]}}}}}};
    const sections=executionSections(run);
    return {sections:sections.map(({key,groups})=>[key,groups.flatMap(g=>g.spans.map(s=>s.id))]),
      original:spans.map(s=>s.id),ordered:sections.flatMap(s=>s.groups.flatMap(g=>g.spans.map(s=>s.id))),html:renderExecution(run),
      failed:stageResult({...spans[1],status:'incomplete'},run),
      missing:stageResult({...spans.find(s=>s.id==='save'),output:{}},run),changed:stageResult(spans.find(s=>s.id==='counter'),run),
      failedSave:stageResult(spans.find(s=>s.id==='review'),{...run,mcp_response:{body:{structuredContent:{remember:{status:'failed'}}}}})};
  })()`,context);
  assert.equal(JSON.stringify(phases.sections),JSON.stringify([['entry',['entry']],['save',['extract']],['notification',['save-notice']],['save',['review','save']],['recall',['query']],['notification',['notice']],['recall',['retrieve','counter']],['response',['global','end','response']]]));
  assert.equal(JSON.stringify(phases.ordered),JSON.stringify(phases.original),'phase frames must preserve every step in original order');
  assert.match(phases.html,/aria-label="过程通知"/);assert.match(phases.html,/aria-label="响应"/);
  assert.match(phases.html,/aria-label="新增记忆（续）"/);assert.match(phases.html,/aria-label="检索记忆（续）"/);
  assert.doesNotMatch(phases.html,/过程通知与响应/);
  assert.match(phases.html,/aria-label="新增记忆"/);assert.match(phases.html,/aria-label="检索记忆"/);
  assert.match(phases.html,/0 条候选/);assert.match(phases.html,/复核确认无需保存/);assert.match(phases.html,/无变更提交/);
  assert.equal(phases.failed,'');
  assert.equal(phases.missing,'记忆变更数量未采集');
  assert.equal(phases.changed,'记忆变更 0 条 · 其他状态已提交');
  assert.doesNotMatch(phases.failedSave,/无需保存/);
  const retrieval=vm.runInContext(`(()=>{
    const run=bundle.runs[9],span=run.spans.find(s=>s.name==='retrieval');
    const empty=bundle.runs[1].spans.find(s=>s.name==='retrieval');
    const global=run.spans.find(s=>s.name==='global_context');
    const result=span.output.result;
    const withTrace=trace=>({...span,output:{...span.output,result:{...result,trace}}});
    const conflicts=[
      {memory_id:1,decision:'Mandatory'},
      {memory_id:2,decision:{Filtered:'Predicate'}},
      {memory_id:3,decision:{Conflict:{with:4,reason:'EqualSpecificity'}}},
      {memory_id:4,decision:{Conflict:{with:3,reason:'EqualSpecificity'}}}
    ];
    const oldSelected={...result.mandatory[0],value:{Text:'selected-at-retrieval'}};
    const oldFiltered={...span.output.memories[1],value:{Text:'filtered-at-retrieval'}};
    const conflictSpan=withTrace(conflicts);
    conflictSpan.output.memories=[...span.output.memories,...[3,4].map(id=>({...oldFiltered,id,value:{Text:'conflict-memory-'+id}}))];
    const historic={...span,output:{...span.output,memories:[oldSelected,oldFiltered],result:{...result,mandatory:[oldSelected]}}};
    const originalMemories=bundle.memories;
    let historicHtml;
    try {
      bundle.memories=bundle.memories.map(m=>({...m,content:'latest-version-must-not-leak',raw:{...m.raw,value:{Text:'latest-version-must-not-leak'}}}));
      historicHtml=renderStageGroup([historic],0,run);
    } finally { bundle.memories=originalMemories; }
    const malicious={...span,output:{...span.output,result:{...result,
      mandatory:[{...oldSelected,value:{Text:'<img src=x onerror=alert(1)>'}}],
      trace:[{memory_id:2,decision:{Filtered:'<svg onload=alert(2)>'}}]}}};
    return {summary:stageResult(span,run),html:renderStageGroup([span],0,run),phase:renderExecution(run),
      emptyHtml:renderStageGroup([empty],0,run),missingHtml:renderStageGroup([{...span,output:{}}],0,run),
      legacyHtml:renderStageGroup([{...span,name:'查询已保存记忆',output:{selected_refs:[ref(bundle.memories[0])]}}],0,run),
      legacyRef:ref(bundle.memories[0]),conflictHtml:renderStageGroup([conflictSpan],0,run),historicHtml,
      missingSnapshotHtml:renderStageGroup([{...span,output:{...span.output,memories:[]}}],0,run),
      maliciousHtml:renderStageGroup([malicious],0,run),
      unknownHtml:renderStageGroup([withTrace([{memory_id:7,decision:'FutureDecision'}])],0,run),
      incompleteHtml:renderStageGroup([{...span,status:'incomplete'}],0,run),
      empty:stageResult(empty,run),
      unfinished:['failed','incomplete','in_progress'].map(status=>stageResult({...span,status},run)),
      query:stageResult(bundle.runs[2].spans.find(s=>s.name==='query / Rust 校验'),run),
      missing:stageResult({...span,output:{}},{...run,returned_refs:[{id:'global-only'}]}),
      legacyEmpty:stageResult({...span,name:'查询已保存记忆',output:{selected_refs:[],scanned_count:0}},run),
      legacyPhase:renderExecution({...run,retrieval:{...run.retrieval,query_status:'completed',selected_refs:[]}}),
      conflicts:stageResult(withTrace(conflicts),run),
      unknown:stageResult(withTrace([{memory_id:7,decision:'FutureDecision'}]),run),
      global:stageResult(global,run),globalEmpty:stageResult(bundle.runs[1].spans.find(s=>s.name==='global_context'),run),
      globalFailed:stageResult({...global,output:{result:{status:'unavailable'}}},run)};
  })()`,context);
  for(const label of ['选中 1 条记忆','检查 2 条','过滤 1 条']) {
    assert.ok(retrieval.summary.includes(label));assert.ok(retrieval.html.includes(label));
  }
  assert.match(retrieval.phase,/class="phase-result">选中 1 条记忆</);
  assert.match(retrieval.empty,/选中 0 条记忆/);
  assert.equal(JSON.stringify(retrieval.unfinished),'["","",""]','unfinished steps must not claim successful counts');
  assert.doesNotMatch(retrieval.query,/选中/,'query parsing does not select memories');
  assert.equal(retrieval.missing,'检索数量未采集','missing step output must not borrow final or global refs');
  assert.match(retrieval.legacyEmpty,/选中 0 条记忆/);
  assert.match(retrieval.legacyPhase,/class="phase-result">选中 0 条记忆</);
  assert.match(retrieval.conflicts,/冲突 2 条/);assert.match(retrieval.conflicts,/过滤 1 条/);
  assert.match(retrieval.conflicts,/检查 4 条/);
  assert.doesNotMatch(retrieval.unknown,/过滤 \d/,'unknown decisions must not be classified as filtered');
  assert.equal(retrieval.global,'读取 1 条全局要求');
  assert.equal(retrieval.globalEmpty,'读取 0 条全局要求');
  assert.equal(retrieval.globalFailed,'全局要求读取失败');
  const section=(markup,label)=>{
    const match=markup.match(new RegExp('<section[^>]*aria-label="'+label+'"[^>]*>([\\s\\S]*?)</section>'));
    assert.ok(match,'missing section: '+label);return match[1];
  };
  assert.equal((retrieval.html.match(/class="retrieval-section"/g)||[]).length,4);
  assert.equal((retrieval.html.match(/class="stage"/g)||[]).length,1,'retrieval stays one execution step');
  assert.match(section(retrieval.html,'检索条件'),/response_language/);
  assert.match(section(retrieval.html,'选中记忆'),/中文/);
  assert.match(section(retrieval.html,'过滤记忆及原因'),/固定测试属性/);
  assert.match(section(retrieval.html,'过滤记忆及原因'),/属性不匹配/);
  assert.match(section(retrieval.html,'冲突记忆'),/本阶段没有冲突记忆。/);
  assert.match(retrieval.html,/<summary>完整检索原始记录<\/summary>/);
  assert.match(retrieval.html,/&quot;trace&quot;/);
  assert.match(retrieval.unknownHtml,/部分检索决定无法识别/);
  for(const label of ['过滤记忆及原因','冲突记忆']) {
    assert.match(section(retrieval.unknownHtml,label),/未采集/);
    assert.doesNotMatch(section(retrieval.unknownHtml,label),/本阶段没有|>0 条</);
  }
  assert.match(retrieval.incompleteHtml,/本阶段尚未完成，以下仅展示已记录的明细。/);
  assert.match(section(retrieval.incompleteHtml,'选中记忆'),/中文/);
  for(const [label,empty,missing] of [
    ['选中记忆','本阶段未选中记忆。','未采集选中记忆明细。'],
    ['过滤记忆及原因','本阶段没有过滤记忆。','未采集过滤记忆明细。'],
    ['冲突记忆','本阶段没有冲突记忆。','未采集冲突记忆明细。']
  ]) {
    assert.ok(section(retrieval.emptyHtml,label).includes(empty));
    assert.ok(section(retrieval.missingHtml,label).includes(missing));
    assert.ok(!section(retrieval.missingHtml,label).includes(empty),'missing evidence is not an empty list');
  }
  assert.ok(section(retrieval.legacyHtml,'选中记忆').includes('data-memory="'+retrieval.legacyRef+'"'));
  assert.match(section(retrieval.legacyHtml,'过滤记忆及原因'),/未采集过滤记忆明细。/);
  assert.doesNotMatch(section(retrieval.conflictHtml,'过滤记忆及原因'),/conflict-memory-/);
  for(const id of [3,4]) assert.ok(section(retrieval.conflictHtml,'冲突记忆').includes('conflict-memory-'+id));
  assert.match(section(retrieval.historicHtml,'选中记忆'),/selected-at-retrieval/);
  assert.match(section(retrieval.historicHtml,'过滤记忆及原因'),/filtered-at-retrieval/);
  for(const label of ['选中记忆','过滤记忆及原因','冲突记忆']) {
    assert.doesNotMatch(section(retrieval.historicHtml,label),/latest-version-must-not-leak/);
  }
  assert.match(section(retrieval.missingSnapshotHtml,'过滤记忆及原因'),/内容未随本阶段记录/);
  assert.doesNotMatch(section(retrieval.missingSnapshotHtml,'过滤记忆及原因'),/固定测试属性/);
  assert.match(section(retrieval.maliciousHtml,'选中记忆'),/&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.match(section(retrieval.maliciousHtml,'过滤记忆及原因'),/&lt;svg onload=alert\(2\)&gt;/);
  assert.doesNotMatch(retrieval.maliciousHtml,/<img|<svg/,'imported content and reasons must be HTML escaped');
  const transactions=vm.runInContext(`(()=>{
    const run=bundle.runs[2],spans=run.spans.filter(s=>s.name==='transaction_committed');
    const template=spans[0].output.after.repository.memories['1'];
    const memory=(id,fields={})=>({...template,id,...fields});
    const before={repository:{memories:{
      101:memory(101),102:memory(102),103:memory(103),104:memory(104),106:memory(106)
    }},receipts:{}};
    const after={repository:{memories:{
      101:memory(101,{value:{Text:'changed-content'}}),
      102:memory(102,{retrieval_count:20,last_retrieved_at:123456789}),
      104:memory(104),105:memory(105),106:memory(106,{reinforcement_count:5})
    }},receipts:{}};
    const mixed={...spans[0],output:{before,after}};
    const evidenceBefore=JSON.stringify({spans,mixed});
    const render=span=>({summary:stageResult(span,run),html:renderStageGroup([span],0,run)});
    const missing=[{}, {before}, {before,after:{repository:{}}},
      ...[null,[],42,'unrecorded',{'101':null},{'101':[]},{'101':memory(999)}].flatMap(memories=>[
        {before:{repository:{memories}},after}, {before,after:{repository:{memories}}}
      ])];
    const results={save:render(spans[0]),recall:render(spans[1]),mixed:render(mixed),
      receipt:render({...mixed,output:{before,after:{...before,receipts:{'new-receipt':{status:'completed'}}}}}),
      noOp:render({...mixed,output:{before,after:JSON.parse(JSON.stringify(before))}}),
      missing:missing.map(output=>render({...mixed,output})),
      unfinished:['failed','incomplete','in_progress'].map(status=>render({...mixed,status}))};
    return {...results,unchanged:JSON.stringify({spans,mixed})===evidenceBefore};
  })()`,context);
  assert.equal(transactions.save.summary,'记忆变更 1 条 · 新增 1 条 · 修改 0 条');
  assert.equal(transactions.recall.summary,'记忆变更 0 条 · 仅召回统计更新 1 条');
  assert.equal(transactions.mixed.summary,'记忆变更 4 条 · 新增 1 条 · 修改 2 条 · 移除 1 条 · 仅召回统计更新 1 条');
  assert.equal(transactions.receipt.summary,'记忆变更 0 条 · 其他状态已提交');
  assert.equal(transactions.noOp.summary,'无变更提交 · 记忆变更 0 条');
  for(const result of transactions.missing) {
    assert.equal(result.summary,'记忆变更数量未采集');
    assert.match(section(result.html,'结构化记忆提交变更'),/记忆变更数量未采集/);
    assert.doesNotMatch(section(result.html,'结构化记忆提交变更'),/记忆记录实际变动 0 条/);
  }
  for(const result of transactions.unfinished) {
    assert.equal(result.summary,'','unfinished transactions must not claim committed counts');
    assert.match(section(result.html,'结构化记忆提交变更'),/事务尚未确认提交/);
    assert.doesNotMatch(section(result.html,'结构化记忆提交变更'),/记忆记录实际变动 \d+ 条/);
  }
  const transactionDetail=section(transactions.mixed.html,'结构化记忆提交变更');
  assert.match(transactionDetail,/记忆记录实际变动 5 条（含仅召回统计更新）。/);
  for(const [label,ids] of [['新增记忆',['105']],['修改记忆',['101','106']],['移除记忆',['103']],['仅召回统计更新',['102']]]) {
    const row=transactionDetail.match(new RegExp('<label>'+label+' · '+ids.length+' 条</label><div>(.*?)</div>'));
    assert.ok(row,'missing transaction category: '+label);
    assert.deepEqual(Array.from(row[1].matchAll(/记忆 #(\d+)/g),m=>m[1]).sort(),ids);
  }
  assert.match(transactionDetail,/回执、实体与暂存记录不计入记忆条数/);
  assert.match(section(transactions.save.html,'结构化记忆提交变更'),/新增记忆 · 1 条<\/label><div><a[^>]*data-memory="1@[^"]+">记忆 #1<\/a>/);
  assert.match(section(transactions.recall.html,'结构化记忆提交变更'),/仅召回统计更新 · 1 条<\/label><div><a[^>]*data-memory="1@[^"]+">记忆 #1<\/a>/);
  assert.equal(transactions.unchanged,true,'transaction rendering must preserve the recorded snapshots');
  assert.equal((transactions.mixed.html.match(/class="transaction-section"/g)||[]).length,2);
  assert.match(transactions.mixed.html,/<summary>完整事务提交 JSON<\/summary>/);
  const transactionRaw=transactions.mixed.html.match(/(<details[^>]*>)<summary>完整事务提交 JSON<\/summary>/);
  assert.ok(transactionRaw);assert.doesNotMatch(transactionRaw[1],/\bopen\b/);
  const candidateCards=vm.runInContext(`(()=>{
    const run=bundle.runs[2],span=run.spans.find(s=>s.name==='候选规则裁决（提交结果见事务）');
    const before=JSON.stringify(span);
    const render=(result,fields={})=>renderStageGroup([{...span,...fields,output:{...span.output,result}}],0,run);
    const value=value=>{
      const input={...span.input,value};
      return renderStageGroup([{...span,input,output:{...span.output,candidate:input}}],0,run);
    };
    const hostile={...span.input,value:{Text:'<img src=x onerror=alert(1)>'},
      evidence:[{id:'hostile',quote:'<svg onload=alert(2)>',session_id:'fixture',turn_id:'fixture'}]};
    const output={...span.output,candidate:hostile,result:{Ingested:{Rejected:'<script>alert(3)</script>'}}};
    const rendered={single:render(span.output.result),
      rejected:render({Ingested:{Rejected:'source is not user evidence'}}),
      duplicate:render({Ingested:{Duplicate:{evidence_id:'fixture-evidence',memory_id:1}}}),
      ignored:render({Ingested:'Ignored'}),conflict:render({Ingested:{Conflict:{target:1}}}),
      pending:render({Pending:{staged_id:7,resolution:{Ambiguous:['entity-a','entity-b']}}}),
      error:render(null,{status:'failed',error:{reason:'fixture candidate failure'}}),
      unknown:render({FutureDecision:{detail:'future-result-marker'}}),
      fallback:renderStageGroup([{...span,input:null}],0,run),
      missing:renderStageGroup([{...span,input:null,output:{}}],0,run),
      zero:value({Integer:0}),no:value({Boolean:false}),entity:value({Entity:'entity-fixture-7'}),
      hostile:renderStageGroup([{...span,input:hostile,output}],0,run)};
    const trace={admission:'Admit',admission_reason:'admission policy admitted candidate',
      existing_memory_ids:[3,4],matched_memory_ids:[],reconciliation:{action:'New',target:null,reason:null},
      reconciliation_reason:'no active equal-scope memory matched'};
    const traced=(result,decision_trace)=>renderStageGroup([{...span,output:{...span.output,result,decision_trace}}],0,run);
    rendered.traces={
      new:traced(span.output.result,trace),
      checks:traced(span.output.result,{...trace,admission_checks:[
        {rule:'evidence_complete',status:'Passed',reason:'evidence is complete'},
        {rule:'subject_resolved',status:'Passed',reason:'subject is resolved'},
        {rule:'extraction_confidence',status:'Passed',reason:'extraction confidence is not low'},
        {rule:'user_evidence',status:'Passed',reason:'source is user evidence'},
        {rule:'episode_value',status:'NotApplicable',reason:'candidate is not an episode'}]}),
      earlyRejection:traced({Ingested:{Rejected:'evidence is incomplete'}},{...trace,
        admission:{Reject:'evidence is incomplete'},admission_reason:'evidence is incomplete',
        admission_checks:[{rule:'evidence_complete',status:'Failed',reason:'evidence is incomplete'}],
        reconciliation:null,reconciliation_reason:null}),
      custom:traced(span.output.result,{...trace,admission_checks:null}),
      reinforce:traced({Ingested:{Applied:{action:'Reinforce',memory_id:3,target:3}}},{...trace,matched_memory_ids:[3],
        reconciliation:{action:'Reinforce',target:3,reason:null},reconciliation_reason:'same semantic slot, scope, and assertion matched independent evidence'}),
      duplicate:traced({Ingested:{Duplicate:{evidence_id:'fixture-evidence',memory_id:3}}},{...trace,reconciliation:null,
        reconciliation_reason:'duplicate evidence id; reconciliation skipped'}),
      ignored:traced({Ingested:'Ignored'},{...trace,reconciliation:{action:'Ignore',target:null,reason:null},
        reconciliation_reason:'candidate explicitly requests no write'}),
      conflict:traced({Ingested:{Conflict:{target:3}}},{...trace,reconciliation:{action:'Conflict',target:3,reason:null},
        reconciliation_reason:'same slot has a different assertion without explicit update intent'}),
      rejected:traced({Ingested:{Rejected:'source is not user evidence'}},{admission:{Reject:'source is not user evidence'},
        admission_reason:'source is not user evidence',existing_memory_ids:[],matched_memory_ids:[],reconciliation:null,reconciliation_reason:null}),
      pending:traced({Pending:{staged_id:7,resolution:'NotFound'}},null),
      missing:traced(span.output.result,{admission:'Admit'}),
      unknown:traced(span.output.result,{...trace,admission_reason:'<svg onload=alert(4)>',reconciliation_reason:'<img src=x onerror=alert(5)>'}),
      hostileChecks:traced(span.output.result,{...trace,admission_checks:[{rule:'<script>bad</script>',status:'Passed',reason:'<img src=x onerror=alert(6)>'}]})
    };
    return {...rendered,unchanged:JSON.stringify(span)===before};
  })()`,context);
  const candidateSection=(markup,label)=>section(markup,label).replace(/<details class="raw"[^>]*>[\s\S]*?<\/details>/g,'');
  const candidateContent=candidateSection(candidateCards.single,'候选内容');
  for(const text of ['中文','以后所有回答都必须使用中文。','类型','来源','置信度']) assert.ok(candidateContent.includes(text));
  assert.match(candidateContent,/Constraint|约束/);assert.match(candidateContent,/UserStatement|用户陈述/);
  assert.match(candidateContent,/High|高/);
  const decision=markup=>candidateSection(markup,'决策结果与原因');
  assert.match(decision(candidateCards.single),/新增记忆/);
  assert.match(decision(candidateCards.single),/具体裁决原因未采集/);
  assert.match(decision(candidateCards.single),/等待事务提交（阶段记录时）/);
  assert.doesNotMatch(decision(candidateCards.single),/已保存|无匹配旧记忆/);
  for(const [key,label] of [['rejected','拒绝保存'],['duplicate','重复证据，跳过写入'],['ignored','忽略本次候选'],['conflict','存在冲突，未写入'],['pending','等待主体澄清'],['error','处理失败']]) {
    assert.ok(decision(candidateCards[key]).includes(label),key+' must show its own decision');
  }
  assert.match(decision(candidateCards.rejected),/来源不是用户陈述或用户确认/);
  assert.match(decision(candidateCards.error),/fixture candidate failure/);
  assert.match(candidateSection(candidateCards.fallback,'候选内容'),/以后所有回答都必须使用中文。/);
  assert.match(candidateSection(candidateCards.missing,'候选内容'),/候选内容未采集/);
  assert.match(decision(candidateCards.missing),/决策结果未采集/);
  assert.match(candidateCards.unknown,/FutureDecision/);assert.match(candidateCards.unknown,/future-result-marker/);
  assert.doesNotMatch(decision(candidateCards.unknown),/新增记忆|已保存/);
  assert.match(candidateSection(candidateCards.zero,'候选内容'),/>0</);
  assert.match(candidateSection(candidateCards.no,'候选内容'),/否/);
  assert.match(candidateSection(candidateCards.entity,'候选内容'),/实体：entity-fixture-7/);
  assert.match(candidateSection(candidateCards.hostile,'候选内容'),/&lt;img src=x onerror=alert\(1\)&gt;/);
  assert.match(candidateSection(candidateCards.hostile,'候选内容'),/&lt;svg onload=alert\(2\)&gt;/);
  assert.match(decision(candidateCards.hostile),/&lt;script&gt;alert\(3\)&lt;\/script&gt;/);
  assert.doesNotMatch(candidateCards.hostile,/<img|<svg|<script/);
  for(const title of ['完整候选内容 JSON','完整决策结果 JSON']) {
    const details=candidateCards.single.match(new RegExp('(<details[^>]*>)<summary>'+title+'</summary>'));
    assert.ok(details,'raw evidence must remain available: '+title);assert.doesNotMatch(details[1],/\bopen\b/);
  }
  assert.equal((candidateCards.single.match(/class="candidate-section"/g)||[]).length,2);
  assert.equal((grouping.html.match(/class="candidate-section"/g)||[]).length,4,'each grouped candidate keeps both detail sections');
  assert.equal(candidateCards.unchanged,true,'candidate rendering must not rewrite source evidence');
  const traceDecision=decision(candidateCards.traces.new);
  for(const label of ['准入结果','准入原因','检查过的旧记忆','匹配的有效记忆','写入决策原因']) assert.ok(traceDecision.includes(label));
  assert.match(traceDecision,/准入已通过；本记录未采集逐项检查结果/);
  assert.doesNotMatch(traceDecision,/<label>原因<\/label>/,'remove the duplicate overall reason');
  assert.equal((traceDecision.match(/未匹配到同一适用范围内、可合并的有效记忆/g)||[]).length,1);
  for(const label of ['准入原因','写入决策原因'])assert.ok(traceDecision.includes('aria-label="'+label+'：说明"'),'reason help must be keyboard accessible');
  const checkedDecision=decision(candidateCards.traces.checks);
  assert.match(checkedDecision,/<div class="admission-reason"><details class="source-text"><summary><span class="text-box source-preview">通过：证据完整/,'long admission reasons show a preview before expansion');
  assert.match(checkedDecision,/展开全文/);
  assert.match(checkedDecision,/收起全文/);
  assert.doesNotMatch(traceDecision,/<div class="admission-reason"><details/,'short admission reasons stay fully visible');
  for(const text of ['通过：证据完整','通过：主体明确','通过：抽取可靠性达标','通过：证据来源符合要求','不适用：候选不是经历类记忆'])assert.ok(checkedDecision.includes(text),text);
  assert.doesNotMatch(checkedDecision,/准入已通过；本记录未采集逐项检查结果/);
  assert.match(decision(candidateCards.traces.earlyRejection),/<div class="admission-reason"><div class="text-box">未通过：证据不完整<\/div>/);
  assert.doesNotMatch(decision(candidateCards.traces.earlyRejection),/通过：主体明确|通过：抽取可靠性达标/,'do not invent checks after short-circuit rejection');
  assert.match(decision(candidateCards.traces.custom),/未采集逐项检查结果/);
  assert.doesNotMatch(decision(candidateCards.traces.custom),/通过：证据完整/,'custom policy without recorded checks must not inherit default checks');
  assert.match(decision(candidateCards.traces.hostileChecks),/&lt;img src=x onerror=alert\(6\)&gt;/);
  assert.doesNotMatch(candidateCards.traces.hostileChecks,/<script|<img/);
  assert.match(traceDecision,/未匹配到同一适用范围内、可合并的有效记忆/);
  assert.match(traceDecision,/<label>检查过的旧记忆<\/label><div>.*?记忆 #3.*?记忆 #4/);
  assert.match(traceDecision,/<label>匹配的有效记忆<\/label><div>0 条<\/div>/);
  assert.doesNotMatch(traceDecision,/具体裁决原因未采集/);
  assert.match(candidateCards.traces.new,/&quot;decision_trace&quot;/,'recorded diagnostic fields stay in the raw output');
  for(const [name,reason] of [
    ['reinforce','同类记忆的范围和内容一致，本次提供了独立的新证据'],
    ['duplicate','相同证据编号已处理，跳过写入决策'],
    ['ignored','候选明确要求不写入'],
    ['conflict','同类记忆的内容不同，且没有明确的更新意图'],
    ['rejected','来源不是用户陈述或用户确认']
  ]) assert.ok(decision(candidateCards.traces[name]).includes(reason),'missing recorded reason for '+name);
  assert.match(decision(candidateCards.traces.rejected),/未执行（准入拒绝）/);
  assert.match(decision(candidateCards.traces.pending),/未执行（等待主体澄清）/);
  assert.doesNotMatch(decision(candidateCards.traces.pending),/具体裁决原因未采集/);
  const missingTrace=decision(candidateCards.traces.missing);
  assert.match(missingTrace,/具体裁决原因未采集/);
  assert.match(missingTrace.match(/<label>匹配的有效记忆<\/label><div>(.*?)<\/div>/)?.[1]||'',/未采集/);
  assert.doesNotMatch(missingTrace,/<label>匹配的有效记忆<\/label><div>0 条<\/div>/,'missing lookup evidence must not imply zero matches');
  assert.match(decision(candidateCards.traces.unknown),/&lt;svg onload=alert\(4\)&gt;/);
  assert.match(decision(candidateCards.traces.unknown),/&lt;img src=x onerror=alert\(5\)&gt;/);
  assert.doesNotMatch(candidateCards.traces.unknown,/<svg|<img/);
  const tip=nodes.get('stage-tooltip');tip.offsetWidth=320;tip.offsetHeight=100;
  const button={dataset:{stageInfo:'读取已有快照，不新增记忆。'},getBoundingClientRect:()=>({left:380,top:250,bottom:274})};
  const target={closest:()=>button};
  handlers.pointerover({target});
  assert.equal(tip.hidden,false);assert.equal(tip.textContent,button.dataset.stageInfo);
  assert.equal(tip.style.left,'68px');assert.equal(tip.style.top,'142px');
  handlers.pointerout({target,relatedTarget:{closest:()=>tip}});
  assert.equal(pendingHelpClose,null,'moving onto help must keep it open');
  let prevented=false;
  handlers.click({target,preventDefault(){prevented=true;}});
  assert.equal(prevented,true,'info click must not toggle the stage disclosure');
  handlers.keydown({key:'Escape',preventDefault(){}});assert.equal(tip.hidden,true);
  handlers.focusin({target});assert.equal(tip.hidden,false);
  handlers.focusout({target});assert.equal(tip.hidden,true);
  handlers.pointerover({target});handlers.pointerout({target,relatedTarget:null});
  pendingHelpClose();assert.equal(tip.hidden,true);
  for(const name of ['准入原因','写入决策原因']){
    const description=vm.runInContext(`CANDIDATE_REASON_HELP[${JSON.stringify(name)}]`,context);
    const reasonButton={...button,dataset:{stageInfo:description}},reasonTarget={closest:()=>reasonButton};
    handlers.pointerover({target:reasonTarget});assert.equal(tip.hidden,false);assert.equal(tip.textContent,description);
    handlers.keydown({key:'Escape',preventDefault(){}});assert.equal(tip.hidden,true);
    handlers.focusin({target:reasonTarget});assert.equal(tip.textContent,description);
    handlers.focusout({target:reasonTarget});assert.equal(tip.hidden,true);
  }
  assert.equal(network,0);
  assert.match(html,/connect-src 'none'/);
  console.log(JSON.stringify({status:'passed',checks:['real and fixture imports','call/run links','4096 usage','empty recall','search','page changes','disabled channels','incomplete evidence','retrieval stage counts and missing evidence','conflicts versus filtering','global context counts','retrieval detail sections and raw records','stage snapshot isolation','retrieval detail HTML escaping','transaction changes and per-commit isolation','transaction missing evidence and unconfirmed states','candidate cards and decisions','candidate values, escaping and raw evidence','recorded admission and reconciliation reasons','lexical raw candidates, scope and coverage, escaping and no memory claims'],network_requests:network,scope:'Node rendering harness, no browser automation'}));
})().catch(error=>{console.error(error);process.exitCode=1;});
