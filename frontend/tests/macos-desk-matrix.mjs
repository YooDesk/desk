// 实机验证：借用已有 Desk 浏览器套件的登录、解码和所选路径证据。
// 输入/键位用例需要先构建见证窗口（仅目标 Mac 上有意义）：
//   swiftc -O scripts/macos_desk_witness.swift -o /tmp/DeskWitness
//   在 /Users/coder/YooDesk-macos-lab/artifacts/macos-desk-witness/ 下组装
//   DeskWitness.app/Contents/{MacOS/DeskWitness,Info.plist}（CFBundleIdentifier 任意）。
// 该目录名来自原单体仓库的验收记录，路径不同时需同步修改本文件的 root 常量。
import { chromium } from 'playwright';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';

const admin = process.env.YOODESK_LAB_ADMIN ?? 'http://127.0.0.1:19446';
const readAdmin = async (endpoint) => {
  const res = await fetch(new URL('/api/admin/' + endpoint, admin), {
    headers: { 'X-YooDesk-Admin-Page': '1' },
  });
  if (!res.ok) throw new Error('Connector admin unavailable');
  return res.json();
};
const [settings, summary, account] = await Promise.all(
  ['local-settings', 'summary', 'saas/desktop'].map(readAdmin),
);
if (!settings.password || !summary.lanViewerBase || !account.session_code)
  throw new Error('Expected a bound macOS lab Connector with remote access enabled');
const browser = await chromium.launch({
  channel: 'chromium',
  headless: true,
  args: process.env.YOODESK_LAB_SPKI
    ? ['--ignore-certificate-errors-spki-list=' + process.env.YOODESK_LAB_SPKI]
    : [],
});
let failures = 0;
const extras = process.env.YOODESK_LAB_DESK_EXTRAS === '1';
const files = process.env.YOODESK_LAB_DESK_FILES === '1';
const input = process.env.YOODESK_LAB_DESK_INPUT === '1';
const pinyin = process.env.YOODESK_LAB_DESK_PINYIN === '1';
const keymap = process.env.YOODESK_LAB_DESK_KEYMAP === '1';
const caps = process.env.YOODESK_LAB_DESK_CAPS === '1';
const mac = process.env.YOODESK_LAB_MAC_SSH ?? 'yoodesk-mac';
const marker = (part) => 'YooDesk Mac ' + part + ' ' + crypto.randomUUID() + ' 中文';
const ssh = (...args) => execFileSync('ssh', ['-o', 'BatchMode=yes', mac, ...args], {
  encoding: 'utf8', timeout: 5000,
}).trim();
const clipboard = (command, input) =>
  execFileSync('ssh', ['-o', 'BatchMode=yes', mac, '/usr/bin/env', 'LC_ALL=en_US.UTF-8', '/usr/bin/' + command], {
    input, encoding: 'utf8', timeout: 5000,
  });

async function verifyClipboard(page) {
  const previous = clipboard('pbpaste');
  const send = marker('viewer clipboard');
  const receive = marker('native clipboard');
  try {
    await page.evaluate(async ({ send }) => {
      const { csrf, sessionId } = window.__yoodeskDesk;
      const r = await fetch(`/yoodesk/webclient/desk/api/v2/sessions/${sessionId}/clipboard/`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf, 'X-Request-ID': crypto.randomUUID() },
        body: JSON.stringify({ kind: 'text', text: send }),
      });
      if (!r.ok) throw new Error('Clipboard write HTTP ' + r.status);
    }, { send });
    assert.equal(clipboard('pbpaste'), send, 'Mac did not receive clipboard text');
    clipboard('pbcopy', receive);
    await page.waitForFunction(async (text) => {
      const { sessionId } = window.__yoodeskDesk;
      const r = await fetch(`/yoodesk/webclient/desk/api/v2/sessions/${sessionId}/clipboard/`);
      return r.ok && (await r.json()).data?.text === text;
    }, receive, { timeout: 10000 });
  } finally {
    clipboard('pbcopy', previous);
  }
}

async function verifyInput(page) {
  const id = 'yoodesk' + crypto.randomUUID().replaceAll('-', '');
  const text = pinyin ? '你好' : caps ? 'aBCd' : id;
  const root = '/Users/coder/YooDesk-macos-lab/artifacts/macos-desk-witness/';
  const result = root + id;
  const mode = keymap ? ['--keymap'] : pinyin ? ['--pinyin'] : [];
  ssh('/usr/bin/open', '-n', root + 'DeskWitness.app', '--args', result, text, ...mode);
  async function until(check, expected) {
    let last = '';
    for (const deadline = Date.now() + 10000; Date.now() < deadline;) {
      const state = ssh('/bin/test', '-f', result, '&&', '/bin/cat', result, '||', 'true');
      last = state;
      if (state === 'input-source-unavailable') throw new Error('macOS test input source is not enabled: ' + (pinyin ? 'Simplified Pinyin' : 'ABC'));
      if (check(state)) return state;
      await new Promise((resolve) => setTimeout(resolve, 180));
    }
    throw new Error('native macOS input witness did not reach ' + expected + ': ' + last);
  }
  try {
    const state = await until((s) => /^ready \d+ \d+$/.test(s), 'ready');
    if (keymap) {
      await page.waitForFunction(() => document.activeElement?.id === 'screen', null, { timeout: 4000 });
      // Playwright's NumLock-off aliases produce End/Delete for these two keys.
      const cdp = await page.context().newCDPSession(page);
      for (const [code, key, windowsVirtualKeyCode] of [['Numpad1', '1', 97], ['NumpadDecimal', '.', 110]]) {
        for (const type of ['keyDown', 'keyUp']) {
          await cdp.send('Input.dispatchKeyEvent', { type, code, key, windowsVirtualKeyCode, isKeypad: true });
        }
      }
      await cdp.detach();
      for (const key of ['NumpadAdd', 'NumpadEnter', 'MetaLeft', 'MetaRight', 'Control+c', 'Control+c', 'Control+v']) {
        await page.keyboard.press(key, { delay: 50 });
      }
      await until((s) => s === 'keymap-ok', 'keymap-ok');
      return { nativeKeypad: true, nativeLeftAndRightCommand: true, nativeEditShortcuts: true, nativeRawControlCopy: true };
    }
    const [, x, y] = state.split(' ').map(Number);
    const canvas = page.locator('#screen');
    const box = await canvas.boundingBox();
    const size = await canvas.evaluate((c) => ({ width: c.width, height: c.height }));
    await page.mouse.click(box.x + x * box.width / size.width, box.y + y * box.height / size.height,
      { delay: 50 });
    await until((s) => s === 'mouse-ok', 'mouse-ok');
    if (caps) {
      await page.keyboard.press('KeyA');
      await page.keyboard.press('CapsLock');
      const cdp = await page.context().newCDPSession(page);
      // Chromium's CDP does not expose a Caps Lock modifier bit. Supply the same
      // uppercase key with no Shift that a physical Caps Lock produces.
      for (const key of ['B', 'C']) {
        for (const type of ['keyDown', 'keyUp']) {
          await cdp.send('Input.dispatchKeyEvent', { type, code: 'Key' + key, key, windowsVirtualKeyCode: key.charCodeAt(0) });
        }
      }
      await cdp.detach();
      await page.keyboard.press('CapsLock');
      await page.keyboard.press('KeyD');
    } else if (pinyin) {
      for (const key of 'nihao') await page.keyboard.press('Key' + key.toUpperCase(), { delay: 40 });
      await page.keyboard.press('Space');
    } else {
      await page.keyboard.type(text, { delay: 15 });
    }
    await until((s) => s === 'both-ok', 'both-ok');
    return { nativeMouse: true, nativeKeyboardText: true, ...(pinyin ? { nativePinyin: true } : {}),
      ...(caps ? { nativeCapsCasing: true } : {}) };
  } finally {
    ssh('/bin/rm', '-f', '--', result);
  }
}

async function verifyChat(page, context) {
  const local = await context.newPage();
  try {
    await local.goto('http://127.0.0.1:30445/yoodesk/webadmin/', { timeout: 8000 });
    const outgoing = marker('viewer chat');
    const reply = marker('Mac chat');
    await page.bringToFront();
    await page.locator('#toolbar').hover();
    await page.click('#toolbar-pin');
    await page.click('#chat');
    await page.fill('#chat-input', outgoing);
    await page.click('#chat-send');
    let room;
    for (const deadline = Date.now() + 12000; !room && Date.now() < deadline;) {
      room = await local.evaluate(async (text) => {
        const r = await fetch('/yoodesk/webadmin/api/v2/chat/');
        if (!r.ok) return null;
        return (await r.json()).data?.rooms?.find((candidate) =>
          candidate.history.items.some((message) => message.text === text),
        )?.conversation_id ?? null;
      }, outgoing);
      if (!room) await new Promise((resolve) => setTimeout(resolve, 200));
    }
    assert.equal(typeof room, 'string', 'Mac admin did not receive viewer chat');
    await local.selectOption('#conversation', room);
    await local.fill('#chat-input', reply);
    await local.click('#chat-send');
    await page.waitForFunction((text) => document.querySelector('#chat-log')?.textContent.includes(text), reply,
      { timeout: 12000 });
  } finally {
    await local.close();
  }
}

// 按既有 Desk 文件验收规格：64 个 4 KiB 文件，四路并发；直连 128 MiB 双向。
async function verifyFiles(page, large) {
  return page.evaluate(async ({ large }) => {
    const { csrf, sessionId } = window.__yoodeskDesk;
    const url = `/yoodesk/webclient/desk/api/v2/sessions/${sessionId}/endpoint/`;
    const invoke = async (action, fields = {}) => {
      const r = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf, 'X-Request-ID': crypto.randomUUID() },
        body: JSON.stringify({ action, ...fields }),
      });
      const body = await r.json();
      if (!r.ok) throw new Error(`${action}: HTTP ${r.status}: ${body.error?.message ?? ''}`);
      return body.data;
    };
    const root_id = 'system';
    const dir = 'Users/coder/YooDesk-macos-lab/artifacts/browser-files-' + crypto.randomUUID();
    await invoke('files.create', { root_id, path: dir, kind: 'directory' });
    const hash = async (data) => new Uint8Array(await crypto.subtle.digest('SHA-256', data)).toString();
    const transfer = async (name, size) => {
      const item = { root_id, path: dir + '/' + name };
      let write, read, committed = false;
      try {
        write = (await invoke('files.upload', { ...item, size })).transfer_id;
        const source = new Uint8Array(size);
        for (let offset = 0; offset < size; offset += 65536) {
          const chunk = source.subarray(offset, offset + 65536);
          crypto.getRandomValues(chunk);
          const result = await invoke('files.write', {
            transfer_id: write, offset, data: chunk.toBase64(),
          });
          if (result.offset !== offset + chunk.length) throw new Error('upload offset mismatch');
        }
        const expected = await hash(source);
        const commit = await invoke('files.commit', { transfer_id: write });
        committed = true;
        if (commit.name !== name) throw new Error('upload name changed');
        const download = await invoke('files.download', item);
        if (download.size !== size) throw new Error('download size mismatch');
        read = download.transfer_id;
        const received = new Uint8Array(size);
        let offset = 0;
        while (true) {
          const packet = await invoke('files.read', { transfer_id: read, offset });
          const chunk = Uint8Array.fromBase64(packet.data);
          received.set(chunk, offset);
          offset += chunk.length;
          if (offset !== packet.offset) throw new Error('download offset mismatch');
          if (packet.done) break;
          if (!chunk.length) throw new Error('empty download chunk');
        }
        read = undefined;
        if (offset !== size || await hash(received) !== expected) throw new Error('file SHA-256 mismatch');
      } finally {
        if (write && !committed) await invoke('files.cancel', { transfer_id: write }).catch(() => {});
        if (read) await invoke('files.cancel', { transfer_id: read }).catch(() => {});
        if (committed) await invoke('files.delete', { ...item, kind: 'file' });
      }
    };
    try {
      for (let batch = 0; batch < 64; batch += 4) {
        const results = await Promise.allSettled(Array.from({ length: 4 }, (_, i) =>
          transfer('small-' + (batch + i) + '.bin', 4096),
        ));
        const failed = results.find(({ status }) => status === 'rejected');
        if (failed) throw failed.reason;
      }
      const largeBytes = large ? 128 * 1024 * 1024 : 0;
      if (largeBytes) await transfer('large.bin', largeBytes);
      return { smallCount: 64, concurrent: 4, largeBytesEachDirection: largeBytes, sha256: true };
    } finally {
      await invoke('files.delete', { root_id, path: dir, kind: 'directory' });
    }
  }, { large });
}
try {
  for (const path of process.argv.slice(2).length
    ? process.argv.slice(2)
    : ['lan_direct', 'stun', 'turn', 'websocket']) {
    const direct = path === 'lan_direct';
    const entry = new URL('desk/', direct ? summary.lanViewerBase : summary.viewerBase);
    if (!direct) {
      entry.searchParams.set('device_id', account.session_code);
      entry.searchParams.set('transport', path);
    }
    const context = await browser.newContext({ viewport: { width: 1360, height: 900 } });
    const page = await context.newPage();
    const inputErrors = [];
    page.on('pageerror', error => inputErrors.push(error.message));
    page.on('response', response => {
      if (response.url().endsWith('/input/') && !response.ok()) inputErrors.push('input HTTP ' + response.status());
    });
    let phase = 'entry';
    try {
      const response = await page.goto(entry.href, { timeout: 30000 });
      if (!response?.ok()) throw new Error('entry HTTP ' + response?.status());
      phase = 'login';
      await page.fill('#credential', settings.password, { timeout: 15000 });
      await page.click('#login-form button[type=submit]');
      phase = 'video';
      await page.waitForFunction(() => window.__yoodeskDesk?.decodedFrames > 1, null, {
        timeout: 35000,
      });
      const evidence = await page.evaluate(async () => {
        const desk = window.__yoodeskDesk;
        const transport = (await window.__yoodeskTransportReady)?.state;
        return {
          decodedFrames: desk.decodedFrames,
          readOnly: desk.readOnly,
          transport: transport ? { path: transport.path, pair: transport.pair } : null,
        };
      });
      if (input && evidence.readOnly) throw new Error('Desk session is read-only; cannot verify native input');
      if (direct ? evidence.transport !== null : evidence.transport?.path !== path)
        throw new Error('actual transport differs from requested transport');
      const pair = evidence.transport?.pair;
      if (path === 'turn' && (pair?.local !== 'relay' || pair?.remote !== 'relay'))
        throw new Error('TURN pair is not relay/relay');
      if (path === 'stun' && (!pair?.local || !pair.remote || [pair.local, pair.remote].includes('relay')))
        throw new Error('STUN selected pair is not direct');
      if (extras) {
        const extraFailures = {};
        for (const [name, verify] of Object.entries({ clipboard: verifyClipboard, chat: verifyChat })) {
          try {
            await verify(page, context);
            evidence[name] = true;
          } catch (error) {
            evidence[name] = false;
            extraFailures[name] = error.message.split('\n')[0];
          }
        }
        if (Object.keys(extraFailures).length) {
          failures++;
          console.log(JSON.stringify({ path, passed: false, ...evidence, extraFailures }));
          continue;
        }
      }
      if (files) {
        phase = 'files';
        evidence.files = await verifyFiles(page, direct || path === 'stun');
      }
      if (input) {
        phase = 'input';
        evidence.input = await verifyInput(page);
      }
      console.log(JSON.stringify({ path, passed: true, ...evidence }));
    } catch (error) {
      failures++;
      const status = await page.evaluate(() => ({
        transport: document.documentElement.dataset.transportState,
        decodedFrames: window.__yoodeskDesk?.decodedFrames ?? 0,
        hint: [...document.querySelectorAll('[role="alert"], #login-error, #error')]
          .filter((e) => e.getClientRects().length > 0)
          .map((e) => e.textContent.trim())
          .filter(Boolean)
          .join(' ').slice(0, 240),
      })).catch(() => null);
      console.log(JSON.stringify({ path, passed: false, phase, error: error.message.split('\n')[0], status, inputErrors }));
    } finally {
      await context.close();
    }
  }
} finally {
  await browser.close();
}
process.exitCode = failures ? 1 : 0;
