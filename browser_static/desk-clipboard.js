// 文本/图片自动双向同步。参照原 webviewer 的格式和权限；同步状态不展示剪贴板内容。
window.createDeskClipboard = function ({
  request,
  api,
  state,
  say,
  sendInputBatch,
}) {
  const maxBytes = 16 * 1024 * 1024;
  let generation = 0;
  let session;
  let controller;
  let timer;
  let busy;
  let localFingerprint;
  let remoteFingerprint;
  let pendingRemote;
  let remoteCopy = false;
  let pauseUploadUntil = 0;
  let pastePending = false;
  let lastError = "";
  const diagnostic = () =>
    ((window.__yoodeskDesk ||= {}).clipboardSync ||= {
      localUpdates: 0,
      remoteUpdates: 0,
    });
  const focused = () => !document.hidden && document.hasFocus();
  const valid = (current) =>
    current === generation && session && state.sessionId === session;
  const canUpload = () =>
    !state.readonly && state.capabilities?.clipboard_write;
  const url = () => `${api}/sessions/${session}/clipboard/`;
  function report(error) {
    diagnostic().state = "blocked";
    diagnostic().lastError = error.message;
    if (lastError !== error.message)
      say(`剪贴板同步失败：${error.message}`, true);
    lastError = error.message;
  }
  function base64(bytes) {
    let value = "";
    for (let offset = 0; offset < bytes.length; offset += 16384) {
      value += String.fromCharCode(...bytes.subarray(offset, offset + 16384));
    }
    return btoa(value);
  }
  function rgba(data) {
    const value = atob(data);
    return Uint8ClampedArray.from(value, (character) =>
      character.charCodeAt(0),
    );
  }
  async function fingerprint(content) {
    let bytes;
    if (content.kind === "text") {
      bytes = new TextEncoder().encode(
        `text:${content.text.replace(/\r\n/g, "\n")}`,
      );
    } else {
      const pixels = rgba(content.data);
      const prefix = new TextEncoder().encode(
        `rgba:${content.width}:${content.height}:`,
      );
      bytes = new Uint8Array(prefix.length + pixels.length);
      bytes.set(prefix);
      bytes.set(pixels, prefix.length);
    }
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    return Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join("");
  }
  async function readLocal() {
    if (!navigator.clipboard) throw new Error("浏览器不支持系统剪贴板");
    if (navigator.clipboard.read) {
      const items = await navigator.clipboard.read();
      for (const item of items) {
        if (item.types.includes("image/png")) {
          const blob = await item.getType("image/png");
          const bitmap = await createImageBitmap(blob);
          try {
            if (
              !bitmap.width ||
              !bitmap.height ||
              bitmap.width > 8192 ||
              bitmap.height > 8192 ||
              bitmap.width * bitmap.height * 4 > maxBytes
            ) {
              throw new Error("剪贴板图片超过 16 MiB");
            }
            const canvas = document.createElement("canvas");
            canvas.width = bitmap.width;
            canvas.height = bitmap.height;
            const context = canvas.getContext("2d");
            context.drawImage(bitmap, 0, 0);
            return {
              kind: "image",
              width: canvas.width,
              height: canvas.height,
              data: base64(
                context.getImageData(0, 0, canvas.width, canvas.height).data,
              ),
            };
          } finally {
            bitmap.close();
          }
        }
      }
      for (const item of items) {
        if (item.types.includes("text/plain")) {
          return {
            kind: "text",
            text: await (await item.getType("text/plain")).text(),
          };
        }
      }
      // Chromium 的空剪贴板可能返回一个 types=[] 的 ClipboardItem。
      return items.every((item) => item.types.length === 0)
        ? { kind: "text", text: "" }
        : null;
    }
    if (!navigator.clipboard.readText)
      throw new Error("浏览器不支持读取系统剪贴板");
    return { kind: "text", text: await navigator.clipboard.readText() };
  }
  async function writeLocal(content, current) {
    if (!valid(current) || !focused()) return false;
    if (content.kind === "text") {
      if (!navigator.clipboard?.writeText)
        throw new Error("浏览器不支持写入系统剪贴板");
      const expected = await fingerprint(content);
      if (!valid(current) || !focused()) return false;
      await navigator.clipboard.writeText(content.text);
      return expected;
    }
    if (!navigator.clipboard?.write || !window.ClipboardItem)
      throw new Error("浏览器不支持图片剪贴板");
    const pixels = rgba(content.data);
    if (
      pixels.length > maxBytes ||
      pixels.length !== content.width * content.height * 4
    )
      throw new Error("远端剪贴板图片尺寸无效");
    const canvas = document.createElement("canvas");
    canvas.width = content.width;
    canvas.height = content.height;
    canvas
      .getContext("2d")
      .putImageData(new ImageData(pixels, content.width, content.height), 0, 0);
    const blob = await new Promise((resolve, reject) =>
      canvas.toBlob(
        (result) =>
          result ? resolve(result) : reject(new Error("无法转换剪贴板图片")),
        "image/png",
      ),
    );
    const expected = await fingerprint({
      kind: "image",
      width: canvas.width,
      height: canvas.height,
      data: base64(
        canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height)
          .data,
      ),
    });
    if (!valid(current) || !focused()) return false;
    await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
    return expected;
  }
  async function cycle(current, mode) {
    if (!valid(current)) return;
    let localWritten = false;
    let localError;
    if (
      focused() &&
      canUpload() &&
      mode !== "remote" &&
      Date.now() >= pauseUploadUntil
    ) {
      try {
        const content = await readLocal();
        if (!valid(current) || !focused()) return;
        if (!content && mode === "paste")
          throw new Error("当前剪贴板格式不支持自动同步");
        if (content) {
          if (
            content.kind === "text" &&
            new TextEncoder().encode(content.text).length > maxBytes
          )
            throw new Error("剪贴板文本超过 16 MiB");
          const hash = await fingerprint(content);
          if (!valid(current) || !focused()) return;
          if (hash !== localFingerprint) {
            const initiallyEmpty =
              mode !== "paste" &&
              localFingerprint === undefined &&
              content.kind === "text" &&
              content.text === "";
            if (!initiallyEmpty) {
              await request("POST", url(), content, controller.signal);
              if (!valid(current)) return;
              remoteFingerprint = hash;
              pendingRemote = undefined;
              remoteCopy = false;
              localWritten = true;
              diagnostic().localUpdates += 1;
            }
            localFingerprint = hash;
          }
        }
      } catch (error) {
        if (!valid(current)) return;
        localError = error;
        report(error);
        // 粘贴必须等待本地内容真正同步成功，不能把旧远端剪贴板当作成功。
        if (mode === "paste") throw error;
      }
    }
    if (state.capabilities?.clipboard_read && valid(current)) {
      const content = await request("GET", url(), undefined, controller.signal);
      if (!valid(current)) return;
      if (content && content.changed !== false) {
        const hash = await fingerprint(content);
        if (!valid(current)) return;
        if (hash !== remoteFingerprint) {
          remoteFingerprint = hash;
          pendingRemote = content;
        }
      }
      if (pendingRemote && focused() && !localWritten) {
        const content = pendingRemote;
        const writtenFingerprint = await writeLocal(content, current);
        if (!writtenFingerprint) return;
        if (!valid(current)) return;
        // 只确认刚写入的内容；再次读取系统剪贴板会把用户随后新复制的内容误认为回声。
        localFingerprint = writtenFingerprint;
        if (!valid(current)) return;
        pendingRemote = undefined;
        diagnostic().remoteUpdates += 1;
      }
    }
    if (valid(current) && !localError) {
      diagnostic().state = "active";
      delete diagnostic().lastError;
      lastError = "";
    }
  }
  function sync(mode = "auto") {
    if (!session) return Promise.resolve();
    if (busy) return mode === "paste" ? busy : busy.catch(() => {});
    const current = generation;
    const work = cycle(current, mode).catch((error) => {
      if (valid(current) && error.name !== "AbortError") report(error);
      if (mode === "paste") throw error;
    });
    busy = work;
    work
      .finally(() => {
        if (busy === work) busy = undefined;
      })
      .catch(() => {});
    return work;
  }
  function schedule(current) {
    timer = setTimeout(async () => {
      await sync();
      if (valid(current)) schedule(current);
    }, 750);
  }
  function stop() {
    generation += 1;
    clearTimeout(timer);
    controller?.abort();
    session = undefined;
    busy = undefined;
    localFingerprint = undefined;
    remoteFingerprint = undefined;
    pendingRemote = undefined;
    remoteCopy = false;
    pauseUploadUntil = 0;
    pastePending = false;
    lastError = "";
    diagnostic().state = "stopped";
  }
  function start() {
    stop();
    session = state.sessionId;
    if (!session || !(state.capabilities?.clipboard_read || canUpload()))
      return;
    controller = new AbortController();
    const current = generation;
    void sync();
    schedule(current);
  }
  function copiedRemote() {
    remoteCopy = true;
    pauseUploadUntil = Date.now() + 1500;
    void sync("remote");
  }
  async function paste() {
    if (!canUpload() || !session || !focused() || pastePending) return;
    const current = generation;
    pastePending = true;
    try {
      if (busy) await busy;
      if (!valid(current) || !focused()) return;
      if (!remoteCopy) {
        pauseUploadUntil = 0;
        await sync("paste");
      }
      if (!valid(current) || !focused()) return;
      await sendInputBatch([
        { type: "key", control: 4, down: true },
        { type: "key", layout: "v", down: true },
        { type: "key", layout: "v", down: false },
        { type: "key", control: 4, down: false },
      ]);
    } catch (error) {
      if (valid(current)) report(error);
    } finally {
      if (current === generation) pastePending = false;
    }
  }
  window.addEventListener("focus", () => {
    void sync();
  });
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) void sync();
  });
  document.addEventListener("copy", (event) => {
    if (event.target?.id !== "screen") {
      remoteCopy = false;
      pauseUploadUntil = 0;
    }
  });
  document.addEventListener("cut", (event) => {
    if (event.target?.id !== "screen") {
      remoteCopy = false;
      pauseUploadUntil = 0;
    }
  });
  return { start, stop, sync, copiedRemote, paste };
};
