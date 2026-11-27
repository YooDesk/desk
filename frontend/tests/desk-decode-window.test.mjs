import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';

const source = await readFile(new URL('../../browser_static/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  function handleFrame(bytes) {');
const end = source.indexOf('  function resetDecoder()', start);
assert(start >= 0 && end > start);

test('a buffered H264 decoder can warm up while a stalled decoder stays bounded', () => {
  let decoded = 0, refreshes = 0;
  const state = {stats: {bytes: 0}, waitingKey: true, submitted: new Map()};
  const context = vm.createContext({
    state, performance, canvas: {width: 2560, height: 1440},
    window: {EncodedVideoChunk: class {constructor(init) {Object.assign(this, init);}}},
    extensions: {consume() {}}, fields: {}, say() {},
    decodeMessageFields: pts => ({video: {pts, key: pts === 1, data: new Uint8Array(1)}}),
    ensureDecoder: () => ({decode() {decoded++;}}),
    refreshVideo() {refreshes++; state.submitted.clear(); state.waitingKey = true;},
  });
  vm.runInContext(source.slice(start, end), context);
  // No output yet: this exceeds the old eight-picture reset threshold.
  for (let pts = 1; pts <= 16; pts++) context.handleFrame(pts);
  assert.equal(decoded, 16);
  assert.equal(refreshes, 0);
  // Actual decoder output frees a slot; submitting input never claims output.
  assert.equal(state.submitted.size, 16);
  state.submitted.delete(1);
  for (let pts = 17; pts <= 19; pts++) context.handleFrame(pts);
  assert.equal(decoded, 19);
  assert.equal(state.submitted.size, 18);
  context.handleFrame(20);
  assert.equal(decoded, 19);
  assert.equal(refreshes, 1);
});
