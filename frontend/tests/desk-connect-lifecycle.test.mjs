import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';
const source=await readFile(new URL('../../browser_static/app.js',import.meta.url),'utf8');
const start=source.indexOf('  var connectionWanted = false;'),end=source.indexOf('  // protobuf 最小解码',start);
function fixture(){
  const requests=[], timers=[], callbacks=[], storage=new Map();let release,entered,reloads=0;
  const gate=new Promise(resolve=>release=resolve),posted=new Promise(resolve=>entered=resolve);
  const state={sessionId:null,stream:null,readonly:false,capabilities:{pointer:true,keyboard:true}};
  let serial=0;
  const context=vm.createContext({state,api:'/desk',window:{},navigator:{onLine:true},AbortController,AbortSignal,DOMException,Promise,performance,
    connectorAccess:false, document:{documentElement:{dataset:{}}},location:{pathname:'/desk/',href:'https://device.example/desk/',reload(){reloads++;}},
    viewerRecoveryTransport:null, viewerRecoveryCredential:'',
    viewerEntryFetch:async()=>({ok:true,headers:{get:()=> 'text/html'}}),
    sessionStorage:{getItem:key=>storage.get(key),setItem:(key,value)=>storage.set(key,value)},
    setTimeout:(callback,delay)=>{callbacks.push(callback);timers.push(delay);return timers.length;},clearTimeout(){},setStatus(){},say(){},el:()=>({value:'0'}),updateReadonlyControl(){},
    loadCapabilities:async()=>{},loadDisplays:async()=>{},message:{textContent:''},
    disconnect:()=>{state.sessionId=null;state.stream=null;return Promise.resolve();},
    request:async(method,url)=>{requests.push({method,url});if(method==='POST'&&url==='/desk/sessions/'){serial+=1;if(serial===1){entered();await gate;}return {business_session_id:`s${serial}`};}return {};},
  });
  vm.runInContext(source.slice(start,end),context);
  const openEvents=context.openEvents;
  context.openEvents=async()=>{};context.waitForStream=async()=>{state.stream={readyState:1};};context.connectionWanted=true;
  return {context,state,requests,release,posted,timers,callbacks,storage,reloads:()=>reloads,openEvents};
}

test('Desk heartbeat uses authenticated events while the video writer is busy',()=>{
  const {context,state}=fixture();
  const sent=[];let tick,restarts=0,now=1000;
  Object.assign(context,{setInterval:fn=>{tick=fn;},performance:{now:()=>now},el:()=>null,WebSocket:{OPEN:1},restartConnection:()=>{restarts++;}});
  context.window.__yoodeskDesk={};
  state.stats={frames:0,bytes:0,since:0,ping:null};
  state.stream={readyState:1,send:()=>{throw new Error('video write must not carry heartbeat');}};
  state.events={readyState:1,send:message=>sent.push(JSON.parse(message))};
  context.lastPong=now;
  const end=source.indexOf('  function teardown()');
  const start=source.lastIndexOf('  setInterval(function () {',end);
  vm.runInContext(source.slice(start,end),context);
  tick();
  assert.equal(sent[0].type,'ping');assert.equal(restarts,0);
  now=9001;tick();assert.equal(restarts,1,'the original eight-second heartbeat deadline remains enforced');
});

test('events setup waits for open and cancellation closes only its socket',async()=>{
  const f=fixture(),sockets=[];
  f.context.URL=URL;f.context.location={origin:'https://device.example'};
  f.context.WebSocket=class{constructor(){sockets.push(this);}close(){this.closed=true;}};
  const abort=new AbortController();
  let ready=false;
  const opening=f.openEvents('/events',0,abort.signal).then(()=>{ready=true;});
  await new Promise(setImmediate);
  assert.equal(ready,false);assert.equal(sockets.length,1);
  sockets[0].onopen();await opening;assert.equal(ready,true);
  const nextAbort=new AbortController();
  const canceled=f.openEvents('/events',0,nextAbort.signal);
  await new Promise(setImmediate);nextAbort.abort();
  await assert.rejects(canceled,{name:'AbortError'});assert.equal(sockets[1].closed,true);
});
// 服务器已经创建会话但 POST 响应晚到时，模式切换必须释放该资源，且只有一个连接任务。
test('a late session creation is deleted before the replacement connects',async()=>{
  const f=fixture();const old=f.context.runConnection();await f.posted;
  f.context.restartConnection();
  const duplicate=f.context.runConnection();await duplicate;
  assert.equal(f.requests.filter(r=>r.method==='POST').length,1);
  f.release();await old;
  assert(f.requests.some(r=>r.method==='DELETE'&&r.url==='/desk/sessions/s1/'));
  assert.equal(f.state.sessionId,null);
  await f.context.runConnection();
  assert.equal(f.state.sessionId,'s2');
});
// 退出期间到达的创建响应也必须关闭，不能恢复流或触发新的连接。
test('logout during session creation never resumes the stream',async()=>{
  const f=fixture();const attempt=f.context.runConnection();await f.posted;
  f.context.connectionWanted=false;f.context.connectionGeneration+=1;f.context.connectionController.abort();
  f.release();await attempt;
  assert.equal(f.state.sessionId,null);
  assert(f.requests.some(r=>r.method==='DELETE'&&r.url==='/desk/sessions/s1/'));
  await f.context.runConnection();assert.equal(f.requests.filter(r=>r.method==='POST').length,1);
});

test('consecutive unexpected failures back off and a user restart stays immediate',()=>{
  const {context,timers}=fixture();
  context.restartConnection(true);
  context.restartConnection(true);
  context.restartConnection(true);
  assert.deepEqual(timers,[500,1000,2000]);
  context.restartConnection();
  context.restartConnection(true);
  assert.deepEqual(timers.slice(-2),[0,500]);
});
test('an unexpected restart during setup retains its delay after cleanup',async()=>{
  const f=fixture();
  f.context.performance={now:()=>100};
  const old=f.context.runConnection();
  await f.posted;
  f.context.restartConnection(true);
  f.release();
  await old;
  assert.equal(f.timers.at(-1),500);
});

test('a closed Repeater transport reopens the viewer entry instead of retrying its stale session',async()=>{
  const f=fixture();
  f.context.connectorAccess=true;
  f.context.document.documentElement.dataset.transportState='closed';
  f.context.restartConnection(true,'stream_closed');
  f.context.restartConnection(true,'events_closed');
  await f.context.runConnection();
  assert.deepEqual(f.timers,[1000]);
  assert.equal(f.requests.length,0);
  await Promise.all([f.callbacks[0](), f.callbacks[0]()]);
  assert.equal(f.reloads(),1);
  assert.equal(f.storage.get(f.context.transportRecoveryKey),'1');
});

test('transport recovery waits for network and stops after a bounded number of page attempts',async()=>{
  const f=fixture();
  f.context.connectorAccess=true;
  f.context.navigator.onLine=false;
  f.context.recoverViewerTransport();
  assert.equal(f.timers.length,0);
  f.context.navigator.onLine=true;
  f.context.resumeViewerTransportRecovery(0);
  await f.callbacks[0]();
  assert.equal(f.reloads(),1);
  f.context.stopViewerTransportRecovery();
  f.storage.set(f.context.transportRecoveryKey,'9');
  f.context.recoverViewerTransport();
  assert.deepEqual(f.timers,[0,15000]);
  await f.callbacks[1]();
  assert.equal(f.storage.get(f.context.transportRecoveryKey),'10');
  f.context.stopViewerTransportRecovery();
  f.context.recoverViewerTransport();
  assert.deepEqual(f.timers,[0,15000]);
});

test('an unavailable gateway entry is retried without navigating the existing page to an error',async()=>{
  const f=fixture();
  f.context.connectorAccess=true;
  f.context.viewerEntryFetch=async()=>({ok:false,status:503});
  f.context.recoverViewerTransport();
  await f.callbacks[0]();
  assert.equal(f.reloads(),0);
  assert.deepEqual(f.timers,[1000,2000]);
  f.context.viewerEntryFetch=async()=>({ok:false,status:403});
  await f.callbacks[1]();
  assert.equal(f.reloads(),0);
  assert.equal(f.context.connectionWanted,false);
  assert.deepEqual(f.timers,[1000,2000]);
});

test('connector restart installs a fresh transport and restores access in the same page', async()=>{
  const f=fixture();
  f.context.connectorAccess=true;
  f.state.sessionId='old-desk';
  f.context.viewerRecoveryCredential='test-only';
  const next={state:{path:'websocket'}};
  let closed=false, restored=false;
  f.context.disconnect=async()=>{closed=true;f.state.sessionId=null;};
  f.context.viewerRecoveryTransport={async reconnect(signal){
    assert(closed);
    assert(!signal.aborted);
    return next;
  }};
  f.context.login=async(credential,recovering)=>{
    assert.equal(credential,'test-only');
    assert.equal(recovering,true);
    assert.equal(await f.context.transportReady,next.state);
    restored=true;
    f.context.stopViewerTransportRecovery();
  };
  f.context.recoverViewerTransport();
  await f.callbacks[0]();
  assert(restored);
  assert.equal(f.reloads(),0);
  assert.equal(f.context.viewerRecoveryTransport,next);
});
