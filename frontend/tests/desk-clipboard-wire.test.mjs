import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';

const source = await readFile(new URL('../../browser_static/desk-clipboard.js', import.meta.url), 'utf8');
const context = vm.createContext({Uint8Array, DataView, TextEncoder, TextDecoder, maxBytes: 16 * 1024 * 1024});
vm.runInContext(source.slice(source.indexOf('  function encodeContent('), source.indexOf('  async function fingerprint(')), context);
const stream = bytes => new ReadableStream({start(controller) {
  for (let i = 0; i < bytes.length; i += 3) controller.enqueue(bytes.subarray(i, i + 3));
  controller.close();
}});

test('clipboard binary frame preserves arbitrary pixels across split reads', async () => {
  const pixels = Uint8Array.of(0, 128, 255, 0);
  const frame = context.encodeContent({kind: 'image', width: 1, height: 1, data: pixels});
  assert.deepEqual([...frame], [89, 68, 67, 49, 2, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, ...pixels]);
  const result = await context.decodeContent(stream(frame));
  assert.equal(result.width, 1);
  assert.equal(result.height, 1);
  assert.deepEqual([...result.data], [...pixels]);
});

test('clipboard distinguishes empty text from unchanged and rejects corrupt frames', async () => {
  const text = await context.decodeContent(stream(context.encodeContent({kind: 'text', text: ''})));
  assert.equal(text.kind, 'text');
  assert.equal(text.text, '');
  const none = Uint8Array.from([89, 68, 67, 49, ...new Array(12).fill(0)]);
  assert.equal((await context.decodeContent(stream(none))).changed, false);
  const frame = context.encodeContent({kind: 'text', text: 'x'});
  frame[16] = 255;
  await assert.rejects(context.decodeContent(stream(frame)));
  await assert.rejects(context.decodeContent(stream(none.subarray(0, 15))));
  frame[4] = 2;
  await assert.rejects(context.decodeContent(stream(frame)));
});
