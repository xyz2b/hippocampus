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
const evaluate = code => vm.runInContext(code,context);
const summary={before:null,after:null,business_changed:true,sources_changed:false,
 committed_revision:8,read_revision:7,revision_observability:'persisted',
 counts:{sources:2,relations:1,updates:3},duration_ms:2};
context.summary=summary;
const rendered=evaluate("renderTransactionDetail({name:'transaction_committed',status:'completed',output:summary})");
assert.match(rendered,/已提交 · 事务变更摘要/);
assert.match(rendered,/7 → 8/);
assert.match(rendered,/PMR_TRACE_FULL_SNAPSHOTS=1/);
assert.doesNotMatch(rendered,/事务尚未确认提交/);
assert.match(evaluate("stageResult({name:'transaction_committed',status:'completed',output:summary})"),/已提交/);
for(const name of ['w2_summary','w5_commit','r3_evidence','r5_final','retrieval']){
 context.span={name,status:'completed',id:name,output:{total:2,admitted:1,
 product_ids:[12],decision_types:['new'],revision:8,records:1,raw:2,
 status:'unavailable',query_error:'connect',segments_total:4,segments_indexed:0}};
 const result=evaluate('stageBody(span)');
 assert.doesNotMatch(result,/尚未完成|事务尚未确认提交/);
 if(name==='w5_commit'){assert.match(result,/已提交产物 ID/);assert.match(result,/12/);}
 if(name==='retrieval'){assert.match(result,/connect/);assert.match(result,/已索引片段/);}
}
const detail=evaluate(`runDetail({...bundle.runs[0],write_changes:[],spans:[
 {id:'tx',name:'transaction_committed',status:'completed',output:summary,llm_call_ids:[]},
 {id:'w5',name:'w5_commit',status:'completed',output:{product_ids:[12],decision_types:['new'],revision:8},llm_call_ids:[]}
]})`);
assert.doesNotMatch(detail,/最早观察到的失败|事务尚未确认提交/);
assert.match(detail,/写入 \/ 修改证据链/);
assert.match(detail,/已提交产物 ID/);
assert.equal(network,0);
console.log('AF381 summary UI checks passed');
