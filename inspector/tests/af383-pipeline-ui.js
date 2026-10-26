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

const evaluate = code=>vm.runInContext(code,context);
evaluate(`
const event=(name,trace,source,parent,attempt,output={})=>({id:trace+name,name,status:'completed',
 trace_meta:{trace_id:trace,source_id:source,parent_trace_id:parent,attempt_id:attempt,round:1},
 output,llm_call_ids:[]});
bundle.runs=[
 {id:'D',tool:'memory_prepare',spans:[event('source_receipt','D','source-1',null,null,{source:{source_id:'source-1',status:'stored'}})]},
 {id:'A',tool:'background',spans:[event('w2_summary','A','source-1','D','attempt-1',{total:0}),event('w3_skipped','A','source-1','D','attempt-1',{reason:'no_candidates'}),event('w4_plan','A','source-1','D','attempt-1',{groups:0,held:0,model_called:false,reason:'no_supported_candidates'})]},
 {id:'B',tool:'background',spans:[event('w5_commit','B','source-1','D','attempt-1',{product_ids:[12],decision_types:['new']})]},
 {id:'C',tool:'background',spans:[event('r3_evidence','C',null,'D','recall-1',{records:1,raw:2}),event('r5_final','C',null,'D','recall-1',{status:'supported',facts:1})]},
 {id:'old',tool:'background',source:{source_id:'source-1'},spans:[event('w2_summary','old',null,null,'attempt-old',{total:1})]},
 {id:'unrelated',tool:'background',linked_source_ids:['source-1'],spans:[event('r5_final','unrelated','source-2',null,'other',{facts:1})]}
];
bundle.llm_calls=[];
`);
assert.equal(evaluate('messageRuns(bundle.runs[0]).length'),5);
assert.equal(evaluate('messageRuns(bundle.runs[5]).length'),1,'evidence references must not join messages');
const rendered=evaluate('renderMessagePipeline(bundle.runs[0])');
for(const lane of ['W0','W1','W2','W3','W4','W5','R1','R2','R3','R4','R5'])
 assert.match(rendered,new RegExp('data-pipeline-lane="'+lane+'"'));
assert.match(rendered,/未记录/);
assert.match(rendered,/跳过/);
assert.match(rendered,/零候选/);
assert.match(rendered,/attempt-1/);
assert.match(rendered,/attempt-old/);
assert.match(rendered,/recall-1/);
assert.match(rendered,/已提交产物 ID/);
assert.match(rendered,/本 attempt 已调用模型/);
assert.doesNotMatch(rendered,/source-2/);
evaluate(`bundle.llm_calls=[{id:'call',stage:'w1_v5',model:'fixture-model',effort:'low',usage:{input_tokens:10,output_tokens:20},duration_ms:3,parsed_result:{candidates:[{}]}}];`);
context.modelSpan={name:'model_start',status:'completed',trace_meta:{stage:'w1_v5'},llm_call_ids:['call'],output:{}};
assert.equal(evaluate('pipelineLane(modelSpan)'),'W1');
assert.match(evaluate('pipelineResult(modelSpan)'),/fixture-model/);
assert.match(evaluate('pipelineResult(modelSpan)'),/low/);
assert.match(evaluate('pipelineResult(modelSpan)'),/candidates/);
assert.equal(network,0);
console.log('AF383 pipeline UI checks passed');
