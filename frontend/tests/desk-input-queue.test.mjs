import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import test from 'node:test';
const source = await readFile(process.env.YOODESK_DESK_INPUT_SOURCE || new URL('../../browser_static/app.js', import.meta.url), 'utf8');
const events = (requests) => requests.flatMap(({payload}) => payload.type === 'batch' ? payload.events : [payload]);
const start = source.indexOf('  function sendInput(payload) {');
const end = source.indexOf('  function canvasPoint(event)', start);
assert(start >= 0 && end > start);
function fixture() {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const requests = [];
  let restarts = 0;
  const state = { sessionId: 'old', readonly: false, inputVersion: 0, modePending: false, inputController: new AbortController() };
  const context = vm.createContext({
    state,
    api: '/desk',
    inputTail: Promise.resolve(),
    inputPending: 0,
    transportRecoveryPending: false,
    disconnect: async () => {
      state.sessionId = null;
    },
    say: () => {},
    restartConnection: () => { restarts += 1; },
    request: async (method, url, payload, signal) => {
      requests.push({ url, payload });
      if (requests.length === 1) await new Promise((resolve, reject) => {
        gate.then(resolve);
        signal.addEventListener('abort', () => reject(new DOMException('input cancelled', 'AbortError')), {once: true});
      });
    },
  });
  vm.runInContext(source.slice(start, end), context);
  return { context, state, release, requests, get restarts() { return restarts; } };
}

test('absolute mouse motion keeps only the newest unsent position without reconnecting', async () => {
  const f = fixture();
  const first = { type: 'mouse', mask: 0, x: 0, y: 0 };
  const sent = f.context.sendInput(first);
  await Promise.resolve();
  const moves = Array.from({ length: 200 }, (_, x) => f.context.sendInput({ type: 'mouse', mask: 0, x: x + 1, y: 2 }));
  assert.equal(f.context.inputPending, 2);
  assert.equal(f.restarts, 0);
  assert.equal(f.requests[0].payload.x, 0, 'an in-flight request cannot be changed');
  f.release();
  await Promise.all([sent, ...moves]);
  assert.deepEqual(f.requests.map(({ payload }) => payload.x), [0, 200]);
  assert.equal(f.context.inputPending, 0);
});

test('buttons, wheel, relative motion and key combinations remain barriers to motion coalescing', async () => {
  const f = fixture();
  const send = (mask, x) => f.context.sendInput({ type: 'mouse', mask, x, y: 0 });
  const pending = [send(0, 0)];
  await Promise.resolve();
  pending.push(send(0, 1), send(0, 2), send(9, 2), send(0, 3), send(0, 4), send(10, 4), send(3, 5), send(5, 6), send(5, 7));
  pending.push(f.context.sendInputs([{ control: 4, down: true }, { layout: 'v', down: true }, { layout: 'v', down: false }, { control: 4, down: false }]));
  pending.push(send(0, 8), send(0, 9));
  f.release();
  await Promise.all(pending);
  assert.deepEqual(events(f.requests).map(payload => payload.type === 'mouse' ? [payload.mask, payload.x] : [payload.control || payload.layout, payload.down]), [
    [0, 0], [0, 2], [9, 2], [0, 4], [10, 4], [3, 5], [5, 6], [5, 7],
    [4, true], ['v', true], ['v', false], [4, false], [0, 9],
  ]);
});

test('a replacement session never merges its motion into an old pending position', async () => {
  const f = fixture();
  const old = f.context.sendInput({ type: 'mouse', mask: 0, x: 1, y: 0 });
  await Promise.resolve();
  const stale = f.context.sendInput({ type: 'mouse', mask: 0, x: 2, y: 0 });
  f.state.inputController.abort();
  f.state.sessionId = 'new';
  f.state.inputVersion += 1;
  f.state.inputController = new AbortController();
  const fresh = f.context.sendInput({ type: 'mouse', mask: 0, x: 3, y: 0 });
  const latest = f.context.sendInput({ type: 'mouse', mask: 0, x: 4, y: 0 });
  await Promise.all([old, stale, fresh, latest]);
  assert.deepEqual(f.requests.map(({ url, payload }) => [url.includes('/new/'), payload.x]), [[false, 1], [true, 4]]);
  assert.equal(f.restarts, 0);
});

test('an 80-event typing burst behind one slow request drains in bounded ordered batches', async () => {
  const f = fixture();
  const first = f.context.sendInput({type: 'mouse', mask: 0, x: 0, y: 0});
  await Promise.resolve();
  const keys = Array.from({length: 80}, (_, i) => ({
    type: 'key', layout: String.fromCharCode(97 + Math.floor(i / 2) % 26), down: i % 2 === 0,
  }));
  const pending = keys.map(key => f.context.sendInput(key));
  assert.equal(f.requests.length, 1);
  assert.equal(f.restarts, 0);
  f.release();
  await Promise.all([first, ...pending]);
  assert.deepEqual(f.requests.slice(1).map(({payload}) => payload.events.length), [32, 32, 16]);
  assert.deepEqual(events(f.requests).slice(1), keys);
  assert.equal(f.context.inputPending, 0);
});
test('a paste combination stays together while physical key releases arrive during HTTP awaits', async () => {
  const f = fixture();
  const batch = f.context.sendInputs([
    { control: 4, down: true },
    { layout: 'v', down: true },
    { layout: 'v', down: false },
    { control: 4, down: false },
  ]);
  await Promise.resolve();
  const physicalV = f.context.sendInput({ layout: 'v', down: false });
  const physicalCtrl = f.context.sendInput({ control: 4, down: false });
  f.release();
  await Promise.all([batch, physicalV, physicalCtrl]);
  assert.deepEqual(
    events(f.requests).map(payload => [payload.control || payload.layout, payload.down]),
    [
      [4, true],
      ['v', true],
      ['v', false],
      [4, false],
      ['v', false],
      [4, false],
    ],
  );
});
test('remaining keys from an old combination never enter a newly connected session', async () => {
  const f = fixture();
  const old = f.context.sendInputs([
    { control: 4, down: true },
    { layout: 'v', down: true },
    { layout: 'v', down: false },
    { control: 4, down: false },
  ]);
  await Promise.resolve();
  f.state.sessionId = 'new';
  const fresh = f.context.sendInput({ layout: 'x', down: true });
  f.release();
  await Promise.all([old, fresh]);
  assert.equal(f.requests.length, 2);
  assert(f.requests[0].url.includes('/old/'));
  assert(f.requests[1].url.includes('/new/'));
  assert.equal(f.requests[1].payload.layout, 'x');
});

test('aborting an old HTTP input unlocks the new session without waiting for its response', async () => {
  const f = fixture();
  const old = f.context.sendInput({ layout: 'a', down: true });
  await Promise.resolve();
  f.state.inputController.abort();
  f.state.sessionId = 'new';
  f.state.inputController = new AbortController();
  const fresh = f.context.sendInput({ layout: 'b', down: true });
  await Promise.race([Promise.all([old, fresh]), new Promise((_, reject) => setTimeout(() => reject(new Error('new input remained blocked')), 1000))]);
  assert.equal(f.requests.length, 2);
  assert(f.requests[1].url.includes('/new/'));
});

function modeFixture() {
  const f = fixture();
  Object.assign(f.context, {
    desiredReadonly: false,
    window: {__yoodeskDesk: {}},
    pressedKeyboard: new Map(),
    extensions: {pause: async () => {}},
    clipboard: {start() {}}, chat: {start() {}},
    updateReadonlyControl: () => {f.state.readonly = f.context.desiredReadonly;},
    el: () => ({ value: f.context.desiredReadonly ? 'view' : 'control' }),
  });
  const modeStart = source.indexOf('  var modeTail = Promise.resolve();');
  vm.runInContext(source.slice(modeStart, start), f.context);
  return f;
}

function availabilityFixture(viewerMode) {
  const nodes = new Map();
  const node = (id) => {
    if (!nodes.has(id)) nodes.set(id, { value: viewerMode, hidden: false, disabled: false });
    return nodes.get(id);
  };
  const state = { viewerMode, controlAllowed: true, capabilities: { pointer: true, keyboard: true }, sessionId: 'same', modePending: false };
  const context = vm.createContext({
    state, el: node, extensions: { updateAvailability() {} },
    clipboard: { stop() {} }, chat: { stop() {} },
    document: { body: { classList: { toggle() {} } } },
    window: { __yoodeskDesk: {} }, updateKeyboardLock() {},
  });
  const start = source.indexOf('  function updateReadonlyControl() {');
  const end = source.indexOf('  if (invitationCredential)', start);
  assert(start >= 0 && end > start, 'readonly controls source boundaries changed');
  vm.runInContext(source.slice(start, end), context);
  return { state, node, update: context.updateReadonlyControl };
}

test('temporary worker capability loss blocks input and restores the chosen viewer mode', () => {
  for (const preference of ['control', 'view']) {
    const f = availabilityFixture(preference);
    f.update();
    assert.equal(f.node('mode').value, preference);
    f.state.capabilities = { pointer: false, keyboard: false };
    f.update();
    assert.equal(f.state.readonly, true);
    assert.equal(f.node('mode').disabled, true);
    assert.equal(f.state.viewerMode, preference);
    f.state.capabilities = { pointer: true, keyboard: true };
    f.update();
    assert.equal(f.node('mode').value, preference);
    assert.equal(f.state.readonly, preference === 'view');
    assert.equal(f.node('mode').disabled, false);
  }
});

test('quick view/control toggles keep the session and discard old queued keys before input resumes', async () => {
  const f = modeFixture();
  const old = f.context.sendInputs([{layout: 'a', down: true}, {layout: 'b', down: true}]);
  await Promise.resolve();
  f.context.desiredReadonly = true; f.context.changeViewerMode();
  f.context.desiredReadonly = false; f.context.changeViewerMode();
  await f.context.sendInput({layout: 'x', down: true});
  f.release(); await old; await f.context.modeTail;
  assert.equal(f.state.sessionId, 'old');
  assert.equal(f.state.modePending, false);
  assert.deepEqual(events(f.requests).map(payload => payload.action || payload.layout), ['a', 'b', 'input.release', 'input.release']);
  await f.context.sendInput({layout: 'z', down: true});
  assert.equal(f.requests.at(-1).payload.layout, 'z');
});

test('failed input release keeps local input blocked without replacing the video session', async () => {
  const f = modeFixture();
  f.context.request = async () => {throw new Error('release failed');};
  f.context.changeViewerMode(); await f.context.modeTail;
  await f.context.sendInput({layout: 'a', down: true});
  assert.equal(f.state.modePending, true);
  assert.equal(f.state.sessionId, 'old');
  assert.equal(f.requests.length, 0);
});

test('closing during mode cleanup prevents an old release from reaching the next session', async () => {
  const f = modeFixture();
  const old = f.context.sendInput({layout: 'a', down: true});
  await Promise.resolve();
  f.context.desiredReadonly = true; f.context.changeViewerMode();
  f.state.inputController.abort();
  f.state.sessionId = 'new'; f.state.inputVersion += 1;
  await old; await f.context.modeTail;
  assert.equal(f.requests.length, 1);
  assert(f.requests[0].url.includes('/old/'));
});
