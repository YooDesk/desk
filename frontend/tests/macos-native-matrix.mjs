// 用既有 native 浏览器套件的终端回显和真实 VNC 像素检查验证 Mac WebService。
import { chromium } from 'playwright';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { installSessionCapture, verifyDirectFile } from '../../scripts/split_lab/direct_file_probe.mjs';

const admin = process.env.YOODESK_LAB_ADMIN ?? 'http://127.0.0.1:19446';
const get = async (path) => {
  const r = await fetch(new URL('/api/admin/' + path, admin), {
    headers: { 'X-YooDesk-Admin-Page': '1' },
  });
  if (!r.ok) throw new Error('Connector admin unavailable');
  return r.json();
};
const [settings, summary, account] = await Promise.all(
  ['local-settings', 'summary', 'saas/desktop'].map(get),
);
const sshPassword = Buffer.from(process.env.YOODESK_LAB_SSH_SECRET_B64 ?? '', 'base64').toString('utf8');
const pin = process.env.YOODESK_LAB_SPKI;
const sshFingerprint = process.env.YOODESK_LAB_SSH_FINGERPRINT;
const browser = await chromium.launch({
  channel: 'chromium', headless: true,
  args: pin ? ['--ignore-certificate-errors-spki-list=' + pin] : [],
});
const protocols = process.env.YOODESK_LAB_PROTOCOL ? [process.env.YOODESK_LAB_PROTOCOL] : ['ssh', 'vnc'];
const paths = process.argv.length > 2 ? process.argv.slice(2) : ['lan_direct', 'stun', 'turn', 'websocket'];
const label = 'YooDesk Mac acceptance SSH';
const extras = process.env.YOODESK_LAB_NATIVE_EXTRAS === '1';
let failed = 0;

async function ssh(page, path) {
  if (extras) await installSessionCapture(page);
  const matches = page.locator('#hosts .host-connect').filter({ hasText: label });
  const target = matches.first();
  if (path === 'lan_direct') {
    await page.locator('#connection-new').waitFor({ state: 'visible' });
    await page.locator('#hosts .host-connect[data-target-id="local-ssh"]').waitFor();
    if (!(await matches.count())) {
      await page.click('#connection-new');
      await page.fill('#connection-label', label);
      await page.fill('#connection-host', '127.0.0.1');
      await page.fill('#connection-username', 'coder');
      await page.fill('#connection-fingerprint', sshFingerprint);
      await page.fill('#connection-root', '/Users/coder/YooDesk-macos-lab/artifacts');
      await page.click('#connection-save');
      await target.waitFor();
    }
  }
  if (!(await page.locator('#ssh-challenge').isVisible())) await target.click();
  await page.waitForSelector('#ssh-challenge[open]', { timeout: 12000 });
  if (!(await page.locator('#credential-fields').isVisible()))
    throw new Error('unexpected SSH host key challenge with the pinned fingerprint');
  await page.selectOption('#auth-method', 'password');
  await page.fill('#ssh-password', sshPassword);
  await page.click('#challenge-submit');
  await page.waitForFunction(() => document.querySelector('#status')?.dataset.state === 'active', null, { timeout: 15000 });
  await page.locator('.terminal-pane:not([hidden]) .xterm-helper-textarea').focus();
  await page.keyboard.type("printf '\\131\\117\\117\\104\\105\\123\\113_MAC_SSH_OK\\n'");
  await page.keyboard.press('Enter');
  await page.waitForFunction(() => document.querySelector('#terminals')?.textContent.includes('YOODESK_MAC_SSH_OK'), null, { timeout: 10000 });
  const sftp = extras && (path === 'lan_direct' || path === 'stun')
    ? await verifyDirectFile(page, 16, '', path)
    : undefined;
  return { terminalEcho: true, sftp };
}

async function vnc(page) {
  const local = page.locator('#hosts .host-connect[data-target-id="local-vnc"]');
  await page.waitForFunction(() =>
    document.querySelector('#status')?.dataset.state === 'active' ||
    !!document.querySelector('#hosts .host-connect[data-target-id="local-vnc"]')?.getClientRects().length,
    null, { timeout: 15000 });
  if (await local.isVisible() && (await page.locator('#status').getAttribute('data-state')) === 'idle')
    await local.click();
  await page.waitForFunction(() => document.querySelector('#status')?.dataset.state === 'active', null, { timeout: 20000 });
  const result = await page.waitForFunction(() => {
    const canvas = document.querySelector('#stage canvas');
    if (!canvas || canvas.width !== 1024 || canvas.height !== 768) return false;
    const pixels = canvas.getContext('2d').getImageData(900, 700, 1, 1).data;
    return pixels[0] === 60 && pixels[1] === 119 && pixels[2] === 189
      ? { width: canvas.width, height: canvas.height, realVncPixel: true } : false;
  }, null, { timeout: 12000 });
  const evidence = await result.jsonValue();
  if (extras) {
    const id = crypto.randomUUID().replaceAll('-', '');
    const witness = '/tmp/yoodesk-vnc-' + id;
    const canvas = page.locator('#stage canvas');
    const box = await canvas.boundingBox();
    await page.mouse.click(box.x + 100 * box.width / 1024, box.y + 95 * box.height / 768);
    const wsl = (...args) => execFileSync('wsl.exe', ['-d', 'Ubuntu-22.04', '--', ...args], {
      encoding: 'utf8', timeout: 3000, stdio: ['ignore', 'pipe', 'pipe'],
    }).trim();
    const pointer = `import ctypes as c
x=c.CDLL('libX11.so.6')
x.XOpenDisplay.restype=c.c_void_p
d=c.c_void_p(x.XOpenDisplay(b':78'))
assert d.value
x.XDefaultRootWindow.restype=c.c_ulong
r=c.c_ulong(x.XDefaultRootWindow(d))
rr,cr=c.c_ulong(),c.c_ulong()
rx,ry,wx,wy=c.c_int(),c.c_int(),c.c_int(),c.c_int()
m=c.c_uint()
assert x.XQueryPointer(d,r,c.byref(rr),c.byref(cr),c.byref(rx),c.byref(ry),c.byref(wx),c.byref(wy),c.byref(m))
print(rx.value,ry.value)`;
    const [x, y] = wsl('python3', '-c', pointer).split(' ').map(Number);
    assert(Math.abs(x - 100) <= 4 && Math.abs(y - 95) <= 4, 'VNC mouse did not reach fixture');
    evidence.nativeMouse = true;
    await page.keyboard.type(`printf '%s' vnc-ok > ${witness}`, { delay: 20 });
    await page.keyboard.press('Enter');
    try {
      for (const deadline = Date.now() + 9000; Date.now() < deadline;) {
        let value;
        try { value = wsl('cat', witness); } catch {}
        if (value === 'vnc-ok') {
          evidence.nativeKeyboardText = true;
          evidence.inputWindow = true;
          const outbound = 'vnc-browser-' + id;
          const inbound = 'vnc-native-' + id;
          await page.evaluate((text) => navigator.clipboard.writeText(text), outbound);
          let arrived = false;
          for (const until = Date.now() + 8000; !arrived && Date.now() < until;) {
            try {
              arrived = wsl('env', 'DISPLAY=:78', 'xclip', '-selection', 'clipboard', '-o') === outbound;
            } catch {}
            if (!arrived) await new Promise((resolve) => setTimeout(resolve, 200));
          }
          assert(arrived, 'VNC viewer clipboard did not reach X11');
          execFileSync('wsl.exe', ['-d', 'Ubuntu-22.04', '--', 'env', 'DISPLAY=:78',
            'xclip', '-selection', 'clipboard', '-i'], {
            input: inbound, encoding: 'utf8', timeout: 4000, stdio: ['pipe', 'ignore', 'ignore'],
          });
          await page.waitForFunction(async (text) => (await navigator.clipboard.readText()) === text,
            inbound, { timeout: 10000 });
          evidence.clipboard = true;
          return evidence;
        }
        await new Promise((resolve) => setTimeout(resolve, 200));
      }
      throw new Error('VNC Xterm did not receive keyboard command');
    } finally {
      wsl('rm', '-f', '--', witness);
    }
  }
  return evidence;
}

try {
  for (const protocol of protocols) {
    for (const path of paths) {
      const direct = path === 'lan_direct';
      const url = new URL(protocol + '/', direct ? summary.lanViewerBase : summary.viewerBase);
      if (protocol === 'vnc') {
        url.searchParams.set('viewer', '1');
        url.searchParams.set('target', 'local-vnc');
      }
      if (!direct) {
        url.searchParams.set('device_id', account.session_code);
        url.searchParams.set('transport', path);
      }
      const context = await browser.newContext({
        viewport: { width: 1360, height: 900 },
        permissions: protocol === 'vnc' && extras ? ['clipboard-read', 'clipboard-write'] : [],
      });
      const page = await context.newPage();
      try {
        const response = await page.goto(url.href, { timeout: 25000 });
        assert(response?.ok(), 'entry HTTP ' + response?.status());
        await page.fill('#credential', settings.password, { timeout: 12000 });
        await page.click('#login-form button[type=submit]');
        await page.waitForFunction(() => document.getElementById('login')?.classList.contains('hidden') ||
          !document.body.classList.contains('login-required'), null, { timeout: 20000 });
        const evidence = protocol === 'ssh' ? await ssh(page, path) : await vnc(page);
        const state = await page.evaluate(async () => (await window.__yoodeskTransportReady)?.state ?? null);
        assert.equal(state?.path ?? 'lan_direct', path);
        if (path === 'stun') assert(state.pair?.local && state.pair?.remote && ![state.pair.local, state.pair.remote].includes('relay'));
        if (path === 'turn') assert.deepEqual([state.pair?.local, state.pair?.remote], ['relay', 'relay']);
        console.log(JSON.stringify({ protocol, path, passed: true, ...evidence, transport: state?.pair }));
      } catch (error) {
        failed++;
        const detail = await page.evaluate(() => ({
          status: document.querySelector('#status')?.textContent,
          challenge: document.querySelector('#challenge-error')?.textContent,
          login: document.querySelector('#login-error')?.textContent,
        })).catch(() => null);
        console.log(JSON.stringify({ protocol, path, passed: false, error: error.message.split('\n')[0], detail }));
      } finally {
        await context.close();
      }
    }
  }
} finally {
  await browser.close();
}
process.exitCode = failed ? 1 : 0;
