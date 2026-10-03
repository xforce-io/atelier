import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, link, readdir, readFile, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ToolLedger, type ToolOperation } from '../src/tool-ledger.js';

test('delivery-scoped ledger migrates completed old records without replay and preserves original identity',async()=>{
  const root=await mkdtemp(join(tmpdir(),'atelier-ledger-'));
  try {
    let calls=0; let original:ToolOperation|undefined;
    const first=new ToolLedger(root,'delivery-one');
    await first.call('run-one','call-one','message_respond',{reason:'done'},async operation=>{original=operation;calls++;return {ok:true};});
    const filename=(await readdir(root))[0]!;
    const bytes=await readFile(join(root,filename));
    // Simulate a migration interrupted after link, before removal of the root name.
    await mkdir(join(root,'delivery-one'),{mode:0o700});
    await link(join(root,filename),join(root,'delivery-one',filename));
    const next=await ToolLedger.openDelivery(root,'delivery-two');
    assert.deepEqual(await next.recover(async()=>{throw new Error('foreign_call');},true),[]);
    assert.equal(calls,1);
    assert.deepEqual(await readFile(join(root,'delivery-one',filename)),bytes);
    assert.deepEqual((await readdir(root)).sort(),['delivery-one','delivery-two']);
    const retry=await ToolLedger.openDelivery(root,'delivery-one');
    const recovered=await retry.recover(async operation=>{assert.deepEqual(operation,original);return {ok:false,error:'forbidden'};},true);
    assert.deepEqual(recovered[0]?.result,{ok:false,error:'forbidden'});
  }finally{await rm(root,{recursive:true,force:true});}
});

test('unreconciled foreign delivery blocks both legacy and new layouts without moving or calling it',async()=>{
  for(const oldLayout of [true,false]) {
    const root=await mkdtemp(join(tmpdir(),'atelier-ledger-pending-'));
    try {
      const first=oldLayout?new ToolLedger(root,'delivery-one'):await ToolLedger.openDelivery(root,'delivery-one');
      let operationId='';
      await assert.rejects(first.call('run-one','call-one','message_respond',{},async operation=>{operationId=operation.operationId;throw new Error('lost_reply');}),/lost_reply/);
      const before=(await readdir(root)).sort();
      await assert.rejects(ToolLedger.openDelivery(root,'delivery-two'),/foreign_delivery_unreconciled/);
      assert.deepEqual((await readdir(root)).sort(),before);
      const retry=await ToolLedger.openDelivery(root,'delivery-one');
      const recovered=await retry.recover(async operation=>{assert.equal(operation.operationId,operationId);return {ok:true};});
      assert.equal(recovered.length,1);
      const next=await ToolLedger.openDelivery(root,'delivery-two');
      assert.deepEqual(await next.recover(async()=>{throw new Error('foreign_call');},true),[]);
    }finally{await rm(root,{recursive:true,force:true});}
  }
});

test('migration refuses conflicting durable records and linked delivery directories',async()=>{
  const root=await mkdtemp(join(tmpdir(),'atelier-ledger-conflict-'));
  try {
    await new ToolLedger(root,'delivery-one').call('run','call','message_send',{},async()=>({ok:true}));
    await new ToolLedger(join(root,'delivery-one'),'delivery-one').call('run','call','message_send',{},async()=>({ok:true}));
    await assert.rejects(ToolLedger.openDelivery(root,'delivery-one'),/migration_conflict/);
    assert.equal((await readdir(root)).length,2);
    await rm(join(root,'delivery-one'),{recursive:true});
    await symlink(root,join(root,'delivery-one'));
    await assert.rejects(ToolLedger.openDelivery(root,'delivery-two'),/ledger_corrupt/);
    await assert.rejects(ToolLedger.openDelivery(root,'../other'),/delivery_invalid/);
  }finally{await rm(root,{recursive:true,force:true});}
});
