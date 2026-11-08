import { readFile, writeFile, readdir, rename } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import {
  X509Certificate,
  createHash,
  randomUUID,
  randomBytes,
} from "node:crypto";
import assert from "node:assert/strict";
import path from "node:path";
const [
  configPath,
  reportPath,
  label = "desktop",
  cyclesArg = "20",
  fullArg = "8",
] = process.argv.slice(2);
const config = JSON.parse(await readFile(configPath, "utf8"));
const cert = new X509Certificate(await readFile(config.certificate));
const credential = (await readFile(config.access_key_file, "utf8")).trim();
const base = `${config.public_origin}/${config.product_id}/webclient/`;
const prefix = "yoodesk-stability-" + randomUUID();
const cycles = Number(cyclesArg);
assert.ok(
  Number.isInteger(cycles) && cycles >= 1 && cycles <= 100,
  "cycles must be 1..100",
);
const fullCycles = Number(fullArg);
assert.ok(
  Number.isInteger(fullCycles) && fullCycles >= 1 && fullCycles <= 100,
  "full cycles must be 1..100",
);
const service = path.basename(path.dirname(configPath));
const binary =
  process.platform === "win32"
    ? path.join(
        process.env.ProgramFiles,
        config.product_id + "-WebService",
        service,
        "web-service.exe",
      )
    : `/opt/${config.product_id}/web-service/${service}/web-service`;
const report = {
  label,
  sourceCommit: process.env.YOODESK_TEST_SOURCE_REVISION || "working-tree",
  started: new Date().toISOString(),
  cycles: 0,
  abruptCloses: 0,
  files: 0,
  fileBytes: 0,
  cancelledTransfers: 0,
  clipboardText: 0,
  clipboardImage: 0,
  chat: 0,
  requests: 0,
  errors: [],
  latencies: [],
  samples: [],
  firstFrameMs: [],
  configuredFps: config.fps,
  configuredBitrateBps: config.bitrate_bps,
};
report.validation =
  "mixed operations, full disconnects, native service retirement";
report.binarySha256 = createHash("sha256")
  .update(await readFile(binary))
  .digest("hex");
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const { chromium } = await import(
  process.env.YOODESK_PLAYWRIGHT_MODULE || "playwright"
);
const pin = createHash("sha256")
  .update(cert.publicKey.export({ type: "spki", format: "der" }))
  .digest("base64");
const browser = await chromium.launch({
  headless: true,
  args: [`--ignore-certificate-errors-spki-list=${pin}`],
});
const clients = [];
const paths = new Set();
const fixtureDir = process.env.YOODESK_STABILITY_FIXTURE_DIR;
let fixtureSequence = Date.now();
async function fixture(op, value) {
  const seq = ++fixtureSequence,
    temp = path.join(fixtureDir, `command-${seq}.tmp`);
  await writeFile(temp, JSON.stringify({ seq, op, ...value }), {
    flag: "wx",
    mode: 0o644,
  });
  await rename(temp, path.join(fixtureDir, "command.json"));
  for (let i = 0; i < 60; i++) {
    const r = JSON.parse(
      await readFile(path.join(fixtureDir, "report.json"), "utf8"),
    );
    if (r.seq === seq) {
      assert.equal(r.ok, true, "native clipboard fixture mismatch");
      return;
    }
    await pause(50);
  }
  throw Error("native clipboard fixture timed out");
}
async function resources() {
  if (process.platform === "win32") {
    const raw = execFileSync(
      "powershell.exe",
      [
        "-NoProfile",
        "-Command",
        "$ProgressPreference='SilentlyContinue'; @(Get-Process -Name web-service | ForEach-Object { @{pid=$_.Id;threads=$_.Threads.Count;rssBytes=$_.WorkingSet64} }) | ConvertTo-Json -Compress",
      ],
      { encoding: "utf8" },
    );
    const values = JSON.parse(raw.replace(/^\uFEFF/, ""));
    return Array.isArray(values) ? values : [values];
  }
  const result = [];
  for (const pid of await readdir("/proc")) {
    if (!/^\d+$/.test(pid)) continue;
    try {
      if (
        (await readFile(`/proc/${pid}/comm`, "utf8")).trim() !== "web-service"
      )
        continue;
      const args = (await readFile(`/proc/${pid}/cmdline`, "utf8")).split("\0");
      const role = [
        "--service",
        "--rustdesk-worker",
        "--privileged-worker",
        "--user-input-service",
      ].find((v) => args.includes(v));
      if (!role) continue;
      const status = await readFile(`/proc/${pid}/status`, "utf8");
      let backend;
      if (role === "--rustdesk-worker") {
        const values = (await readFile(`/proc/${pid}/environ`, "utf8")).split(
          "\0",
        );
        const value = (key) =>
          values.find((v) => v.startsWith(key + "="))?.slice(key.length + 1);
        backend = {
          service: value("YOODESK_ENGINE_SERVICE_NAME"),
          drm: value("YOODESK_ENGINE_DRM_ENABLED"),
          displayServer: value("RUSTDESK_FORCED_DISPLAY_SERVER"),
        };
      }
      result.push({
        pid: Number(pid),
        ppid: Number(status.match(/^PPid:\s+(\d+)/m)[1]),
        role,
        backend,
        threads: Number(status.match(/^Threads:\s+(\d+)/m)[1]),
        rssBytes: Number(status.match(/^VmRSS:\s+(\d+)/m)[1]) * 1024,
      });
    } catch (e) {
      if (e.code !== "ENOENT") throw e;
    }
  }
  return result;
}
async function client(existingContext, authenticate = true) {
  const started = Date.now();
  const context = existingContext || (await browser.newContext());
  const page = await context.newPage();
  const c = { context, page, shared: !!existingContext };
  clients.push(c);
  page.on("pageerror", (e) => report.errors.push(e.message));
  await page.route("**/clipboard/", (r) =>
    r.request().method() === "GET" && !r.request().headers()["x-yoodesk-probe"]
      ? r.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ v: 2, data: { changed: false } }),
        })
      : r.continue(),
  );
  await page.goto(base + "desk/");
  await page.evaluate(() => {
    const m = document.getElementById("mode");
    m.value = "control";
    m.dispatchEvent(new Event("change"));
  });
  if (authenticate) {
    await page.fill("#credential", credential);
    const login = page.waitForResponse(
      (r) =>
        r.url().endsWith("/access/v2/login/") &&
        r.request().method() === "POST",
    );
    await page.click("#login-form button");
    assert.equal((await login).status(), 200);
  }
  try {
    await page.waitForFunction(
      () => window.__yoodeskDesk?.decodedFrames >= 1,
      null,
      { timeout: 30000 },
    );
  } catch (e) {
    c.id = await page.evaluate(() => window.__yoodeskDesk?.sessionId);
    const diagnostic = await page.evaluate(() => ({
      message: document.getElementById("message").textContent,
      status: document.getElementById("status").dataset.state,
      frames: window.__yoodeskDesk?.decodedFrames,
    }));
    throw Error("first frame: " + JSON.stringify(diagnostic));
  }
  report.firstFrameMs.push(Date.now() - started);
  c.id = await page.evaluate(() => window.__yoodeskDesk.sessionId);
  assert.ok(c.id);
  return c;
}
async function request(c, suffix, body, method = "POST", expected = 200) {
  const start = Date.now();
  const out = await c.page.evaluate(
    async ({ base, suffix, body, method, id }) => {
      const meta = await fetch(base + "access/v2/session/");
      const v = (await meta.json()).data;
      const r = await fetch(base + `desk/api/v2/sessions/${id}/` + suffix, {
        method,
        headers: {
          "X-YooDesk-Probe": "1",
          "Content-Type": "application/json",
          "X-CSRF-Token": v.csrf_token,
          "X-Access-Instance": meta.headers.get("x-access-instance"),
          "X-Request-ID": crypto.randomUUID(),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      const raw = await r.text();
      return { status: r.status, value: raw ? JSON.parse(raw) : null };
    },
    { base, suffix, body, method, id: c.id },
  );
  report.requests++;
  report.latencies.push(Date.now() - start);
  assert.equal(
    out.status,
    expected,
    `${suffix} ${body?.action || ""}: ${JSON.stringify(out)}`,
  );
  return out.value?.data;
}
const endpoint = (c, v, expected = 200) =>
  request(c, "endpoint/", v, "POST", expected);
async function close(c, abrupt = false) {
  if (abrupt) {
    await c.page.route("**/sessions/*/", (r) =>
      r.request().method() === "DELETE" ? r.abort() : r.continue(),
    );
    await c.page.close();
    report.abruptCloses++;
  } else {
    if (c.id) await request(c, "", undefined, "DELETE", 204);
    await c.page.close();
  }
  c.closed = true;
  if (c.shared) return;
  await logout(c.context);
}
async function logout(context) {
  // 用同一cookie上下文单独退出访问身份；不重建已关闭的桌面会话。
  const cleanup = await context.newPage();
  await cleanup.goto(base + "access/v2/session/");
  await cleanup.evaluate(async (base) => {
    const r = await fetch(base + "access/v2/session/");
    if (!r.ok) return;
    const v = (await r.json()).data;
    await fetch(base + "access/v2/logout/", {
      method: "POST",
      headers: {
        "X-CSRF-Token": v.csrf_token,
        "X-Access-Instance": r.headers.get("x-access-instance"),
        "X-Request-ID": crypto.randomUUID(),
      },
    });
  }, base);
  await context.close();
}
async function files(c) {
  for (let n = 0; n < Math.max(2, Math.ceil(cycles / 2)); n++) {
    const path = `${prefix}-${n}.bin`,
      bytes = randomBytes(2 * 1024 * 1024);
    paths.add(path);
    const up = await endpoint(c, {
      action: "files.upload",
      path,
      size: bytes.length,
    });
    for (let offset = 0; offset < bytes.length; offset += 131072)
      await endpoint(c, {
        action: "files.write",
        transfer_id: up.transfer_id,
        offset,
        data: bytes.subarray(offset, offset + 131072).toString("base64"),
      });
    const committed = await endpoint(c, {
      action: "files.commit",
      transfer_id: up.transfer_id,
    });
    assert.equal(committed.name, path);
    const down = await endpoint(c, { action: "files.download", path });
    let offset = 0;
    const hash = createHash("sha256");
    while (offset < bytes.length) {
      const block = await endpoint(c, {
        action: "files.read",
        transfer_id: down.transfer_id,
        offset,
      });
      const data = Buffer.from(block.data, "base64");
      assert.ok(data.length);
      hash.update(data);
      offset += data.length;
      assert.equal(block.offset, offset);
    }
    assert.equal(
      hash.digest("hex"),
      createHash("sha256").update(bytes).digest("hex"),
    );
    await endpoint(c, { action: "files.delete", path, kind: "file" });
    paths.delete(path);
    report.files++;
    report.fileBytes += bytes.length;
    const cancel = await endpoint(c, {
      action: "files.upload",
      path,
      size: bytes.length,
    });
    await endpoint(c, {
      action: "files.write",
      transfer_id: cancel.transfer_id,
      offset: 0,
      data: bytes.subarray(0, 131072).toString("base64"),
    });
    await endpoint(c, {
      action: "files.cancel",
      transfer_id: cancel.transfer_id,
    });
    report.cancelledTransfers++;
  }
}
async function clipboard(c) {
  for (let n = 0; n < cycles * 4; n++) {
    const pixels = randomBytes(64 * 64 * 4);
    for (let i = 3; i < pixels.length; i += 4) pixels[i] = 255;
    const value =
      n % 2
        ? { kind: "text", text: `${prefix}-中文-${n}` }
        : {
            kind: "image",
            width: 64,
            height: 64,
            data: pixels.toString("base64"),
          };
    await request(c, "clipboard/", value);
    const read = await request(c, "clipboard/", undefined, "GET");
    assert.ok(
      read.kind === value.kind &&
        (value.kind === "text"
          ? read.text === value.text
          : read.width === value.width &&
            read.height === value.height &&
            read.data === value.data),
      "clipboard content mismatch",
    );
    if (fixtureDir) {
      const start = Date.now();
      await fixture(
        value.kind === "text" ? "verify-text" : "verify-image",
        value.kind === "text"
          ? { text: value.text }
          : {
              width: value.width,
              height: value.height,
              sha256: createHash("sha256").update(pixels).digest("hex"),
            },
      );
      const native =
        value.kind === "text"
          ? { kind: "text", text: `${prefix}-native-${n}` }
          : {
              ...value,
              data: Buffer.from(pixels)
                .fill(pixels[0] ^ 1, 0, 1)
                .toString("base64"),
            };
      assert.notDeepEqual(
        native,
        value,
        "native fixture must change clipboard content",
      );
      await fixture(
        native.kind === "text" ? "set-text-value" : "set-image-value",
        native,
      );
      let matched = false;
      const observations = [];
      for (let i = 0; i < 30; i++) {
        const observed = await request(c, "clipboard/", undefined, "GET");
        matched =
          observed.kind === native.kind &&
          (native.kind === "text"
            ? observed.text === native.text
            : observed.data === native.data);
        observations.push({
          kind: observed?.kind,
          changed: observed?.changed,
          matched,
          width: observed?.width,
          height: observed?.height,
        });
        if (matched) break;
        await pause(100);
      }
      if (!matched)
        report.clipboardFailure = {
          cycle: n,
          expectedKind: native.kind,
          observations,
        };
      assert.ok(matched, "native-to-browser clipboard mismatch");
      report.nativeClipboardRoundtrips =
        (report.nativeClipboardRoundtrips || 0) + 1;
      (report.nativeClipboardRoundtripMs ||= []).push(Date.now() - start);
    }
    if (value.kind === "text") report.clipboardText++;
    else report.clipboardImage++;
    await pause(50);
  }
}
async function chat(c) {
  let after = 0;
  for (let n = 0; n < cycles * 5; n++) {
    const id = randomUUID(),
      text = `${prefix}-chat-${n}`;
    const sent = await endpoint(c, {
      action: "chat.send",
      message_id: id,
      text,
    });
    const again = await endpoint(c, {
      action: "chat.send",
      message_id: id,
      text,
    });
    assert.deepEqual(sent, again);
    const read = await endpoint(c, { action: "chat.read", after });
    assert.ok(
      read.items.some(
        (v) => v.message_id === id || v.id === id || v.text === text,
      ),
    );
    after = sent.message.sequence;
    report.chat++;
    await pause(120);
  }
}
let failure,
  timer,
  originalClipboard,
  churnContext,
  phase = "baseline";
try {
  report.backendEvidence = await resources();
  report.actualBackend = "windows-native";
  if (process.platform !== "win32") {
    const worker = report.backendEvidence.find(
      (v) => v.role === "--rustdesk-worker" && v.backend?.service === service,
    );
    const helper =
      worker &&
      report.backendEvidence.some(
        (v) => v.role === "--privileged-worker" && v.ppid === worker.ppid,
      );
    report.actualBackend =
      worker?.backend.displayServer === "x11"
        ? "x11"
        : worker?.backend.drm === "1" && helper
          ? "drm"
          : worker?.backend.drm === "0" &&
              !helper &&
              worker?.backend.displayServer === "wayland"
            ? "portal"
            : "unknown";
    if (label.startsWith("x11"))
      assert.equal(
        report.actualBackend,
        "x11",
        "requested label does not match the running backend",
      );
    if (label.startsWith("drm"))
      assert.equal(
        report.actualBackend,
        "drm",
        "requested label does not match the running backend",
      );
    if (label.startsWith("portal"))
      assert.equal(
        report.actualBackend,
        "portal",
        "requested label does not match the running backend",
      );
  }
  const anchor = await client(),
    work = await client();
  if (fixtureDir) {
    const point = await work.page.evaluate(() => ({
      x: Math.round(document.getElementById("screen").width * 0.5),
      y: Math.round(document.getElementById("screen").height * 0.8),
    }));
    for (const mask of [0, 9, 10])
      await request(work, "input/", { type: "mouse", mask, ...point });
  }
  originalClipboard = await request(work, "clipboard/", undefined, "GET");
  report.phaseResources = { baseline: await resources() };
  timer = setInterval(
    () =>
      anchor.page
        .evaluate(() => {
          const d = window.__yoodeskDesk;
          return {
            at: Date.now(),
            fps: d.fps,
            bitrateMbps: d.bitrateMbps,
            rttMs: d.rttMs,
            decodeMs: d.decodeMs,
            decodedFrames: d.decodedFrames,
          };
        })
        .then((v) => report.samples.push({ ...v, phase }))
        .catch(() => {}),
    1000,
  );
  await pause(10000);
  phase = "mixed";
  report.phaseResources.mixed = await resources();
  churnContext = await browser.newContext();
  const tasks = [
    files(work),
    clipboard(work),
    chat(work),
    (async () => {
      for (let n = 0; n < Number(cyclesArg); n++) {
        const c = await client(churnContext, n === 0);
        await pause(150);
        await close(c, n % 2 === 1);
        report.cycles++;
        const active = await anchor.page.evaluate(() => ({
          frames: window.__yoodeskDesk.decodedFrames,
          state: document.getElementById("status").dataset.state,
        }));
        assert.equal(active.state, "active");
        report.anchorFrames = active.frames;
        await pause(100);
      }
    })(),
  ];
  const settled = await Promise.allSettled(tasks);
  for (let i = 0; i < settled.length; i++)
    if (settled[i].status === "rejected")
      report.errors.push(
        ["files", "clipboard", "chat", "reconnect"][i] +
          ": " +
          settled[i].reason.message,
      );
  assert.deepEqual(report.errors, []);
  assert.deepEqual(report.errors, []);
  report.passed = true;
  phase = "recovery";
  report.phaseResources.recovery = await resources();
  await pause(10000);
  clearInterval(timer);
  if (["text", "image"].includes(originalClipboard?.kind))
    await request(work, "clipboard/", originalClipboard);
  await close(work);
  await close(anchor);
  await pause(1000);
  report.idleBefore = await resources();
  report.fullDisconnectCycles = 0;
  report.idleSamples = [];
  for (let n = 0; n < fullCycles; n++) {
    const c = await client(churnContext, false);
    await pause(100);
    await close(c, n % 2 === 1);
    report.fullDisconnectCycles++;
    if ((n + 1) % 4 === 0) {
      await pause(250);
      report.idleSamples.push({ cycle: n + 1, processes: await resources() });
    }
  }
  await pause(1000);
  report.idleAfter = await resources();
  for (const before of report.idleBefore) {
    const after = report.idleAfter.find((v) => v.pid === before.pid);
    assert.ok(after, "desktop process unexpectedly restarted");
    assert.ok(
      after.threads <= before.threads + 4,
      `native threads grew from ${before.threads} to ${after.threads}`,
    );
  }
} catch (e) {
  failure = e;
  report.passed = false;
  report.errors.push(e.message);
} finally {
  clearInterval(timer);
  const alive = clients.find((c) => !c.closed && !c.page.isClosed());
  if (alive && ["text", "image"].includes(originalClipboard?.kind))
    await request(alive, "clipboard/", originalClipboard).catch(() => {});
  if (alive)
    for (const path of paths)
      await endpoint(alive, {
        action: "files.delete",
        path,
        kind: "file",
      }).catch(() => {});
  for (const c of clients)
    if (!c.closed)
      await close(c).catch((e) => report.errors.push("cleanup: " + e.message));
  if (churnContext)
    await logout(churnContext).catch((e) =>
      report.errors.push("cleanup churn: " + e.message),
    );
  await browser.close();
  report.finished = new Date().toISOString();
  const sorted = report.latencies.sort((a, b) => a - b);
  report.requestP95Ms = sorted[Math.floor(sorted.length * 0.95)];
  delete report.latencies;
  for (const metric of ["fps", "bitrateMbps", "rttMs", "decodeMs"]) {
    const values = report.samples
      .map((v) => v[metric])
      .filter(Number.isFinite)
      .sort((a, b) => a - b);
    report[metric] = {
      samples: values.length,
      min: values[0],
      median: values[Math.floor(values.length / 2)],
      p95: values[Math.floor(values.length * 0.95)],
      max: values.at(-1),
    };
  }
  report.phases = {};
  for (const phase of ["baseline", "mixed", "recovery"]) {
    report.phases[phase] = {};
    for (const metric of ["fps", "bitrateMbps", "rttMs", "decodeMs"]) {
      const v = report.samples
        .filter((s) => s.phase === phase)
        .map((s) => s[metric])
        .filter(Number.isFinite)
        .sort((a, b) => a - b);
      report.phases[phase][metric] = {
        samples: v.length,
        median: v[Math.floor(v.length / 2)],
        p95: v[Math.floor(v.length * 0.95)],
      };
    }
  }
  if (report.errors.length) report.passed = false;
  await writeFile(reportPath, JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ ...report, samples: report.samples.length }));
}
if (failure) process.exitCode = 1;
