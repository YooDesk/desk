// 文件导航与有界传输队列。每项绑定选中文件时的目录、会话和输入代次。
window.createDeskFiles = function ({ request, api, state }) {
  const el = (id) => document.getElementById(id);
  const diagnostic = () => (window.__yoodeskDesk ||= {});
  const MAX_PENDING = 32;
  const MAX_HISTORY = 40;
  const MAX_FILE = 1024 ** 3;
  const CHUNK = 65536;
  const tasks = [];
  let active;
  let running = false;
  let supported = false;
  let directory = "";
  let rootId = "default";
  let currentPath = "";
  let windows = false;
  let entries = [];
  let listingTruncated = false;
  let listingVersion = 0;
  let loading = false;
  let cleanupBlocked = false;
  let operation;
  const objectUrls = new Map();
  const pending = (task) =>
    !["completed", "failed", "cancelled"].includes(task.status);
  const allowed = () =>
    supported && state.sessionId && !state.readonly && !state.modePending;
  const endpoint = (body, session = state.sessionId) =>
    request("POST", `${api}/sessions/${session}/endpoint/`, body);
  const join = (base, name) => [base, name].filter(Boolean).join("/");
  const bytesText = (value) => {
    const units = ["B", "KiB", "MiB", "GiB"];
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return `${unit ? value.toFixed(value < 10 ? 1 : 0) : value} ${units[unit]}`;
  };
  const errorText = (error) => {
    if (error.message.includes("file already exists"))
      return "同名文件已存在，请改名后重新选择";
    if (error.status === 413) return "单个文件不能超过 1 GiB";
    if (error.message.includes("parent directory not found"))
      return "目标目录已不存在，请刷新目录";
    return error.message;
  };
  function showError(message = "") {
    el("file-error").textContent = message;
  }
  function make(tag, text, className) {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    if (className) node.className = className;
    return node;
  }
  function button(text, label, handler) {
    const node = make("button", text);
    node.type = "button";
    if (label) node.setAttribute("aria-label", label);
    node.addEventListener("click", handler);
    return node;
  }
  function updateAvailability() {
    const disabled = !allowed() || cleanupBlocked;
    el("file-upload").disabled = disabled || rootId === null;
    el("file-drop").classList.toggle("disabled", disabled || rootId === null);
    el("file-refresh").disabled = disabled || loading;
    el("file-default").disabled = disabled || loading;
    el("file-new").disabled = el("file-mkdir").disabled =
      disabled || loading || rootId === null;
    for (const control of el("file-list").querySelectorAll("button"))
      control.disabled = disabled || loading;
    for (const crumb of el("file-path").children) {
      if (crumb.tagName === "BUTTON") crumb.disabled = disabled || loading;
    }
  }
  function renderLocation() {
    const location = el("file-path");
    location.dataset.path = currentPath;
    location.title = currentPath;
    location.replaceChildren();
    const crumb = (label, fullPath, destination, selectedRoot, current) => {
      if (location.children.length)
        location.append(make("span", "›", "file-path-separator"));
      const node = button(label, `转到 ${fullPath}`, () =>
        listFiles(destination, selectedRoot),
      );
      node.title = fullPath;
      if (current) node.setAttribute("aria-current", "location");
      location.append(node);
    };
    if (!currentPath)
      location.append(
        make("span", "正在读取当前目录", "file-path-placeholder"),
      );
    else {
      if (windows) crumb("我的电脑", "我的电脑", "", null, rootId === null);
      if (rootId !== null) {
        const parts = currentPath
          .replaceAll("\\", "/")
          .split("/")
          .filter(Boolean);
        const drive = windows ? parts.shift().toUpperCase() : "";
        const systemRoot = windows ? `drive-${drive[0]}` : "system";
        const base = windows ? `${drive}\\` : "/";
        crumb(base, base, "", systemRoot, !parts.length);
        for (let index = 0; index < parts.length; index += 1) {
          const destination = parts.slice(0, index + 1).join("/");
          const fullPath =
            base + (windows ? destination.replaceAll("/", "\\") : destination);
          crumb(
            parts[index],
            fullPath,
            destination,
            systemRoot,
            index === parts.length - 1,
          );
        }
      }
    }
    el("file-drop-path").textContent =
      rootId === null ? "请先打开一个磁盘" : "当前目录";
    el("file-drop-path").title = currentPath;
    updateAvailability();
  }
  function renderEntries() {
    const list = el("file-list");
    list.replaceChildren();
    const filter = el("file-filter").value.trim().toLocaleLowerCase();
    const visible = entries.filter((entry) =>
      entry.name.toLocaleLowerCase().includes(filter),
    );
    el("file-count").textContent = (filter
      ? `${visible.length} / ${entries.length} 项`
      : `${entries.length} 项`) + (listingTruncated ? " (仅显示部分项目)" : "");
    if (!visible.length) {
      list.append(
        make(
          "li",
          filter ? "没有匹配的文件" : "此目录为空，可将文件拖到这里上传",
          "file-empty",
        ),
      );
    }
    for (const entry of visible) {
      const folder = entry.type === "directory";
      const drive = entry.type === "root";
      const row = make("li", undefined, "file-entry");
      row.dataset.name = entry.name;
      const icon = make(
        "span",
        undefined,
        folder ? "file-icon folder" : drive ? "file-icon drive" : "file-icon",
      );
      icon.setAttribute("aria-hidden", "true");
      const name =
        folder || drive
          ? button(
              entry.name,
              `${drive ? "打开磁盘" : "打开目录"} ${entry.name}`,
              () =>
                listFiles(
                  drive ? "" : join(directory, entry.name),
                  drive ? entry.root_id : rootId,
                ),
            )
          : make("span", entry.name);
      name.className = "file-name";
      name.title = entry.name;
      row.append(
        icon,
        name,
        make(
          "span",
          drive ? "磁盘" : folder ? "文件夹" : bytesText(entry.size),
          "file-size",
        ),
      );
      if (!folder && !drive) {
        const download = button("下载", `下载 ${entry.name}`, () =>
          enqueue([{ name: entry.name, size: entry.size }], "download"),
        );
        download.disabled = !allowed() || cleanupBlocked;
        row.append(download);
      }
      if (!drive) {
        const actions = make("div", undefined, "file-actions");
        actions.append(
          button("重命名", `重命名 ${entry.name}`, () =>
            openOperation("rename", entry),
          ),
          button("删除", `删除 ${entry.name}`, () =>
            openOperation("delete", entry),
          ),
        );
        actions.lastChild.classList.add("danger");
        for (const control of actions.children)
          control.disabled = !allowed() || cleanupBlocked || loading;
        row.append(actions);
      }
      list.append(row);
    }
  }
  function closeOperation() {
    operation = undefined;
    el("file-dialog").close();
  }
  // 弹窗绑定打开时的位置和会话；切换模式/断线关闭，不把确认应用到新的目录。
  function openOperation(action, entry = { name: "", type: "file" }) {
    if (!allowed() || cleanupBlocked || loading || rootId === null) return;
    operation = {
      action,
      entry,
      directory,
      rootId,
      session: state.sessionId,
      inputVersion: state.inputVersion,
    };
    const deleting = action === "delete";
    const folder = entry.type === "directory";
    el("file-dialog-title").textContent =
      action === "create"
        ? `新建${folder ? "文件夹" : "文件"}`
        : deleting
          ? "确认删除"
          : "重命名";
    el("file-dialog-description").textContent = deleting
      ? `将永久删除“${entry.name}”${folder ? "及其中的全部内容" : ""}。此操作无法撤销。`
      : `位置：${currentPath}`;
    el("file-name-label").hidden = el("file-name").hidden = deleting;
    el("file-name").required = !deleting;
    el("file-name").value = entry.name;
    el("file-dialog-error").textContent = "";
    el("file-dialog-submit").textContent = deleting ? "删除" : "确定";
    el("file-dialog-submit").classList.toggle("danger", deleting);
    el("file-dialog-submit").disabled = false;
    el("file-dialog").showModal();
    if (deleting) el("file-dialog-cancel").focus();
    else {
      el("file-name").focus();
      const dot = entry.name.lastIndexOf(".");
      el("file-name").setSelectionRange(
        0,
        !folder && dot > 0 ? dot : entry.name.length,
      );
    }
  }
  el("file-new").addEventListener("click", () => openOperation("create"));
  el("file-mkdir").addEventListener("click", () =>
    openOperation("create", { name: "", type: "directory" }),
  );
  el("file-dialog-cancel").addEventListener("click", closeOperation);
  el("file-dialog").addEventListener("cancel", () => {
    operation = undefined;
  });
  el("file-dialog-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const current = operation;
    if (
      !current ||
      current.busy ||
      !allowed() ||
      current.session !== state.sessionId ||
      current.inputVersion !== state.inputVersion
    )
      return;
    const name = el("file-name").value;
    if (
      current.action !== "delete" &&
      (!name ||
        name === "." ||
        name === ".." ||
        /[\\/\x00-\x1f:*?"<>|]/u.test(name) ||
        /[. ]$/u.test(name))
    ) {
      el("file-dialog-error").textContent =
        "请输入有效名称，不能包含路径分隔符或特殊字符";
      return;
    }
    if (current.action === "rename" && name === current.entry.name) {
      closeOperation();
      return;
    }
    current.busy = true;
    el("file-dialog-submit").disabled = true;
    const body = {
      action: `files.${current.action}`,
      root_id: current.rootId,
      path: join(
        current.directory,
        current.action === "create" ? name : current.entry.name,
      ),
    };
    if (current.action === "rename") body.name = name;
    else body.kind = current.entry.type;
    try {
      await endpoint(body, current.session);
      if (operation !== current) return;
      closeOperation();
      if (directory === current.directory && rootId === current.rootId)
        await listFiles();
    } catch (error) {
      if (operation !== current) return;
      el("file-dialog-error").textContent =
        error.status === 409
          ? "名称已存在或对象正被传输占用，请刷新后重试"
          : errorText(error);
    } finally {
      if (operation === current) {
        current.busy = false;
        el("file-dialog-submit").disabled = false;
      }
    }
  });
  // 只显示最新成功的位置，Windows“我的电脑”是磁盘列表，不是可上传目录。
  async function listFiles(destination = directory, selectedRoot = rootId) {
    if (!allowed()) return;
    const version = ++listingVersion;
    const session = state.sessionId;
    const inputVersion = state.inputVersion;
    loading = true;
    el("file-list").setAttribute("aria-busy", "true");
    el("file-count").textContent = "正在加载…";
    showError();
    updateAvailability();
    try {
      const response = await endpoint(
        selectedRoot === null
          ? { action: "files.roots" }
          : { action: "files.list", path: destination, root_id: selectedRoot },
        session,
      );
      if (
        version !== listingVersion ||
        session !== state.sessionId ||
        inputVersion !== state.inputVersion ||
        !allowed()
      )
        return;
      directory = destination;
      rootId = selectedRoot;
      windows = response.platform === "windows";
      currentPath = selectedRoot === null ? "我的电脑" : response.path;
      listingTruncated = response.truncated === true;
      entries = (
        selectedRoot === null
          ? response.roots.map((root) => ({
              name: root.path,
              type: "root",
              root_id: root.id,
            }))
          : response.entries
      )
        .slice()
        .sort(
          (left, right) =>
            Number(right.type === "directory") -
              Number(left.type === "directory") ||
            left.name.localeCompare(right.name, "zh-CN", { numeric: true }),
        );
      el("file-filter").value = "";
      renderLocation();
      renderEntries();
    } catch (error) {
      if (
        version === listingVersion &&
        session === state.sessionId &&
        inputVersion === state.inputVersion
      ) {
        showError(`无法打开目录：${errorText(error)}`);
      }
    } finally {
      if (version === listingVersion) {
        loading = false;
        el("file-list").setAttribute("aria-busy", "false");
        const filter = el("file-filter").value.trim().toLocaleLowerCase();
        el("file-count").textContent = (filter
          ? `${entries.filter((entry) => entry.name.toLocaleLowerCase().includes(filter)).length} / ${entries.length} 项`
          : `${entries.length} 项`) + (listingTruncated ? " (仅显示部分项目)" : "");
        updateAvailability();
      }
    }
  }
  function taskDetail(task) {
    const percent = `${task.size ? Math.floor((task.offset / task.size) * 100) : task.status === "completed" ? 100 : 0}%`;
    if (task.status === "queued")
      return `等待${task.direction === "upload" ? "上传到" : "下载自"} ${task.location}`;
    if (task.status === "preparing") return "正在准备…";
    if (task.status === "committing") return "100% · 正在保存到远程目录…";
    if (task.status === "saving") return "100% · 正在交给浏览器保存…";
    if (task.status === "completed")
      return task.direction === "upload"
        ? `100% · 上传完成${task.savedName && task.savedName !== task.name ? ` · 保存为 ${task.savedName}` : ""}`
        : "100% · 已交给浏览器保存";
    if (task.status === "failed") return task.error;
    if (task.status === "cancelled") return task.error || "已取消";
    const rate =
      task.offset / Math.max((performance.now() - task.started) / 1000, 0.001);
    const remaining = rate ? Math.ceil((task.size - task.offset) / rate) : 0;
    const eta = remaining
      ? ` · 剩余 ${remaining < 60 ? `${remaining} 秒` : `${Math.ceil(remaining / 60)} 分钟`}`
      : "";
    return `${percent} · ${bytesText(task.offset)} / ${bytesText(task.size)} · ${rate ? bytesText(rate) + "/s" : "计算速度…"}${eta}`;
  }
  function renderTasks() {
    const list = el("file-tasks");
    list.replaceChildren();
    for (const task of tasks) {
      const row = make("li", undefined, "file-task");
      row.dataset.name = task.name;
      row.dataset.state = task.status;
      const info = make("div", undefined, "file-task-info");
      info.append(
        make(
          "div",
          `${task.direction === "upload" ? "↑" : "↓"} ${task.name}`,
          "file-task-name",
        ),
        make("div", taskDetail(task), "file-task-detail"),
      );
      row.append(info);
      if (pending(task)) {
        const cancel = button("取消", `取消 ${task.name}`, () =>
          cancelTask(task).catch((error) => showError(errorText(error))),
        );
        cancel.disabled = ["committing", "saving"].includes(task.status);
        row.append(cancel);
      } else if (task.status !== "completed") {
        const retry = button("重试", `重试 ${task.name}`, () =>
          enqueue(
            [task.file || { name: task.name, size: task.size }],
            task.direction,
            task.directory,
            task.rootId,
            task.location,
          ),
        );
        retry.disabled =
          !allowed() || cleanupBlocked || task.session !== state.sessionId;
        row.append(retry);
      }
      list.append(row);
    }
    const waiting = tasks.filter((task) => task.status === "queued").length;
    const current =
      active && pending(active) ? active : tasks[tasks.length - 1];
    el("file-status").textContent = current
      ? `${current.name} · ${taskDetail(current)}${waiting ? ` · ${waiting} 项等待` : ""}`
      : "选择或拖入文件开始传输";
    el("file-cancel").disabled = !tasks.some(
      (task) =>
        pending(task) && !["committing", "saving"].includes(task.status),
    );
    if (
      current &&
      ["preparing", "committing", "saving"].includes(current.status)
    )
      el("file-progress").removeAttribute("value");
    else
      el("file-progress").value = current
        ? current.size
          ? (current.offset / current.size) * 100
          : current.status === "completed"
            ? 100
            : 0
        : 0;
  }
  function enqueue(
    files,
    direction = "upload",
    destination = directory,
    selectedRoot = rootId,
    location = currentPath,
  ) {
    if (!allowed() || cleanupBlocked) return;
    if (selectedRoot === null) {
      showError("请先打开一个磁盘，再选择上传文件");
      return;
    }
    const selected = Array.from(files);
    if (tasks.filter(pending).length + selected.length > MAX_PENDING) {
      showError(`最多同时排队 ${MAX_PENDING} 个文件，请分批选择`);
      return;
    }
    showError();
    for (const file of selected) {
      while (tasks.length >= MAX_HISTORY) {
        const index = tasks.findIndex(
          (task) => !pending(task) && task !== active,
        );
        if (index < 0) break;
        tasks.splice(index, 1);
      }
      tasks.push({
        name: file.name,
        file: direction === "upload" ? file : undefined,
        size: file.size,
        offset: 0,
        direction,
        directory: destination,
        rootId: selectedRoot,
        location,
        session: state.sessionId,
        inputVersion: state.inputVersion,
        status: file.size > MAX_FILE ? "failed" : "queued",
        error: file.size > MAX_FILE ? "单个文件不能超过 1 GiB" : undefined,
      });
    }
    renderTasks();
    void drain();
  }
  function ensureCurrent(task) {
    if (
      task.cancelled ||
      state.sessionId !== task.session ||
      state.readonly ||
      state.modePending ||
      state.inputVersion !== task.inputVersion
    ) {
      throw new Error("传输已取消或会话已结束");
    }
  }
  async function release(task) {
    if (!task.id || task.released) return;
    if (!task.releasePromise) {
      task.releasePromise = endpoint(
        { action: "files.cancel", transfer_id: task.id },
        task.session,
      )
        .then(() => {
          task.released = true;
        })
        .catch((error) => {
          // 已完成/会话关闭的资源确实不存在；其余清理失败必须可见并停止后续任务。
          if (error.status === 404 || error.status === 410) {
            task.released = true;
            return;
          }
          throw error;
        });
    }
    await task.releasePromise;
  }
  async function cancelTask(task, reason = "已取消") {
    if (!pending(task) || ["committing", "saving"].includes(task.status))
      return;
    task.cancelled = true;
    task.status = "cancelled";
    task.error = reason;
    renderTasks();
    try {
      await release(task);
    } catch (error) {
      if (task.session !== state.sessionId) throw error;
      cleanupBlocked = true;
      task.error = `取消清理失败：${errorText(error)}`;
      showError(`${task.error}，请重新连接后传输`);
      updateAvailability();
      renderTasks();
      throw error;
    }
  }
  async function cancelAll(reason = "已取消") {
    const results = await Promise.allSettled(
      tasks.filter(pending).map((task) => cancelTask(task, reason)),
    );
    const failed = results.find((result) => result.status === "rejected");
    if (failed) throw failed.reason;
  }
  const encode = (bytes) => {
    let binary = "";
    for (let index = 0; index < bytes.length; index += 8192)
      binary += String.fromCharCode(...bytes.subarray(index, index + 8192));
    return btoa(binary);
  };
  const decode = (text) =>
    Uint8Array.from(atob(text), (char) => char.charCodeAt(0));
  function reportProgress(task) {
    if (performance.now() - (task.lastRender || 0) >= 100) {
      task.lastRender = performance.now();
      renderTasks();
    }
  }
  async function transfer(task) {
    ensureCurrent(task);
    task.status = "preparing";
    renderTasks();
    const body = {
      action: `files.${task.direction}`,
      path: join(task.directory, task.name),
      root_id: task.rootId,
    };
    if (task.direction === "upload") body.size = task.size;
    const created = await endpoint(body, task.session);
    task.id = created.transfer_id;
    task.size = created.size;
    task.savedName = created.name;
    // begin 尚未返回也已占住队列；取消/换代后只回收晚到的任务，不发送数据。
    ensureCurrent(task);
    task.started = performance.now();
    task.status = task.direction === "upload" ? "uploading" : "downloading";
    renderTasks();
    if (task.direction === "upload") {
      while (task.offset < task.size) {
        ensureCurrent(task);
        const bytes = new Uint8Array(
          await task.file.slice(task.offset, task.offset + CHUNK).arrayBuffer(),
        );
        ensureCurrent(task);
        const response = await endpoint(
          {
            action: "files.write",
            transfer_id: task.id,
            offset: task.offset,
            data: encode(bytes),
          },
          task.session,
        );
        ensureCurrent(task);
        if (response.offset !== task.offset + bytes.length)
          throw new Error("上传偏移不符");
        task.offset = response.offset;
        reportProgress(task);
      }
      ensureCurrent(task);
      task.status = "committing";
      renderTasks();
      const committed = await endpoint(
        { action: "files.commit", transfer_id: task.id },
        task.session,
      );
      task.savedName = committed.name;
      task.released = true;
      if (state.sessionId === task.session)
        diagnostic().fileUploaded = task.size;
    } else {
      const blocks = [];
      let done = false;
      while (!done) {
        ensureCurrent(task);
        const response = await endpoint(
          { action: "files.read", transfer_id: task.id, offset: task.offset },
          task.session,
        );
        ensureCurrent(task);
        const bytes = decode(response.data);
        if (
          response.offset !== task.offset + bytes.length ||
          response.offset > task.size ||
          (!bytes.length && !response.done)
        )
          throw new Error("下载偏移不符");
        blocks.push(bytes);
        task.offset = response.offset;
        done = response.done;
        reportProgress(task);
      }
      if (task.offset !== task.size) throw new Error("下载长度不符");
      task.released = true;
      ensureCurrent(task);
      task.status = "saving";
      renderTasks();
      const url = URL.createObjectURL(new Blob(blocks));
      const link = make("a");
      link.href = url;
      link.download = task.name;
      link.click();
      objectUrls.set(
        url,
        setTimeout(() => {
          URL.revokeObjectURL(url);
          objectUrls.delete(url);
        }, 60_000),
      );
      diagnostic().fileDownloaded = task.offset;
    }
    task.status = "completed";
    if (task.direction === "upload") task.file = undefined;
  }
  // 单个服务端任务串行执行，批次中的失败不会吞掉后续文件，也不自动重放失败写入。
  async function drain() {
    if (running) return;
    running = true;
    try {
      while (allowed() && !cleanupBlocked) {
        const task = tasks.find((item) => item.status === "queued");
        if (!task) break;
        active = task;
        try {
          await transfer(task);
        } catch (error) {
          if (!task.cancelled) {
            task.status = "failed";
            task.error = errorText(error);
          }
          try {
            await release(task);
          } catch (cleanupError) {
            if (task.session === state.sessionId) {
              cleanupBlocked = true;
              showError(
                `传输清理失败：${errorText(cleanupError)}，请重新连接后传输`,
              );
            }
          }
        } finally {
          if (active === task) active = undefined;
          renderTasks();
          updateAvailability();
        }
        if (
          task.status === "completed" &&
          task.direction === "upload" &&
          task.session === state.sessionId &&
          !el("file-panel").hidden
        )
          await listFiles();
      }
    } finally {
      running = false;
    }
  }
  el("files").addEventListener("click", () => {
    if (!allowed()) return;
    el("file-panel").hidden = !el("file-panel").hidden;
    el("files").setAttribute("aria-pressed", String(!el("file-panel").hidden));
    if (!el("file-panel").hidden) {
      el("chat-panel").hidden = true;
      el("chat").setAttribute("aria-pressed", "false");
      void listFiles();
    }
  });
  el("file-close").addEventListener("click", () =>
    el("files").setAttribute("aria-pressed", "false"),
  );
  el("file-default").addEventListener("click", () => listFiles("", "default"));
  el("file-refresh").addEventListener("click", () => listFiles());
  el("file-filter").addEventListener("input", renderEntries);
  el("file-upload").addEventListener("change", () => {
    enqueue(el("file-upload").files);
    el("file-upload").value = "";
  });
  el("file-cancel").addEventListener("click", () =>
    cancelAll().catch((error) => showError(errorText(error))),
  );
  el("file-clear").addEventListener("click", () => {
    for (let index = tasks.length - 1; index >= 0; index -= 1)
      if (!pending(tasks[index]) && tasks[index] !== active)
        tasks.splice(index, 1);
    renderTasks();
  });
  for (const name of ["dragenter", "dragover"])
    el("file-browser").addEventListener(name, (event) => {
      event.preventDefault();
      if (allowed() && !cleanupBlocked && rootId !== null)
        el("file-browser").classList.add("dragging");
      if (event.dataTransfer)
        event.dataTransfer.dropEffect =
          allowed() && rootId !== null ? "copy" : "none";
    });
  el("file-browser").addEventListener("dragleave", (event) => {
    if (!el("file-browser").contains(event.relatedTarget))
      el("file-browser").classList.remove("dragging");
  });
  el("file-browser").addEventListener("drop", (event) => {
    event.preventDefault();
    el("file-browser").classList.remove("dragging");
    if (!allowed()) return;
    if (
      Array.from(event.dataTransfer.items || []).some(
        (item) => item.webkitGetAsEntry?.()?.isDirectory,
      )
    ) {
      showError("请拖入文件；文件夹上传尚未支持");
      return;
    }
    enqueue(event.dataTransfer.files);
  });
  renderLocation();
  return {
    applyCapabilities(caps) {
      supported = !!caps.file_transfer;
      updateAvailability();
    },
    updateAvailability() {
      updateAvailability();
      renderTasks();
    },
    async pause() {
      closeOperation();
      listingVersion += 1;
      loading = false;
      await cancelAll("已取消：切换为只看模式");
      updateAvailability();
    },
    teardown() {
      closeOperation();
      listingVersion += 1;
      loading = false;
      cleanupBlocked = false;
      for (const task of tasks) {
        if (pending(task)) {
          task.cancelled = true;
          task.status = "cancelled";
          task.error = "连接已断开，请重新选择文件";
        }
        task.file = undefined;
      }
      for (const [url, timer] of objectUrls) {
        clearTimeout(timer);
        URL.revokeObjectURL(url);
      }
      objectUrls.clear();
      directory = "";
      rootId = "default";
      currentPath = "";
      entries = [];
      listingTruncated = false;
      el("file-panel").hidden = true;
      el("files").setAttribute("aria-pressed", "false");
      renderLocation();
      renderEntries();
      renderTasks();
    },
  };
};
