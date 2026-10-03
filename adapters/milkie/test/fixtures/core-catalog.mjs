// Rust supplies its live catalogue. Real SDK, deterministic CLI protocol only;
// this cannot serve as native model or complete team acceptance.
import assert from 'node:assert/strict';
import {mkdtemp, mkdir, symlink, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {ExecutionClient, MemoryStore, JsonlEventStore} from '@freemanxu/milkie';
import {executeCliTurn} from '../../dist/src/cli-turn.js';
import {executeApiTurn} from '../../dist/src/api-turn.js';
import {ToolLedger} from '../../dist/src/tool-ledger.js';
let data = '';
for await (const chunk of process.stdin) data += chunk;
const catalogues = JSON.parse(data);
assert.equal(catalogues.length, 3);
for (const [index, description] of catalogues.entries()) {
  const root = await mkdtemp(join(tmpdir(),'atelier-core-catalog-'));
  try {
    for (const name of ['bin','cwd','config','sessions','journal','ledger']) await mkdir(join(root,name),{mode:0o700});
    await symlink(resolve('test/fixtures/pi.cjs'),join(root,'bin','pi'));
    await writeFile(join(root,'config','auth.json'),'{}',{mode:0o600});
    const execution = new ExecutionClient({dataDir:join(root,'data'),connection:{contractVersion:1,fields:{transport:'agent-cli',runtime:'pi'}},env:{PATH:`${join(root,'bin')}:${process.env.PATH}`}});
    const context = execution.createContext(join(root,'cwd'),{configDir:join(root,'config'),sessionDir:join(root,'sessions')});
    const hasRead = description.tools.some(tool=>tool.name==='read_file');
    assert.equal(hasRead,index>0,'executor and verifier must include the real code tools');
    let forwarded = 0;
    const terminal = await executeCliTurn({execution,nativeContextId:context.contextId,scope:{taskId:'task',runId:'run',deliveryId:'delivery'},journalDirectory:join(root,'journal'),ledger:new ToolLedger(join(root,'ledger'),'delivery'),tools:description.tools,skill:description.skill,goal:'catalogue integration',input:JSON.stringify({calls:hasRead?[{name:'atelier_read_file',input:{path:'index.html'}}]:[]}),signal:new AbortController().signal,forward:async operation=>{assert.equal(operation.name,'read_file');forwarded++;return {ok:true};}});
    assert.equal(terminal.stopReason,'completed');
    assert.equal(forwarded,hasRead?1:0);
    let apiRequests=0, apiForwarded=0;
    const api=await executeApiTurn({workerId:'worker',taskId:'task',runId:'api-run',contextId:'api-context',goal:'catalogue integration',input:'请按当前职责处理工作消息。',skill:description.skill,tools:description.tools,model:{model:'fixture',provider:'fixture',adapter:'fixture'},stateStore:new MemoryStore(),eventStore:new JsonlEventStore(join(root,'api-events')),ledger:new ToolLedger(join(root,'api-ledger'),'api-delivery'),gateway:{
      async complete(request){
        assert.deepEqual(request.tools.map(t=>t.name).sort(),description.tools.map(t=>t.name).sort());
        assert.ok(request.system.includes(description.skill));
        if(apiRequests++===0){const tool=hasRead?'read_file':'task_read';const input=hasRead?{path:'index.html'}:{};return {content:[{type:'tool_use',id:'api-call',name:tool,input}],toolCalls:[{id:'api-call',name:tool,input}],finishReason:'tool_use'};}
        return {content:[{type:'text',text:'已处理'}],toolCalls:[],finishReason:'end_turn'};
      },
      async *stream(){throw new Error('unexpected_stream');}
    },forward:async operation=>{assert.equal(operation.name,hasRead?'read_file':'task_read');apiForwarded++;return {ok:true};}});
    assert.equal(api.result.status,'completed',JSON.stringify(api));
    assert.equal(apiRequests,2);
    assert.equal(apiForwarded,1);
  } finally {await rm(root,{recursive:true,force:true});}
}
