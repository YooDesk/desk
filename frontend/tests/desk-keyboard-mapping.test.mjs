import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const source = await readFile(new URL('../../browser_static/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  var CONTROL_KEYS =');
const end = source.indexOf('  // 键盘事件走与鼠标相同的 input 端点', start);
const shortcuts = source.indexOf('  function handleShortcut(', start);
const handlersEnd = source.indexOf("  canvas.addEventListener('blur', releasePressedKeyboard);", start);
assert(start >= 0 && end > start);

function fixture(os, viewer = os === 'windows' ? 'MacIntel' : 'Win32') {
  const handlers = new Map();
  const events = [];
  let now = 0;
  let focused = 0;
  const document = { body: {}, documentElement: {}, activeElement: null };
  const state = { platform: { os }, sessionId: 'desk', capabilities: {
    keyboard: true, clipboard_read: true, clipboard_write: true,
  } };
  const emit = (event) => events.push({ ...event });
  const scope = {
    canvas: { addEventListener: (name, handler) => handlers.set(name, handler), focus() { focused++; } },
    state, document, el: () => ({ contains: () => false }),
    navigator: { platform: viewer },
    performance: { now: () => now },
    clipboard: { copiedRemote() {}, paste(keys) { keys().forEach(emit); } },
    sendInput: emit,
    sendKey: (event) => emit({ type: 'key', ...event }),
    sendInputs: (batch) => batch.forEach(emit),
  };
  vm.runInNewContext(source.slice(start, handlersEnd) + source.slice(shortcuts, end), scope);
  return {
    events,
    state, document, focusDeskKeyboard: scope.focusDeskKeyboard, focusCount: () => focused,
    advance(ms) { now += ms; },
    press(name, key, code, extra) {
      handlers.get(name)({ key, code, preventDefault() {}, ...extra });
    },
  };
}

test('Desk recovers an empty keyboard focus and leaves chat or view-only mode alone', () => {
  const f = fixture('macos');
  f.document.activeElement = f.document.body;
  f.focusDeskKeyboard();
  assert.equal(f.focusCount(), 1);
  f.document.activeElement = { id: 'chat-input' };
  f.focusDeskKeyboard();
  f.document.activeElement = f.document.body;
  f.state.readonly = true;
  f.focusDeskKeyboard();
  assert.equal(f.focusCount(), 1);
});

test('Caps Lock casing stays usable on Mac, including Shift plus Caps Lock', () => {
  const f = fixture('macos');
  const caps = { getModifierState: key => key === 'CapsLock' };
  f.press('keydown', 'CapsLock', 'CapsLock');
  f.press('keyup', 'CapsLock', 'CapsLock');
  f.press('keydown', 'A', 'KeyA', caps);
  f.press('keyup', 'A', 'KeyA', caps);
  f.press('keydown', 'Shift', 'ShiftLeft', { shiftKey: true });
  f.press('keydown', 'b', 'KeyB', { ...caps, shiftKey: true });
  f.press('keyup', 'b', 'KeyB', { ...caps, shiftKey: true });
  f.press('keyup', 'Shift', 'ShiftLeft');
  f.press('keydown', 'CapsLock', 'CapsLock');
  f.press('keydown', 'c', 'KeyC');
  f.press('keyup', 'c', 'KeyC');
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [[25, true], ['a', true], ['a', false], [25, false],
      [25, true], [25, false], ['b', true], ['b', false], [25, true], [25, false],
      ['c', true], ['c', false]]);
});

test('Pinyin and candidate selection send key edges on Windows, macOS and Linux', () => {
  for (const os of ['windows', 'macos', 'linux']) {
    const f = fixture(os);
    for (const char of 'nihao') {
      const code = 'Key' + char.toUpperCase();
      f.press('keydown', char, code);
      f.press('keyup', char, code);
    }
    f.press('keydown', ' ', 'Space');
    f.press('keyup', ' ', 'Space');
    assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
      [...'nihao', 26].flatMap(key => [[key, true], [key, false]]));
    assert(f.events.every(e => !Object.hasOwn(e, 'text')));
  }
});

test('a second physical Ctrl+C within one second is passed as raw Ctrl+C on Mac', () => {
  const f = fixture('macos');
  f.press('keydown', 'Control', 'ControlLeft');
  const control = { ctrlKey: true };
  f.press('keydown', 'c', 'KeyC', control);
  f.press('keydown', 'c', 'KeyC', { ...control, repeat: true });
  f.press('keyup', 'c', 'KeyC', control);
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [[4, true], [4, false], [18, true], ['c', true], ['c', false], [18, false], [4, true]]);
  f.events.length = 0;
  f.advance(500);
  f.press('keydown', 'c', 'KeyC', control);
  f.press('keydown', 'c', 'KeyC', { ...control, repeat: true });
  f.press('keyup', 'c', 'KeyC', control);
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]), [['c', true], ['c', false]]);
  f.events.length = 0;
  f.advance(1001);
  f.press('keydown', 'c', 'KeyC', control);
  assert(f.events.some(e => e.control === 18 && e.down));
});

test('Windows to Mac maps Ctrl+A and redo without remapping ordinary Ctrl chords', () => {
  const f = fixture('macos');
  f.press('keydown', 'Control', 'ControlLeft');
  for (const key of ['a', 'y', 'g']) {
    f.press('keydown', key, 'Key' + key.toUpperCase(), {ctrlKey: true});
    f.press('keyup', key, 'Key' + key.toUpperCase(), {ctrlKey: true});
  }
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [[4, true], [4, false], [18, true], ['a', true], ['a', false], [18, false], [4, true],
      [4, false], [18, true], [25, true], ['z', true], ['z', false], [18, false], [25, false], [4, true],
      ['g', true], ['g', false]]);
});

test('a local Windows Win+V stays a Windows shortcut', () => {
  const f = fixture('windows', 'Win32');
  f.press('keydown', 'Meta', 'MetaLeft');
  f.press('keydown', 'v', 'KeyV', {metaKey: true});
  f.press('keyup', 'v', 'KeyV', {metaKey: true});
  f.press('keyup', 'Meta', 'MetaLeft');
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [[18, true], ['v', true], ['v', false], [18, false]]);
});

test('Mac to Windows maps Command navigation and redo, preserving Shift selection', () => {
  const f = fixture('windows', 'MacIntel');
  f.press('keydown', 'Meta', 'MetaLeft');
  f.press('keydown', 'Shift', 'ShiftLeft', {metaKey: true});
  f.press('keydown', 'ArrowLeft', 'ArrowLeft', {metaKey: true, shiftKey: true});
  f.press('keyup', 'ArrowLeft', 'ArrowLeft', {metaKey: true, shiftKey: true});
  f.press('keydown', 'Z', 'KeyZ', {metaKey: true, shiftKey: true});
  f.press('keyup', 'Z', 'KeyZ', {metaKey: true, shiftKey: true});
  f.press('keyup', 'Shift', 'ShiftLeft', {metaKey: true});
  f.press('keyup', 'Meta', 'MetaLeft');
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [[25, true], [16, true], [16, false],
      [25, false], [4, true], ['y', true], ['y', false], [4, false],
      [25, true], [25, false]]);
});

test('Mac Option+Backspace and Option+Arrow become Windows Ctrl shortcuts', () => {
  const f = fixture('windows', 'MacIntel');
  f.press('keydown', 'Alt', 'AltLeft');
  for (const key of ['Backspace', 'ArrowRight']) {
    f.press('keydown', key, key, {altKey: true});
    f.press('keyup', key, key, {altKey: true});
  }
  f.press('keyup', 'Alt', 'AltLeft');
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [[1, true], ...[2, 23].flatMap(key =>
      [[1, false], [4, true], [key, true], [key, false], [4, false], [1, true]]), [1, false]]);
});

test('IME Process and changing Shift state preserve the key to release', () => {
  const f = fixture('macos');
  f.press('keydown', 'Process', 'KeyN', { isComposing: true });
  f.press('keyup', 'n', 'KeyN');
  f.press('keydown', 'Shift', 'ShiftRight');
  f.press('keydown', '!', 'Digit1', { shiftKey: true });
  f.press('keyup', 'Shift', 'ShiftRight');
  f.press('keyup', '1', 'Digit1');
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [['n', true], ['n', false], [31, true], ['1', true], [31, false], ['1', false]]);
});

test('keypad digits, decimal and Enter stay distinct from the main keyboard', () => {
  const f = fixture('windows');
  for (const [key, code] of [['1', 'Digit1'], ['1', 'Numpad1'], ['.', 'NumpadDecimal'],
    ['Enter', 'NumpadEnter'], ['Enter', 'Enter'], ['End', 'Numpad1']]) {
    f.press('keydown', key, code);
    f.press('keyup', key, code);
  }
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    ['1', 35, 44, 49, 22, 7].flatMap(key => [[key, true], [key, false]]));
});

test('left/right Meta stay distinct; clipboard shortcuts use the target modifier', () => {
  const mac = fixture('macos');
  mac.press('keydown', 'Meta', 'MetaRight');
  mac.press('keydown', 'v', 'KeyV', { metaKey: true });
  mac.press('keyup', 'v', 'KeyV', { metaKey: true });
  mac.press('keyup', 'Meta', 'MetaRight');
  assert.deepEqual(mac.events.map(e => [e.layout ?? e.control, e.down]),
    [[33, true], ['v', true], ['v', false], [33, false]]);
  for (const [os, key, code, modifier, target] of [
    ['macos', 'Control', 'ControlLeft', 4, 18],
    ['windows', 'Meta', 'OSLeft', 18, 4],
  ]) {
    const f = fixture(os);
    const flags = os === 'macos' ? { ctrlKey: true } : { metaKey: true };
    f.press('keydown', key, code);
    f.press('keydown', 'c', 'KeyC', flags);
    f.press('keyup', 'c', 'KeyC', flags);
    f.press('keyup', key, code);
    const copy = [[target, true], ['c', true], ['c', false], [target, false]];
    assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]), os === 'macos'
      ? [[modifier, true], [modifier, false], ...copy, [modifier, true], [modifier, false]]
      : copy);
  }
});

test('Windows keeps Win tap and Win+R without firing Win for Command+copy', () => {
  const f = fixture('windows');
  f.press('keydown', 'Meta', 'MetaLeft');
  f.press('keyup', 'Meta', 'MetaLeft');
  f.press('keydown', 'Meta', 'OSRight');
  f.press('keydown', 'r', 'KeyR', { metaKey: true });
  f.press('keyup', 'r', 'KeyR', { metaKey: true });
  f.press('keyup', 'Meta', 'OSRight');
  assert.deepEqual(f.events.map(e => [e.layout ?? e.control, e.down]),
    [[18, true], [18, false], [33, true], ['r', true], ['r', false], [33, false]]);
});
