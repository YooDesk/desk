(function () {
  // 邀请片段只在内存使用；认证前清除地址栏，HTTP 请求不携带邀请秘密。
  var invitationFragment = new URLSearchParams(location.hash.slice(1));
  var invitationCredential = invitationFragment.get('invite') || '';
  if (invitationFragment.has('invite')) { history.replaceState(history.state, '', location.pathname + location.search); }
  'use strict';

  var productId = document.documentElement.dataset.productId;
  // IP沿用当前访问地址，不附带端口；机器名来自被控端渲染的转义属性。
  document.title = document.documentElement.dataset.productName + '-' + document.documentElement.dataset.machineName + '(' + location.hostname + ')';
  var base = '/' + productId + '/webclient';
  var api = base + '/desk/api/v2';
  var connectorAccess = new URLSearchParams(window.__yoodeskViewer?.query ?? location.search).get('gateway') === '1';
  var viewerEntryFetch = window.fetch.bind(window);
  var viewerRecoveryTransport = null;
  // 只保留本页已验证的访问密码；不写入浏览器存储、URL 或诊断。
  var viewerRecoveryCredential = '';
  var transportReady = connectorAccess ? import('/' + productId + '/transport/viewer.js').then(function (module) {
    return module.attachViewer(productId).then(function (state) {
      return Promise.resolve(window.__yoodeskTransportReady).then(function (transport) {
        if (typeof transport?.reconnect === 'function') { viewerRecoveryTransport = transport; }
        return state;
      });
    });
  }) : Promise.resolve();
  transportReady.catch(function (error) {
    // SDK 初次协商失败后，这个 Promise 不会恢复；同页重试只能再次得到原错误。
    if (connectorAccess && !/rejected \((401|403)\)$/.test(error.message)) {
      recoverViewerTransport();
    }
  });
  // 所有业务读取都等待 SDK；信令与授权由 SDK 保存的原生 fetch 执行。
  function fetch(url, options) {
    var ready = transportReady;
    if (options && options.signal) {
      var signal = options.signal;
      if (signal.aborted) { return Promise.reject(signal.reason); }
      ready = new Promise(function (resolve, reject) {
        var abort = function () { reject(signal.reason); };
        signal.addEventListener('abort', abort, {once: true});
        transportReady.then(resolve, reject).finally(function () { signal.removeEventListener('abort', abort); });
      });
    }
    return ready.then(function () {
      if (options && options.signal) { options.signal.throwIfAborted(); }
      return window.fetch(url, options);
    });
  }
  var access = base + (connectorAccess ? '/desk/connector-access/v2' : '/access/v2');

  var state = {
    csrf: '',
    sessionId: null,
    stream: null,
    events: null,
    inputController: null,
    inputVersion: 0,
    modePending: false,
    controlAllowed: false,
    decoder: null,
    decoderGeneration: 0,
    waitingKey: true,
    videoRecovering: false,
    submitted: new Map(),
    stats: { frames: 0, bytes: 0, since: performance.now(), ping: null },
    readonly: false,
    viewerMode: 'control',
    hasVideoFrame: false,
    displays: [],
    frameWidth: 0,
    frameHeight: 0,
    capabilities: null,
    platform: {},
    cursor: { shapes: new Map(), image: null, x: 0, y: 0, visible: false, hotx: 0, hoty: 0 },
  };
  var extensions = window.createDeskExtensions({ request: request, api: api, state: state });
  // 剪贴板失败保留诊断状态，按用户要求不显示同步提示。
  var clipboard = window.createDeskClipboard({ request: request, api: api, state: state, say: function (text) {
    window.__yoodeskDesk = window.__yoodeskDesk || {};
    window.__yoodeskDesk.clipboardError = text;
  }, sendInputBatch: sendInputs });
  var chat = window.createDeskChat({request: request, api: api, state: state});

  var el = function (id) { return document.getElementById(id); };
  var message = el('message');
  var statusEl = el('status');
  var canvas = el('screen');
  var ctx = canvas.getContext('2d', { alpha: false });
  var overlay = el('overlay');
  var overlayCtx = overlay.getContext('2d');
  var toolbarTimer;
  function revealToolbar() {
    el('toolbar').classList.remove('auto-hidden');
    clearTimeout(toolbarTimer);
    toolbarTimer = setTimeout(function () {
      if (!pinned && !drag && !el('toolbar').matches(':hover') &&
          !(el('toolbar').contains(document.activeElement) && document.activeElement.matches(':focus-visible'))) {
        el('toolbar').classList.add('auto-hidden');
      }
    }, 1500);
  }
  el('toolbar-reveal').addEventListener('pointerenter', revealToolbar);
  el('toolbar').addEventListener('pointerenter', revealToolbar);
  el('toolbar').addEventListener('pointerleave', revealToolbar);
  el('toolbar').addEventListener('focusin', revealToolbar);
  el('toolbar').addEventListener('focusout', revealToolbar);
  var fit = true;

  function fitStage() {
    var stage = el('stage');
    var padding = getComputedStyle(stage);
    var width = stage.clientWidth - parseFloat(padding.paddingLeft) - parseFloat(padding.paddingRight);
    var height = stage.clientHeight - parseFloat(padding.paddingTop) - parseFloat(padding.paddingBottom);
    var ratio = fit ? Math.min(width / canvas.width, height / canvas.height) : 1;
    var viewport = stage.querySelector('.viewport');
    viewport.style.width = Math.max(1, Math.floor(canvas.width * ratio)) + 'px';
    viewport.style.height = Math.max(1, Math.floor(canvas.height * ratio)) + 'px';
    stage.classList.toggle('native-size', !fit);
  }
  new ResizeObserver(fitStage).observe(canvas);
  window.addEventListener('resize', function () {
    fitStage();
    var toolbar = el('toolbar');
    if (toolbar.style.transform === 'none') {
      toolbar.style.left = Math.max(8, Math.min(innerWidth - toolbar.offsetWidth - 8, parseFloat(toolbar.style.left))) + 'px';
      toolbar.style.top = Math.max(8, Math.min(innerHeight - toolbar.offsetHeight - 8, parseFloat(toolbar.style.top))) + 'px';
    }
  });
  el('fit').addEventListener('click', function () {
    fit = !fit;
    el('fit').setAttribute('aria-pressed', String(fit));
    el('fit').title = fit ? '切换原始尺寸' : '适应窗口';
    fitStage();
  });
  el('fullscreen').addEventListener('click', function () {
    var action = document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen();
    action.then(function () {
      if (document.fullscreenElement) { canvas.focus(); }
      updateKeyboardLock();
    }).catch(function (error) { say(error.message, true); });
  });
  document.addEventListener('fullscreenchange', function () {
    if (!document.fullscreenElement) { releasePressedKeyboard(); }
    updateKeyboardLock();
    fitStage();
  });
  el('file-close').addEventListener('click', function () { el('file-panel').hidden = true; });
  var pinned = false;
  var drag;
  el('toolbar-pin').addEventListener('click', function () {
    pinned = !pinned;
    el('toolbar-pin').setAttribute('aria-pressed', String(pinned));
    el('toolbar-pin').title = pinned ? '解除固定' : '固定工具栏';
    revealToolbar();
  });
  el('toolbar-drag').addEventListener('pointerdown', function (event) {
    var rect = el('toolbar').getBoundingClientRect();
    drag = {x: event.clientX - rect.left, y: event.clientY - rect.top};
    el('toolbar-drag').setPointerCapture(event.pointerId);
    el('toolbar').style.transform = 'none';
    el('toolbar').style.left = rect.left + 'px';
  });
  el('toolbar-drag').addEventListener('pointermove', function (event) {
    if (!drag) { return; }
    var toolbar = el('toolbar');
    toolbar.style.left = Math.max(8, Math.min(innerWidth - toolbar.offsetWidth - 8, event.clientX - drag.x)) + 'px';
    toolbar.style.top = Math.max(8, Math.min(innerHeight - toolbar.offsetHeight - 8, event.clientY - drag.y)) + 'px';
  });
  el('toolbar-drag').addEventListener('pointerup', function () { drag = null; });
  el('toolbar-drag').addEventListener('pointercancel', function () { drag = null; });

  function setStatus(text, kind) {
    statusEl.textContent = ({active: '已连接', connecting: '连接中', transport_ready: '准备就绪', failed: '连接失败', closed: '已断开'})[text] || text;
    if (kind) { statusEl.dataset.state = kind; } else { delete statusEl.dataset.state; }
  }
  function say(text, isError) {
    message.textContent = text || '';
    message.classList.toggle('error', !!isError);
  }
  function requestId() {
    if (window.crypto && window.crypto.randomUUID) { return window.crypto.randomUUID(); }
    return 'req-' + Date.now() + '-' + Math.random().toString(16).slice(2);
  }
  function headers(extra) {
    var all = { 'X-Request-ID': requestId() };
    if (state.csrf) { all['X-CSRF-Token'] = state.csrf; }
    if (extra) { Object.keys(extra).forEach(function (k) { all[k] = extra[k]; }); }
    return all;
  }
  function apiError(payload, fallback) {
    if (payload && payload.error && payload.error.message) { return payload.error.message; }
    return fallback;
  }
  function request(method, url, body, signal) {
    if (state.csrf && method !== 'GET' && url !== access + '/login/') {
      var id = requestId();
      var expectedInstance = state.accessInstance;
      var write = async function () {
        var refresh = async function () {
          var timeout = AbortSignal.timeout(15000);
          var info = await rawRequest('GET', access + '/session/', undefined,
            signal ? AbortSignal.any([signal, timeout]) : timeout);
          if (!expectedInstance || state.accessInstance !== expectedInstance || info.accessInstance !== expectedInstance) {
            throw new DOMException('访问会话已变更，旧操作已取消', 'AbortError');
          }
          state.csrf = info.csrf_token;
        };
        await refresh();
        var result;
        try { result = await rawRequest(method, url, body, signal, id, expectedInstance); }
        catch (error) {
          // CSRF阶段已拒绝的请求没有执行；保持请求id仅重发一次。
          if (!error.csrfRejected || (signal && signal.aborted)) { throw error; }
          await refresh();
          result = await rawRequest(method, url, body, signal, id, expectedInstance);
        }
        if (url === access + '/session/renew/') { state.csrf = result.csrf_token; }
        return result;
      };
      if (url !== access + '/session/renew/') { return write(); }
      signal = signal ? AbortSignal.any([signal, AbortSignal.timeout(15000)]) : AbortSignal.timeout(15000);
      // 锁只协调有期限的续期，不等待任何远端业务I/O。
      return navigator.locks ? navigator.locks.request(productId + '-access-renew', {signal: signal}, write) : write();
    }
    return rawRequest(method, url, body, signal);
  }
  function rawRequest(method, url, body, signal, id, instance) {
    var init = { method: method, headers: headers(), credentials: 'same-origin' };
    if (id) { init.headers['X-Request-ID'] = id; }
    if (method !== 'GET' && url !== access + '/login/' && (instance || state.accessInstance)) {
      init.headers['X-Access-Instance'] = instance || state.accessInstance;
    }
    // 页面退出仍发送小型关闭请求，避免浏览器导航取消资源回收。
    if (method === 'DELETE' && url.indexOf(api + '/sessions/') === 0) { init.keepalive = true; }
    if (signal) { init.signal = signal; }
    if (body !== undefined) {
      init.headers['Content-Type'] = 'application/json';
      init.body = JSON.stringify(body);
    }
    return fetch(url, init).then(function (response) {
      var type = response.headers.get('Content-Type') || '';
      var parse = type.indexOf('application/json') === 0 ? response.json() : Promise.resolve(null);
      return parse.then(function (payload) {
        if (!response.ok) {
          var error = new Error(apiError(payload, 'HTTP ' + response.status));
          error.status = response.status;
          error.csrfRejected = response.status === 400 && payload && payload.error &&
            payload.error.code === 'invalid_request' && payload.error.retryable === true &&
            payload.error.details && payload.error.details.stage === 'csrf' &&
            payload.error.details.field === 'x-csrf-token';
          if (response.status === 401 && connectionWanted) { expireAccess(); }
          throw error;
        }
        if (payload && payload.data && url.indexOf(access + '/') === 0) {
          Object.defineProperty(payload.data, 'accessInstance', {value: response.headers.get('x-access-instance') || ''});
        }
        return payload ? payload.data : null;
      });
    });
  }

  function showLogin(show) {
    el('login').classList.toggle('hidden', !show);
    document.body.classList.toggle('login-required', show);
  }

  function bootstrap() {
    return fetch(access + '/bootstrap/?mode=cookie', { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (payload) {
        if (!payload || payload.v !== 2 || !payload.data) { throw new Error('bootstrap failed'); }
        state.csrf = payload.data.preauth_csrf;
        return payload.data;
      });
  }

  function login(credential, recovering, signal) {
    return (connectorAccess ? Promise.resolve() : bootstrap()).then(function () {
      return request('POST', access + '/login/', { credential: credential, access_mode: 'cookie' }, signal);
    }).then(function (info) {
      if (signal) { signal.throwIfAborted(); }
      state.csrf = info.csrf_token;
      window.__yoodeskDesk = window.__yoodeskDesk || {};
      window.__yoodeskDesk.csrf = info.csrf_token;
      if (connectorAccess) { viewerRecoveryCredential = credential; }
      el('credential').value = '';
      if (recovering) { stopViewerTransportRecovery(); }
      return afterLogin(info);
    }).catch(function (error) {
      if (recovering && transportRecoveryPending) { throw error; }
      el('login-error').textContent = error.message;
      showLogin(true);
      throw error;
    });
  }

  function loadSession() {
    return fetch(access + '/session/', { credentials: 'same-origin' }).then(function (response) {
      if (response.status === 401) { return null; }
      return response.json().then(function (payload) {
        if (!response.ok) { return null; }
        Object.defineProperty(payload.data, 'accessInstance', {value: response.headers.get('x-access-instance') || ''});
        state.csrf = payload.data.csrf_token;
        return payload.data;
      });
    });
  }

  // 按服务端能力裁剪 UI：不支持的控件隐藏，而不是点了才报错。
  function applyCapabilities(data) {
    state.capabilities = data.capabilities || null;
    var caps = state.capabilities || {};
    var __diag = window.__yoodeskDesk = window.__yoodeskDesk || {};
    __diag.capabilities = caps;
    extensions.applyCapabilities(caps);
    el('chat').hidden = !caps.chat;
    updateReadonlyControl();
    var platform = data.platform || {};
    state.platform = platform;
    __diag.platform = platform;
    say('桌面能力已就绪');
  }

  // 读取一次能力，供 UI 裁剪与“是否允许输入”的判断使用。
  // 会话只读取决于实际授权；viewer的“只看”只是本地输入开关。
  function loadCapabilities(signal) {
    return request('GET', api + '/capabilities/', undefined, signal).then(applyCapabilities);
  }

  function loadDisplays(signal) {
    var selected = el('display').value;
    return request('GET', api + '/displays/', undefined, signal).then(function (data) {
      state.displays = data.items || [];
      var select = el('display');
      select.textContent = '';
      state.displays.forEach(function (display, index) {
        var option = document.createElement('option');
        option.value = display.display_id;
        option.textContent = '显示器' + (index + 1) + '(' + display.width + '*' + display.height + ')';
        select.appendChild(option);
      });
      if (state.displays.some(function (display) { return display.display_id === selected; })) { select.value = selected; }
      select.closest('.display-picker').hidden = state.displays.length <= 1;
    });
  }

  // 单个连接任务串行执行；切模式、退出、网络变化立即隔离旧回调和输入队列。
  var connectionWanted = false;
  var connectionGeneration = 0;
  var connectionAttempt = null;
  var connectionController = null;
  var reconnectTimer;
  var retryDelay = 500;
  var restartAt = 0;
  var streamOpenedAt = null;
  var cleanupTail = Promise.resolve();
  var lastPong = 0;
  var transportRecoveryTimer;
  var transportRecoveryPending = false;
  var transportRecoveryNavigating = false;
  var transportRecoveryController;
  var transportRecoveryAttempts = 0;
  var transportRecoveryDelays = [1000, 2000, 5000, 10000, 15000];
  var transportRecoveryLimit = 10;
  var transportRecoveryCleared = false;
  var transportRetryFailures = 0;
  var transportRecoveryKey = 'yoodesk-desk-reconnect:' + location.pathname;

  function recoverViewerTransport() {
    if (!connectorAccess || transportRecoveryPending) { return; }
    var attempts;
    try {
      attempts = Number(sessionStorage.getItem(transportRecoveryKey) || '0');
      // 跨页面保留预算；权限失效或持续故障不能造成无限刷新。
      if (!Number.isInteger(attempts) || attempts < 0) { return; }
      if (attempts >= transportRecoveryLimit) {
        connectionWanted = false;
        setStatus('重连失败', 'failed');
        return;
      }
    } catch (_) { return; }
    transportRecoveryAttempts = attempts;
    transportRecoveryPending = true;
    clearTimeout(reconnectTimer);
    setStatus('等待重连', 'connecting');
    resumeViewerTransportRecovery(transportRecoveryDelays[Math.min(attempts, transportRecoveryDelays.length - 1)]);
  }

  function resumeViewerTransportRecovery(delay) {
    clearTimeout(transportRecoveryTimer);
    if (!transportRecoveryPending || transportRecoveryNavigating || !navigator.onLine) { return; }
    transportRecoveryTimer = setTimeout(async function () {
      if (!transportRecoveryPending || transportRecoveryNavigating || !navigator.onLine) { return; }
      try { sessionStorage.setItem(transportRecoveryKey, String(transportRecoveryAttempts + 1)); }
      catch (_) { stopViewerTransportRecovery(); return; }
      transportRecoveryNavigating = true;
      try {
        if (viewerRecoveryTransport) {
          var controller = transportRecoveryController = new AbortController();
          var credential = viewerRecoveryCredential;
          // 已排队的旧输入/关闭请求不能跨到新的授权。
          connectionGeneration += 1;
          if (connectionController) { connectionController.abort(); }
          await disconnect();
          if (!transportRecoveryPending || controller.signal.aborted) { return; }
          var replacement = viewerRecoveryTransport.reconnect(controller.signal);
          transportReady = replacement.then(function (next) { return next.state; });
          transportReady.catch(function () {});
          viewerRecoveryTransport = await replacement;
          if (!transportRecoveryPending || controller.signal.aborted) { return; }
          if (credential) {
            await login(credential, true, controller.signal);
          } else {
            stopViewerTransportRecovery();
            connectionWanted = false;
            showLogin(true);
          }
          return;
        }
        // 只探测当前页面入口，业务请求仍只能走 SDK。已失效的入口不能把页面刷成错误页。
        var response = await viewerEntryFetch(location.href, {
          credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: AbortSignal.timeout(5000)
        });
        if (!transportRecoveryPending) { return; }
        if (response.ok && (response.headers.get('content-type') || '').includes('text/html')) {
          if (window.__yoodeskViewer?.reload) { window.__yoodeskViewer.reload(); }
          else { location.reload(); }
          return;
        }
        if ([401, 403].includes(response.status)) {
          stopViewerTransportRecovery();
          connectionWanted = false;
          setStatus('访问已失效', 'failed');
          return;
        }
      } catch (_) {
        if (!transportRecoveryPending) { return; }
      }
      stopViewerTransportRecovery();
      recoverViewerTransport();
    }, delay);
  }

  function stopViewerTransportRecovery() {
    transportRecoveryPending = false;
    transportRecoveryNavigating = false;
    clearTimeout(transportRecoveryTimer);
    if (transportRecoveryController) { transportRecoveryController.abort(); transportRecoveryController = null; }
  }

  function ensureGeneration(generation) {
    if (!connectionWanted || generation !== connectionGeneration) {
      throw new DOMException('连接已取消', 'AbortError');
    }
  }
  function deleteSession(id) {
    if (!id) { return Promise.resolve(); }
    var controller = new AbortController();
    var timer = setTimeout(function () { controller.abort(); }, 5000);
    return request('DELETE', api + '/sessions/' + id + '/', undefined, controller.signal)
      .catch(function (error) {
        if (error.status !== 404 && !transportRecoveryPending) { say('关闭会话失败：' + error.message, true); }
      }).finally(function () { clearTimeout(timer); });
  }
  function scheduleConnection(delay) {
    clearTimeout(reconnectTimer);
    if (!connectionWanted || connectionAttempt || !navigator.onLine || transportRecoveryPending) { return; }
    reconnectTimer = setTimeout(runConnection, delay);
  }
  function restartConnection(unexpected, reason) {
    if (!connectionWanted) { return; }
    if (connectorAccess && document.documentElement.dataset.transportState === 'closed') {
      recoverViewerTransport();
      if (transportRecoveryPending || !connectionWanted) { return; }
    }
    var now = performance.now();
    var diagnostic = window.__yoodeskDesk = window.__yoodeskDesk || {};
    diagnostic.lastReconnectReason = reason || 'other';
    diagnostic.lastReconnectPongAgeMs = now - lastPong;
    if (!unexpected || (streamOpenedAt !== null && now - streamOpenedAt >= 10000)) {
      retryDelay = 500;
    }
    streamOpenedAt = null;
    var delay = unexpected ? retryDelay : 0;
    if (unexpected) { retryDelay = Math.min(10000, retryDelay * 2); }
    restartAt = now + delay;
    connectionGeneration += 1;
    if (connectionController) { connectionController.abort(); }
    disconnect();
    setStatus(navigator.onLine ? '连接中' : '等待网络', 'connecting');
    scheduleConnection(delay);
  }
  async function runConnection() {
    if (!connectionWanted || connectionAttempt || !navigator.onLine || transportRecoveryPending) { return; }
    var generation = connectionGeneration;
    var controller = connectionController = new AbortController();
    // 等待关闭旧会话后才创建新会话，避免遗留按键和隐私屏跨越模式。
    connectionAttempt = (async function () {
      await cleanupTail;
      ensureGeneration(generation);
      setStatus('连接中', 'connecting');
      await loadCapabilities(controller.signal);
      ensureGeneration(generation);
      await loadDisplays(controller.signal);
      ensureGeneration(generation);
      await createSession(generation, controller.signal);
    }());
    var timeout = setTimeout(function () { controller.abort(); }, 15000);
    try {
      await connectionAttempt;
      ensureGeneration(generation);
      transportRetryFailures = 0;
      message.textContent = '';
    } catch (error) {
      if (generation === connectionGeneration && connectionWanted) {
        if (error.status === 403) {
          connectionWanted = false;
          setStatus('访问被拒绝', 'failed');
        } else {
          setStatus('等待重连', 'connecting');
          // 信令还活着时，业务通道也可能已经不可用。给原页重连机会，再重开入口。
          if (connectorAccess && navigator.onLine &&
              (!error.status || [429, 502, 503, 504].includes(error.status)) &&
              ++transportRetryFailures >= 3) {
            recoverViewerTransport();
          }
        }
        say(error.name === 'AbortError' ? '连接超时' : error.message, true);
        disconnect();
      }
    } finally {
      clearTimeout(timeout);
      connectionAttempt = null;
      if (connectionController === controller) { connectionController = null; }
      if (!state.stream) {
        if (generation === connectionGeneration) {
          scheduleConnection(retryDelay);
          retryDelay = Math.min(10000, retryDelay * 2);
        } else {
          scheduleConnection(Math.max(0, restartAt - performance.now()));
        }
      }
    }
  }
  async function createSession(generation, signal) {
    updateReadonlyControl();
    var options = { read_only: !state.controlAllowed || !(state.capabilities.pointer || state.capabilities.keyboard) };
    var displayId = el('display').value;
    if (displayId) { options.display_id = displayId; }
    // 创建请求收到响应后检查代次，晚到的新资源也必须关闭。
    var info = await request('POST', api + '/sessions/', {
      protocol_version: 2, target_id: 'local', options: options,
      client_features: ['h264s', 'mouse', 'keyboard']
    }, AbortSignal.timeout(15000));
    if (!connectionWanted || generation !== connectionGeneration || signal.aborted) {
      await deleteSession(info.business_session_id);
      throw new DOMException('连接已取消', 'AbortError');
    }
    state.sessionId = info.business_session_id;
    state.inputController = new AbortController();
    state.frameWidth = 0;
    state.frameHeight = 0;
    var diagnostic = window.__yoodeskDesk = window.__yoodeskDesk || {};
    diagnostic.sessionId = state.sessionId;
    diagnostic.readOnly = state.readonly;
    diagnostic.modePending = false;
    // 事件/心跳与视频写入分开；均使用同一访问实例的单次票据。
    await Promise.all([
      openEvents(info.event_url, generation, signal),
      waitForStream(generation, signal)
    ]);
  }
  async function waitForStream(generation, signal) {
    var id = state.sessionId;
    for (var attempts = 0; attempts < 100; attempts += 1) {
      ensureGeneration(generation);
      var info = await request('GET', api + '/sessions/' + id + '/', undefined, signal);
      ensureGeneration(generation);
      if (info.stream_available) { return openStream(info.stream_url, generation, signal); }
      if (info.state === 'failed' || info.state === 'closed') { throw new Error('会话已结束：' + info.state); }
      await new Promise(function (resolve, reject) {
        var timer = setTimeout(done, 100);
        function done() { signal.removeEventListener('abort', cancelled); resolve(); }
        function cancelled() { clearTimeout(timer); signal.removeEventListener('abort', cancelled); reject(new DOMException('连接已取消', 'AbortError')); }
        signal.addEventListener('abort', cancelled, {once: true});
        if (signal.aborted) { cancelled(); }
      });
    }
    throw new Error('等待数据流超时');
  }
  async function openEvents(url, generation, signal) {
    var ticket = await request('POST', api + '/sessions/' + state.sessionId + '/stream-tickets/', {channel: 'events'}, signal);
    ensureGeneration(generation);
    if (signal.aborted) { throw new DOMException('连接已取消', 'AbortError'); }
    var socket = new WebSocket(new URL(url, window.__yoodeskViewer?.origin ?? location.origin).toString().replace(/^http/, 'ws'), ['raccess.business.v2', 'raccess.ticket.' + ticket.ticket]);
    state.events = socket;
    socket.onmessage = function (event) {
      if (state.events !== socket || generation !== connectionGeneration) { return; }
      var data;
      try { data = JSON.parse(event.data); } catch (e) { return; }
      if (data.type === 'pong') { observePong(data); return; }
      if (data.type === 'session.closed' || (data.type === 'session.state' && ['closed', 'failed'].includes((data.payload || {}).state))) { restartConnection(true, 'session_closed'); }
    };
    await new Promise(function (resolve, reject) {
      function cancelled() { socket.close(); reject(new DOMException('连接已取消', 'AbortError')); }
      signal.addEventListener('abort', cancelled, {once: true});
      socket.onopen = function () {
        signal.removeEventListener('abort', cancelled);
        if (state.events !== socket || generation !== connectionGeneration) { socket.close(); reject(new DOMException('连接已取消', 'AbortError')); return; }
        lastPong = performance.now();
        resolve();
      };
      socket.onclose = function () {
        signal.removeEventListener('abort', cancelled);
        reject(new Error('事件通道已关闭'));
        if (state.events === socket) { restartConnection(true, 'events_closed'); }
      };
      socket.onerror = function () { reject(new Error('事件通道连接失败')); };
      if (signal.aborted) { cancelled(); }
    });
  }
  function observePong(control) {
    lastPong = performance.now();
    if (state.stats.ping && control.id === state.stats.ping.id) {
      window.__yoodeskDesk.rttMs = performance.now() - state.stats.ping.at;
      state.stats.ping = null;
    }
  }
  async function openStream(url, generation, signal) {
    var ticket = await request('POST', api + '/sessions/' + state.sessionId + '/stream-tickets/', {channel: 'stream', protocols: ['h264s']}, signal);
    ensureGeneration(generation);
    var ws = new WebSocket(new URL(url, window.__yoodeskViewer?.origin ?? location.origin).toString().replace(/^http/, 'ws'), ['raccess.business.v2', 'raccess.ticket.' + ticket.ticket]);
    state.stream = ws;
    state.waitingKey = true;
    ws.binaryType = 'arraybuffer';
    ws.onmessage = function (event) {
      if (state.stream !== ws || generation !== connectionGeneration) { return; }
      if (typeof event.data === 'string') {
        var control;
        try { control = JSON.parse(event.data); } catch (error) { say('无效的视频控制消息', true); return; }
        if (control.type === 'video_reset') { state.videoRecovering = true; resetDecoder(); }
        else if (control.type === 'pong') {
          observePong(control);
        }
        return;
      }
      var diagnostic = window.__yoodeskDesk = window.__yoodeskDesk || {};
      diagnostic.binaryMessages = (diagnostic.binaryMessages || 0) + 1;
      handleFrame(new Uint8Array(event.data));
    };
    await new Promise(function (resolve, reject) {
      function cancelled() { ws.close(); reject(new DOMException('连接已取消', 'AbortError')); }
      signal.addEventListener('abort', cancelled, {once: true});
      ws.onopen = function () {
        signal.removeEventListener('abort', cancelled);
        if (state.stream !== ws || generation !== connectionGeneration) { ws.close(); reject(new DOMException('连接已取消', 'AbortError')); return; }
        lastPong = streamOpenedAt = performance.now();
        setStatus('已连接', 'active');
        updateReadonlyControl();
        if (!state.readonly) { clipboard.start(); }
        chat.start();
        resolve();
      };
      ws.onclose = function () {
        signal.removeEventListener('abort', cancelled);
        reject(new Error('数据流已关闭'));
        if (state.stream === ws) { restartConnection(true, 'stream_closed'); }
      };
      ws.onerror = function () { reject(new Error('数据流连接失败')); };
      if (signal.aborted) { cancelled(); }
    });
  }

  // protobuf 最小解码：只读取 Message.video_frame -> h264s.frames[0].{data,key,pts}。
  function readVarint(bytes, offset) {
    var result = 0, shift = 0, byte;
    do {
      if (offset >= bytes.length) { throw new Error('varint overflow'); }
      byte = bytes[offset++];
      result += (byte & 0x7f) * Math.pow(2, shift);
      shift += 7;
    } while (byte & 0x80);
    return [result, offset];
  }
  function fields(bytes) {
    var out = [], offset = 0;
    while (offset < bytes.length) {
      var tag;
      var pair = readVarint(bytes, offset);
      tag = pair[0]; offset = pair[1];
      var field = Math.floor(tag / 8), wire = tag & 7;
      if (wire === 0) { var value = readVarint(bytes, offset); offset = value[1]; out.push([field, 0, value[0]]); }
      else if (wire === 2) { var len = readVarint(bytes, offset); offset = len[1]; out.push([field, 2, bytes.subarray(offset, offset + len[0])]); offset += len[0]; }
      else if (wire === 1) { offset += 8; }
      else if (wire === 5) { offset += 4; }
      else { throw new Error('unsupported wire type ' + wire); }
    }
    return out;
  }
  function decodeMessageFields(messageBytes) {
    // Message 的 oneof：video_frame = 6、cursor_data = 12、cursor_position = 13。
    var result = { video: null, cursorData: null, cursorPosition: null, audio: null, audioFormat: null };
    var top;
    try { top = fields(messageBytes); } catch (e) { return result; }
    top.forEach(function (entry) {
      if (entry[1] !== 2) { return; }
      if (entry[0] === 6) { result.video = decodeVideoFrame(entry[2]); }
      else if (entry[0] === 12) { result.cursorData = decodeCursorData(entry[2]); }
      else if (entry[0] === 13) { result.cursorPosition = decodeCursorPosition(entry[2]); }
      else if (entry[0] === 11) { result.audio = entry[2]; }
      else if (entry[0] === 19) {
        fields(entry[2]).forEach(function (m) { if (m[0] === 8 && m[1] === 2) { result.audioFormat = m[2]; } });
      }
    });
    return result;
  }

  function decodeVideoFrame(video) {
    // VideoFrame.h264s = 10；EncodedVideoFrames.frames = 1（repeated）。
    // 一条消息里可能有多个 EncodedVideoFrame（SPS/PPS 与 IDR 分开），必须全部拼接；
    // 只取第一段会丢掉参数集，WebCodecs 就会静默不出帧。
    var h264 = null;
    fields(video).forEach(function (entry) {
      if (entry[0] === 10 && entry[1] === 2) { h264 = entry[2]; }
    });
    if (!h264) { return null; }
    var chunks = [];
    var key = false;
    var pts = 0;
    fields(h264).forEach(function (entry) {
      if (entry[0] !== 1 || entry[1] !== 2) { return; }
      var data = null;
      var frameKey = false;
      var framePts = 0;
      fields(entry[2]).forEach(function (inner) {
        if (inner[0] === 1 && inner[1] === 2) { data = inner[2]; }
        else if (inner[0] === 2 && inner[1] === 0) { frameKey = inner[2] !== 0; }
        else if (inner[0] === 3 && inner[1] === 0) { framePts = inner[2] || 0; }
      });
      if (data) {
        chunks.push(data);
        key = key || frameKey;
        pts = Math.max(pts, framePts);
      }
    });
    if (chunks.length === 0) { return null; }
    var total = chunks.reduce(function (sum, chunk) { return sum + chunk.length; }, 0);
    var merged = new Uint8Array(total);
    var offset = 0;
    chunks.forEach(function (chunk) {
      merged.set(chunk, offset);
      offset += chunk.length;
    });
    var sps = findSps(merged);
    var geometry = sps ? dimensionsFromSps(sps) : null;
    return {
      data: merged,
      key: key,
      pts: pts,
      width: geometry ? geometry[0] : 0,
      height: geometry ? geometry[1] : 0,
    };
  }

  // 从 SPS 解析宽高（仅覆盖 emulation-prevention 之后的 exp-golomb 头部）。
  function dimensionsFromSps(sps) {
    if (!sps || sps.length < 8) { return null; }
    // 跳过 NAL 头 (1) + profile/constraints/level (3)。
    var bytes = [];
    for (var i = 4; i < sps.length; i += 1) {
      if (i + 2 < sps.length && sps[i] === 0 && sps[i + 1] === 0 && sps[i + 2] === 3) {
        bytes.push(sps[i], sps[i + 1]);
        i += 2;
      } else {
        bytes.push(sps[i]);
      }
    }
    var bitPos = 0;
    function readBit() {
      var byteIndex = bitPos >> 3;
      if (byteIndex >= bytes.length) { throw new Error('sps eof'); }
      var bit = (bytes[byteIndex] >> (7 - (bitPos & 7))) & 1;
      bitPos += 1;
      return bit;
    }
    function readBits(count) {
      var value = 0;
      for (var i = 0; i < count; i += 1) { value = value * 2 + readBit(); }
      return value;
    }
    function readUe() {
      var zeros = 0;
      while (readBit() === 0) {
        zeros += 1;
        if (zeros > 31) { throw new Error('sps bad'); }
      }
      return (Math.pow(2, zeros) - 1) + readBits(zeros);
    }
    function readSe() {
      var value = readUe();
      var magnitude = Math.ceil(value / 2);
      return (value % 2 === 1) ? magnitude : -magnitude;
    }
    try {
      var profileIdc = sps[1];
      var chromaFormatIdc = 1;
      var separateColourPlane = 0;
      readUe(); // seq_parameter_set_id
      if ([100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].indexOf(profileIdc) >= 0) {
        chromaFormatIdc = readUe();
        if (chromaFormatIdc === 3) { separateColourPlane = readBit(); }
        readUe(); // bit_depth_luma_minus8
        readUe(); // bit_depth_chroma_minus8
        readBit(); // qpprime_y_zero_transform_bypass_flag
        if (readBit()) {
          var lists = (chromaFormatIdc !== 3) ? 8 : 12;
          for (var i = 0; i < lists; i += 1) {
            if (readBit()) {
              var size = (i < 6) ? 16 : 64;
              var last = 8;
              for (var j = 0; j < size; j += 1) {
                if (last !== 0) { last = (last + readSe() + 256) % 256; }
              }
            }
          }
        }
      }
      readUe(); // log2_max_frame_num_minus4
      var pocType = readUe();
      if (pocType === 0) {
        readUe();
      } else if (pocType === 1) {
        readBit();
        readSe();
        readSe();
        var cycle = readUe();
        for (var i = 0; i < cycle; i += 1) { readSe(); }
      }
      readUe(); // max_num_ref_frames
      readBit(); // gaps_in_frame_num_value_allowed_flag
      var widthMbs = readUe() + 1;
      var heightMapUnits = readUe() + 1;
      var frameMbsOnly = readBit();
      if (!frameMbsOnly) { readBit(); }
      readBit(); // direct_8x8_inference_flag
      var crop = [0, 0, 0, 0];
      if (readBit()) {
        crop = [readUe(), readUe(), readUe(), readUe()];
      }
      var chromaArrayType = separateColourPlane ? 0 : chromaFormatIdc;
      var subWidth = (chromaArrayType === 0) ? 1 : ((chromaArrayType === 3) ? 1 : 2);
      var subHeight = (chromaArrayType === 0) ? 1 : ((chromaArrayType === 1) ? 2 : 1);
      var cropUnitX = subWidth;
      var cropUnitY = subHeight * (2 - (frameMbsOnly ? 1 : 0));
      var width = widthMbs * 16 - cropUnitX * (crop[0] + crop[1]);
      var height = (2 - frameMbsOnly) * heightMapUnits * 16 - cropUnitY * (crop[2] + crop[3]);
      return [width, height];
    } catch (e) {
      return null;
    }
  }

  function decodeCursorData(bytes) {
    // CursorData: id=1(uint64), hotx=2(sint32), hoty=3, width=4, height=5, colors=6(bytes)
    var data = { id: 0, hotx: 0, hoty: 0, width: 0, height: 0, colors: null };
    fields(bytes).forEach(function (entry) {
      var value = entry[2];
      if (entry[0] === 1 && entry[1] === 0) { data.id = typeof value === 'number' ? value : 0; }
      else if (entry[0] === 2 && entry[1] === 0) { data.hotx = zigzag(value); }
      else if (entry[0] === 3 && entry[1] === 0) { data.hoty = zigzag(value); }
      else if (entry[0] === 4 && entry[1] === 0) { data.width = value; }
      else if (entry[0] === 5 && entry[1] === 0) { data.height = value; }
      else if (entry[0] === 6 && entry[1] === 2) { data.colors = value; }
    });
    return data;
  }

  function decodeCursorPosition(bytes) {
    var position = { x: 0, y: 0 };
    fields(bytes).forEach(function (entry) {
      if (entry[1] !== 0) { return; }
      if (entry[0] === 1) { position.x = zigzag(entry[2]); }
      else if (entry[0] === 2) { position.y = zigzag(entry[2]); }
    });
    return position;
  }

  // protobuf sint32 使用 zigzag 编码。
  function zigzag(value) {
    return (value >>> 1) ^ -(value & 1);
  }

  function handleCursorShape(data) {
    if (!data || !data.colors || !data.width || !data.height) { return; }
    var key = String(data.id);
    if (state.cursor.shapes.has(key)) {
      state.cursor.image = state.cursor.shapes.get(key);
    } else {
      var image = new ImageData(new Uint8ClampedArray(data.colors), data.width, data.height);
      var bitmap = document.createElement('canvas');
      bitmap.width = data.width;
      bitmap.height = data.height;
      bitmap.getContext('2d').putImageData(image, 0, 0);
      state.cursor.shapes.set(key, bitmap);
      // 形状缓存有界：超过 64 个时丢弃最早的。
      if (state.cursor.shapes.size > 64) {
        state.cursor.shapes.delete(state.cursor.shapes.keys().next().value);
      }
      state.cursor.image = bitmap;
    }
    state.cursor.hotx = data.hotx || 0;
    state.cursor.hoty = data.hoty || 0;
    state.cursor.visible = data.width > 0 && data.height > 0;
    drawOverlay();
  }

  function handleCursorPosition(position) {
    if (!position) { return; }
    state.cursor.x = position.x;
    state.cursor.y = position.y;
    drawOverlay();
  }

  // 光标单独绘制在叠加层：视频帧里不含光标，避免每帧重复编码。
  function drawOverlay() {
    if (overlay.width !== canvas.width || overlay.height !== canvas.height) {
      overlay.width = canvas.width;
      overlay.height = canvas.height;
    }
    overlayCtx.clearRect(0, 0, overlay.width, overlay.height);
    if (!state.cursor.visible || !state.cursor.image) { return; }
    overlayCtx.drawImage(
      state.cursor.image,
      state.cursor.x - state.cursor.hotx,
      state.cursor.y - state.cursor.hoty
    );
  }

  // 从 SPS NAL 推导 WebCodecs 的 codec 字符串。
  //
  // openh264 默认输出 baseline（profile_idc 0x42，约束 0xC0，level 0x1E），
  // 写死 `avc1.42E01E` 会与实际码流不符并导致解码器静默不输出帧。
  function codecStringFromSps(sps) {
    if (!sps || sps.length < 4) { return null; }
    var profile = sps[1], constraints = sps[2], level = sps[3];
    var hex = function (value) { return ('0' + value.toString(16)).slice(-2).toUpperCase(); };
    return 'avc1.' + hex(profile) + hex(constraints) + hex(level);
  }

  function findSps(annexB) {
    var i = 0;
    while (i + 4 <= annexB.length) {
      if (annexB[i] === 0 && annexB[i + 1] === 0 && annexB[i + 2] === 1) {
        var nal = annexB[i + 3] & 0x1f;
        if (nal === 7) {
          var end = annexB.length;
          for (var j = i + 3; j + 3 < annexB.length; j += 1) {
            if (annexB[j] === 0 && annexB[j + 1] === 0 && annexB[j + 2] === 1) { end = j; break; }
          }
          return annexB.subarray(i + 3, end);
        }
        i += 3;
      } else if (i + 5 <= annexB.length && annexB[i] === 0 && annexB[i + 1] === 0 &&
                 annexB[i + 2] === 0 && annexB[i + 3] === 1) {
        var nal4 = annexB[i + 4] & 0x1f;
        if (nal4 === 7) {
          var end4 = annexB.length;
          for (var k = i + 4; k + 3 < annexB.length; k += 1) {
            if (annexB[k] === 0 && annexB[k + 1] === 0 && annexB[k + 2] === 1) { end4 = k; break; }
          }
          return annexB.subarray(i + 4, end4);
        }
        i += 4;
      } else {
        i += 1;
      }
    }
    return null;
  }

  function ensureDecoder(width, height, firstFrame) {
    var codec = firstFrame ? codecStringFromSps(findSps(firstFrame)) : null;
    if (!codec && !state.decoder) { return null; }
    if (state.decoder && state.frameWidth === width && state.frameHeight === height &&
        (!codec || state.decoderCodec === codec)) { return state.decoder; }
    resetDecoder();
    state.frameWidth = width;
    state.frameHeight = height;
    canvas.width = width;
    canvas.height = height;
    fitStage();
    var support = window.VideoDecoder && window.VideoDecoder.isConfigSupported;
    if (!support) { say('浏览器不支持 WebCodecs H.264 解码', true); return null; }
    // 不提供 `description`：WebCodecs 按 Annex B（含起始码）解析，
    // 这正是 openh264 的输出格式，无需转换成 AVCC。
    var config = { codec: codec, optimizeForLatency: true };
    state.decoderCodec = codec;
    var __diag = window.__yoodeskDesk = window.__yoodeskDesk || {};
    __diag.codecConfigured = codec;
    window.VideoDecoder.isConfigSupported(config).then(function (result) {
      var __obs = window.__yoodeskDesk = window.__yoodeskDesk || {};
      __obs.codecSupported = !!result.supported;
      if (!result.supported) { say('浏览器不支持 ' + codec + ' 解码', true); }
    });
    var generation = state.decoderGeneration;
    var stream = state.stream;
    state.decoder = new window.VideoDecoder({
      output: function (frame) {
        try {
          if (generation !== state.decoderGeneration || state.stream !== stream) { return; }
          var pts = Math.round(frame.timestamp / 1000);
          var submitted = state.submitted.get(pts);
          state.submitted.delete(pts);
          var __obs = window.__yoodeskDesk = window.__yoodeskDesk || {};
          __obs.decodedFrames = (__obs.decodedFrames || 0) + 1;
          __obs.lastDecodedPts = pts;
          __obs.decodeMs = submitted === undefined ? null : performance.now() - submitted;
          state.stats.frames += 1;
          ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
          if (!state.hasVideoFrame) { focusDeskKeyboard(); }
          state.hasVideoFrame = true;
          el('screenshot').disabled = false;
          if (state.videoRecovering) { state.videoRecovering = false; say('画面已恢复'); }
          if (stream && stream.readyState === WebSocket.OPEN) {
            stream.send(JSON.stringify({type: 'video_received', pts: pts}));
          }
        } finally { frame.close(); }
      },
      error: function (error) {
        var __obs = window.__yoodeskDesk = window.__yoodeskDesk || {};
        __obs.decodeErrors = (__obs.decodeErrors || 0) + 1;
        __obs.lastDecodeError = String(error && error.message);
        say('解码失败：' + error.message, true);
        if (generation === state.decoderGeneration) { refreshVideo(); }
      }
    });
    state.decoder.configure(config);
    return state.decoder;
  }

  function handleFrame(bytes) {
    var frame;
    try {
      // 同一条业务流里既有视频帧也有光标消息，按字段区分。
      var parsed = decodeMessageFields(bytes);
      extensions.consume(parsed, fields);
      var __obs = window.__yoodeskDesk = window.__yoodeskDesk || {};
      if (parsed.video) { __obs.videoMessages = (__obs.videoMessages || 0) + 1; }
      if (parsed.cursorData) { __obs.cursorDataMessages = (__obs.cursorDataMessages || 0) + 1; }
      if (parsed.cursorPosition) {
        __obs.cursorPositionMessages = (__obs.cursorPositionMessages || 0) + 1;
        // 只观测实际后端消息；系统隐藏光标形状时仍可核对远端位置反馈。
        __obs.lastCursorPosition = { x: parsed.cursorPosition.x, y: parsed.cursorPosition.y };
      }
      if (parsed.cursorData) { handleCursorShape(parsed.cursorData); }
      if (parsed.cursorPosition) { handleCursorPosition(parsed.cursorPosition); }
      if (!parsed.video) {
        if (!__obs.noVideoShape) {
          __obs.noVideoShape = Array.from(bytes.subarray(0, 24)).map(function (b) {
            return b.toString(16).padStart(2, '0');
          }).join('');
        }
        return;
      }
      frame = parsed.video;
      __obs.lastReceivedPts = frame.pts;
      __obs.lastReceivedKey = frame.key;
      if (frame.key) { __obs.keyframes = (__obs.keyframes || 0) + 1; }
      if (!__obs.firstFrameDiag) {
        __obs.firstFrameDiag = {
          bytes: frame.data.length,
          key: frame.key,
          width: frame.width,
          height: frame.height,
        };
      }
    } catch (e) { say('视频消息解析失败：' + e.message, true); refreshVideo(); return; }
    if (!frame) { return; }
    state.stats.bytes += frame.data.length;
    if (state.waitingKey && !frame.key) { return; }
    // 首帧才可能包含 SPS；用它配置解码器，之后复用同一实例。
    // 尺寸取自 VideoFrame 自身（SPS 里没有画布信息），不能用 canvas 的当前尺寸：
    // 那是上一次的初始化值，会让解码器停在错误的几何上。
    var decoder = ensureDecoder(frame.width || canvas.width, frame.height || canvas.height, frame.data);
    if (!decoder) { return; }
    try {
      state.waitingKey = false;
      if (state.submitted.size >= 8) { refreshVideo(); return; }
      state.submitted.set(frame.pts, performance.now());
      decoder.decode(new window.EncodedVideoChunk({
        type: frame.key ? 'key' : 'delta',
        timestamp: (frame.pts || 0) * 1000,
        data: frame.data
      }));
    } catch (e) {
      var __obs = window.__yoodeskDesk = window.__yoodeskDesk || {};
      __obs.chunkErrors = (__obs.chunkErrors || 0) + 1;
      __obs.lastChunkError = String(e && e.message);
      say('提交解码失败：' + e.message, true);
      refreshVideo();
    }
  }

  // 编码器重建或参考链丢失后，旧回调不能确认新一代的视频帧。
  function resetDecoder() {
    state.decoderGeneration += 1;
    if (state.decoder && state.decoder.state !== 'closed') { state.decoder.close(); }
    state.decoder = null;
    state.decoderCodec = null;
    state.waitingKey = true;
    state.submitted.clear();
  }

  function refreshVideo() {
    state.videoRecovering = true;
    resetDecoder();
    if (state.stream && state.stream.readyState === WebSocket.OPEN) {
      state.stream.send(JSON.stringify({type: 'refresh_video'}));
    }
  }

  setInterval(function () {
    var now = performance.now();
    var elapsed = (now - state.stats.since) / 1000;
    var diagnostic = window.__yoodeskDesk = window.__yoodeskDesk || {};
    diagnostic.fps = state.stats.frames / elapsed;
    diagnostic.bitrateMbps = state.stats.bytes * 8 / elapsed / 1000000;
    state.stats.frames = 0;
    state.stats.bytes = 0;
    state.stats.since = now;
    if (el('video-stats')) {
      el('video-stats').textContent = state.sessionId ? diagnostic.fps.toFixed(0) + ' FPS · ' +
        diagnostic.bitrateMbps.toFixed(1) + ' Mbps · ' + (diagnostic.rttMs === undefined ? '—' : diagnostic.rttMs.toFixed(0)) + ' ms' : '等待连接';
      el('video-detail').textContent = canvas.width + ' × ' + canvas.height + ' · 解码 ' +
        (diagnostic.decodeMs == null ? '—' : diagnostic.decodeMs.toFixed(1)) + ' ms';
    }
    if (state.stream && state.stream.readyState === WebSocket.OPEN) {
      if (now - lastPong > 8000) { restartConnection(true, 'pong_timeout'); return; }
      if (connectorAccess && !transportRecoveryCleared && streamOpenedAt !== null &&
          now - streamOpenedAt >= 30000 && state.hasVideoFrame) {
        transportRecoveryCleared = true;
        try { sessionStorage.removeItem(transportRecoveryKey); } catch (_) {}
      }
      state.stats.ping = {id: Date.now(), at: now};
      if (state.events && state.events.readyState === WebSocket.OPEN) {
        state.events.send(JSON.stringify({type: 'ping', id: state.stats.ping.id}));
      }
    }
  }, 1000);

  function teardown() {
    // 关闭时取消旧输入 HTTP，新会话不必等待旧请求的网络超时。
    if (state.inputController) { state.inputController.abort(); state.inputController = null; }
    state.inputVersion += 1;
    pendingMouseMove = null;
    state.modePending = false;
    state.hasVideoFrame = false;
    el('screenshot').disabled = true;
    pressedKeyboard.clear();
    resetControlCopy();
    chat.stop();
    clipboard.stop();
    resetDecoder();
    state.videoRecovering = false;
    state.stats.ping = null;
    extensions.teardown();
    var stream = state.stream; var events = state.events;
    state.stream = null; state.events = null;
    if (stream) { stream.close(); }
    if (events) { events.close(); }
    state.sessionId = null;
    state.cursor = {shapes: new Map(), image: null, x: 0, y: 0, visible: false, hotx: 0, hoty: 0};
    overlayCtx.clearRect(0, 0, overlay.width, overlay.height);
    if (window.__yoodeskDesk) { window.__yoodeskDesk.sessionId = null; }
    setStatus('未连接');
    updateReadonlyControl();
  }

  function disconnect() {
    var id = state.sessionId;
    teardown();
    cleanupTail = cleanupTail.then(function () { return deleteSession(id); });
    return cleanupTail;
  }
  function stopConnection() {
    chat.clear();
    connectionWanted = false;
    viewerRecoveryCredential = '';
    stopViewerTransportRecovery();
    connectionGeneration += 1;
    clearTimeout(reconnectTimer);
    if (connectionController) { connectionController.abort(); }
    return disconnect();
  }
  function expireAccess() {
    stopAccessRenewal();
    stopConnection();
    state.csrf = '';
    state.accessInstance = '';
    el('credential').value = '';
    el('login-error').textContent = '登录已失效，请重新输入访问密码';
    showLogin(true);
  }
  function logout() {
    stopAccessRenewal();
    return stopConnection().then(function () {
      return request('POST', access + '/logout/');
    }).then(function () { state.csrf = ''; state.accessInstance = ''; showLogin(true); }).catch(function (error) {
      el('login-error').textContent = error.message;
      showLogin(true);
    });
  }
  window.addEventListener('offline', function () {
    if (transportRecoveryPending) { clearTimeout(transportRecoveryTimer); }
    else { restartConnection(true, 'offline'); }
  });
  window.addEventListener('online', function () {
    if (transportRecoveryPending) { resumeViewerTransportRecovery(0); }
    else { restartConnection(false, 'online'); }
  });
  if (connectorAccess) {
    new MutationObserver(function () {
      if (connectionWanted && document.documentElement.dataset.transportState === 'closed') {
        recoverViewerTransport();
      }
    }).observe(document.documentElement, {attributes: true, attributeFilter: ['data-transport-state']});
  }
  function closeOnPageHide() {
    var id = state.sessionId;
    // 卸载期间必须同步发出keepalive请求，不能排队等锁/前置GET。
    if (id) { rawRequest('DELETE', api + '/sessions/' + id + '/').catch(function () {}); }
    connectionWanted = false;
    viewerRecoveryCredential = '';
    stopViewerTransportRecovery();
    connectionGeneration += 1;
    clearTimeout(reconnectTimer);
    if (connectionController) { connectionController.abort(); }
    chat.clear();
    teardown();
  }
  window.addEventListener('pagehide', closeOnPageHide);
  window.addEventListener('pagehide', stopAccessRenewal);
  el('mode').addEventListener('change', changeViewerMode);
  el('display').addEventListener('change', restartConnection);
  // 只保存当前浏览器画布；只看模式也可用，图片不经过服务器。
  el('screenshot').addEventListener('click', function () {
    if (!state.hasVideoFrame || !state.sessionId) { say('尚未收到桌面画面，暂时无法截图', true); return; }
    try {
      canvas.toBlob(function (blob) {
        if (!blob) { say('截图生成失败，请重试', true); return; }
        var url = URL.createObjectURL(blob);
        var link = document.createElement('a');
        var brand = (document.documentElement.dataset.productName || productId).replace(/[\\/:*?"<>|]/g, '-');
        link.href = url;
        link.download = brand + '-desk-' + new Date().toISOString().replace(/[:.]/g, '-') + '.png';
        document.body.appendChild(link);
        try { link.click(); } finally { link.remove(); setTimeout(function () { URL.revokeObjectURL(url); }, 1000); }
      }, 'image/png');
    } catch (error) { say('截图失败：' + error.message, true); }
  });

  var inputTail = Promise.resolve();
  var inputPending = 0;
  var modeTail = Promise.resolve();
  function changeViewerMode() {
    state.viewerMode = el('mode').value;
    updateReadonlyControl();
    var version = ++state.inputVersion;
    pendingMouseMove = null;
    pressedKeyboard.clear();
    if (!state.sessionId || !state.inputController) { return; }
    var session = state.sessionId;
    var signal = state.inputController.signal;
    var drain = inputTail;
    state.modePending = true;
    window.__yoodeskDesk.modePending = true;
    updateReadonlyControl();
    // 不动视频/事件WS和解码器。先让已发送输入完成，丢弃旧排队键，再释放远端按键/鼠标。
    var paused = state.readonly ? extensions.pause().then(function () { return null; }, function (error) { return error; }) : Promise.resolve(null);
    modeTail = modeTail.then(async function () {
      await drain;
      var pauseError = await paused;
      if (pauseError) { throw pauseError; }
      if (state.sessionId !== session || signal.aborted) { return; }
      await request('POST', api + '/sessions/' + session + '/endpoint/', {action: 'input.release'}, signal);
      if (state.sessionId !== session || state.inputVersion !== version || signal.aborted) { return; }
      state.modePending = false;
      window.__yoodeskDesk.modePending = false;
      updateReadonlyControl();
      if (!state.readonly) { clipboard.start(); chat.start(); }
    }).catch(function (error) {
      if (state.sessionId !== session || state.inputVersion !== version || signal.aborted) { return; }
      // 清理失败时保持输入关闭；用户切换可重试，画面保持连接。
      say(error.message, true);
    });
  }
  function sendInput(payload) {
    return sendInputs([payload]);
  }
  // 只合并相邻且尚未发送的绝对坐标；按钮、滚轮和键盘都是顺序屏障。
  var pendingMouseMove = null;
  var queuedInputBatch = null;
  var inputBatchLimit = 32;
  var inputQueueLimit = 128;
  function sendInputs(payloads) {
    if (!payloads.length || !state.sessionId || state.readonly || state.modePending ||
        !state.inputController || transportRecoveryPending) { return Promise.resolve(); }
    var session = state.sessionId;
    var signal = state.inputController.signal;
    var version = state.inputVersion;
    function sameGeneration(batch) {
      return batch && batch.session === session && batch.version === version && batch.signal === signal;
    }
    var motion = payloads.length === 1 && payloads[0].type === 'mouse' && payloads[0].mask === 0;
    if (motion && pendingMouseMove && sameGeneration(pendingMouseMove.batch)) {
      pendingMouseMove.batch.events[pendingMouseMove.index] = payloads[0];
      return pendingMouseMove.batch.done;
    }
    pendingMouseMove = null;
    if (inputPending + payloads.length > inputQueueLimit) {
      // 无法保留按下/抬起顺序时结束会话，由服务端释放按键，避免丢掉抬起后卡键。
      restartConnection(true, 'input_queue_overflow');
      return Promise.resolve();
    }
    inputPending += payloads.length;
    var offset = 0;
    while (offset < payloads.length) {
      var batch = queuedInputBatch;
      if (!sameGeneration(batch) || batch.events.length === inputBatchLimit) {
        batch = queuedInputBatch = {session: session, version: version, signal: signal, events: []};
        batch.done = scheduleInputBatch(batch);
      }
      var count = Math.min(inputBatchLimit - batch.events.length, payloads.length - offset);
      if (motion) { pendingMouseMove = {batch: batch, index: batch.events.length}; }
      batch.events.push.apply(batch.events, payloads.slice(offset, offset + count));
      offset += count;
    }
    return batch.done;
  }
  function scheduleInputBatch(batch) {
    // 同时只发送一个请求；等响应期间收集后续事件，不让每次按键都等待网络往返。
    inputTail = inputTail.then(async function () {
      if (queuedInputBatch === batch) { queuedInputBatch = null; }
      if (pendingMouseMove && pendingMouseMove.batch === batch) { pendingMouseMove = null; }
      if (state.sessionId !== batch.session || state.readonly || state.inputVersion !== batch.version || batch.signal.aborted) { return; }
      var body = batch.events.length === 1 ? batch.events[0] : {type: 'batch', events: batch.events};
      await request('POST', api + '/sessions/' + batch.session + '/input/', body, batch.signal);
    }).catch(function (error) {
      if (state.sessionId !== batch.session || state.inputVersion !== batch.version || batch.signal.aborted) { return; }
      say(error.message, true);
      // 执行结果不明时不能重放。关闭旧会话以释放可能仍按下的键和按钮。
      restartConnection(true, 'input_batch_failed');
    }).finally(function () { inputPending -= batch.events.length; });
    return inputTail;
  }

  function canvasPoint(event) {
    var rect = canvas.getBoundingClientRect();
    var x = Math.round((event.clientX - rect.left) * (canvas.width / rect.width));
    var y = Math.round((event.clientY - rect.top) * (canvas.height / rect.height));
    return { x: Math.max(0, Math.min(canvas.width, x)), y: Math.max(0, Math.min(canvas.height, y)) };
  }
  var MOD = { 1: 0, 2: 0, 4: 0 };
  function mouseMask(kind, button) {
    var bits = { left: 0x01, right: 0x02, middle: 0x04, back: 0x08, forward: 0x10 }[button] || 0;
    return (bits << 3) | kind;
  }

  canvas.addEventListener('mousemove', function (event) {
    if (event.buttons === 0 && event.movementX === 0 && event.movementY === 0) { return; }
    var point = canvasPoint(event);
    sendInput({ type: 'mouse', mask: 0, x: point.x, y: point.y });
  });
  canvas.addEventListener('mousedown', function (event) {
    if (state.readonly || state.modePending) { return; }
    resetControlCopy();
    flushDeferredMeta();
    canvas.focus({preventScroll: true});
    var names = { 0: 'left', 1: 'middle', 2: 'right', 3: 'back', 4: 'forward' };
    var point = canvasPoint(event);
    sendInput({ type: 'mouse', mask: mouseMask(1, names[event.button]), x: point.x, y: point.y });
    event.preventDefault();
  });
  canvas.addEventListener('mouseup', function (event) {
    var names = { 0: 'left', 1: 'middle', 2: 'right', 3: 'back', 4: 'forward' };
    var point = canvasPoint(event);
    sendInput({ type: 'mouse', mask: mouseMask(2, names[event.button]), x: point.x, y: point.y });
  });
  canvas.addEventListener('contextmenu', function (event) { event.preventDefault(); });
  canvas.addEventListener('wheel', function (event) {
    if (state.readonly) { return; }
    var horizontal = Math.round(event.deltaX / 100);
    var vertical = Math.round(-event.deltaY / 100);
    if (horizontal === 0 && vertical === 0) { return; }
    sendInput({ type: 'mouse', mask: 3, x: horizontal, y: vertical });
    event.preventDefault();
  }, { passive: false });

  // HTTP 沿用兼容编号（Home=16、Insert=29）；原生桥按名称转换到 RustDesk 枚举。
  var CONTROL_KEYS = {
    Backspace: 2, Delete: 5, ArrowDown: 6, End: 7, Escape: 8,
    Home: 16, ArrowLeft: 17, PageDown: 20, PageUp: 21, Enter: 22, ArrowRight: 23,
    Shift: 25, ' ': 26, Tab: 27, ArrowUp: 28, Insert: 29, Control: 4, Alt: 1, Meta: 18,
    F1: 9, F2: 13, F3: 14, F4: 15, F10: 10, F11: 11, F12: 12
  };

  // 用物理键标识保留按下时的字符，Shift先抬起时仍释放同一个键；断开后丢弃旧状态。
  var pressedKeyboard = new Map();
  var controlCopyAt = null;
  var copyKeyDown = false;
  var rawCopyDown = false;
  function resetControlCopy() {
    controlCopyAt = null;
    copyKeyDown = false;
    rawCopyDown = false;
  }
  function isMacViewerKeyboard() {
    return /^mac/i.test(navigator.userAgentData?.platform || navigator.platform || '');
  }
  function focusDeskKeyboard() {
    // 登录表单隐藏或会话重连后，焦点可能落到 body；不抢聊天、菜单等控件的焦点。
    if (state.readonly || state.modePending || !state.sessionId || !state.capabilities?.keyboard) { return; }
    var active = document.activeElement;
    if (!active || active === document.body || active === document.documentElement ||
        el('login').contains(active)) {
      canvas.focus({preventScroll: true});
    }
  }
  // 画布只接受物理按键。拒绝浏览器能取消的文本合成，不影响其他控件。
  ['beforeinput', 'compositionstart', 'compositionupdate', 'compositionend'].forEach(function (type) {
    canvas.addEventListener(type, function (event) {
      if (event.cancelable) { event.preventDefault(); }
    });
  });
  function baseLayoutKey(code) {
    if (/^Key[A-Z]$/.test(code)) { return code.slice(-1).toLowerCase(); }
    if (/^Digit[0-9]$/.test(code)) { return code.slice(-1); }
    return {
      Backquote: '`', Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']',
      Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/'
    }[code];
  }
  // RustDesk 按原生键位区分数字区，不能把小键盘回车合并为主键盘回车。
  function keypadControl(event) {
    var code = event.code;
    if (/^Numpad[0-9]$/.test(code) && /^[0-9]$/.test(event.key)) {
      return 34 + Number(code.slice(-1));
    }
    if (code === 'NumpadDecimal' && event.key === 'Delete') { return undefined; }
    return {
      NumpadDecimal: 44, NumpadDivide: 45, NumpadMultiply: 46, NumpadSubtract: 47,
      NumpadAdd: 48, NumpadEnter: 49, NumpadEqual: 50, NumLock: 19
    }[code];
  }
  function flushDeferredMeta() {
    pressedKeyboard.forEach(function (key) {
      if (!key.deferred) { return; }
      key.deferred = false;
      sendKey({control: key.control, down: true});
    });
  }
  canvas.addEventListener('keydown', function (event) {
    if (state.readonly || state.modePending) { return; }
    // Caps Lock 由浏览器反映在字母大小写中，不再让远端锁键独立切换一次。
    if (event.code === 'CapsLock' || event.key === 'CapsLock') {
      resetControlCopy();
      event.preventDefault();
      return;
    }
    if (!event.isComposing && event.key !== 'Process' && handleShortcut(event)) { return; }
    var capsLock = event.getModifierState?.('CapsLock');
    var letterOnly = /^[a-z]$/i.test(event.key) && !event.ctrlKey && !event.metaKey && !event.altKey;
    if (letterOnly && !pressedKeyboard.has(event.code || event.key) &&
        (capsLock || (!event.shiftKey && /^[A-Z]$/.test(event.key)))) {
      var upper = /^[A-Z]$/.test(event.key);
      // macOS 布局入口需要未移位键码；大小写通过 Shift 发送。
      var layout = state.platform.os === 'macos' ? event.key.toLowerCase() : event.key;
      var shift = state.platform.os === 'macos' ? upper : (capsLock && event.shiftKey && !upper ? false : undefined);
      sendInputs(remapShortcutKeys({layout: layout}, [], [], shift));
      event.preventDefault();
      return;
    }
    // 本机输入法可能把 key 改成 Process；用 code 识别键，交给远端输入法处理。
    var controlKey = event.isComposing || event.key === 'Process' ? event.code : event.key;
    var modifiers = { ControlRight: 30, ShiftRight: 31, AltRight: 32, MetaRight: 33, OSRight: 33, MetaLeft: 18, OSLeft: 18 };
    var identity = event.code || event.key;
    var previous = pressedKeyboard.get(identity);
    var code = previous?.control ?? keypadControl(event) ?? modifiers[event.code] ??
      CONTROL_KEYS[controlKey === 'Space' ? ' ' : controlKey];
    if (code === undefined && /^(Shift|Control|Alt|Meta)(Left|Right)$/.test(controlKey)) {
      code = CONTROL_KEYS[controlKey.replace(/Left$|Right$/, '')];
    }
    if ((code === 18 || code === 33) && state.platform.os === 'windows' && isMacViewerKeyboard()) {
      // 等到下一键再发送 Win；Command+C/V 映射到 Ctrl 时不应误触发开始菜单。
      if (!previous) { pressedKeyboard.set(identity, {control: code, deferred: true}); }
      event.preventDefault();
      return;
    }
    if (![1, 4, 25, 30, 31, 32].includes(code)) { flushDeferredMeta(); }
    if (code !== undefined) {
      pressedKeyboard.set(identity, {control: code});
      sendInput({ type: 'key', control: code, down: true });
      event.preventDefault();
    } else {
      var base = baseLayoutKey(event.code);
      var ascii = event.key.length === 1 && event.key >= ' ' && event.key <= '~';
      var layout = ascii ? event.key : base;
      if (base && (event.isComposing || event.ctrlKey || event.metaKey ||
          (state.platform.os === 'macos' && (event.shiftKey || event.altKey)))) { layout = base; }
      var key = pressedKeyboard.get(identity) || (layout && {layout: layout});
      if (!key) { return; }
      pressedKeyboard.set(identity, key);
      sendKey({layout: key.layout, down: true});
      event.preventDefault();
    }
  });
  canvas.addEventListener('keyup', function (event) {
    if (event.code === 'KeyC' || (!event.code && event.key.toLowerCase() === 'c')) {
      copyKeyDown = false;
      rawCopyDown = false;
    }
    if (state.readonly || state.modePending) { return; }
    var identity = event.code || event.key;
    var key = pressedKeyboard.get(identity);
    if (key) {
      pressedKeyboard.delete(identity);
      if (key.deferred) {
        if (!key.usedForEdit) {
          sendInputs([{type: 'key', control: key.control, down: true}, {type: 'key', control: key.control, down: false}]);
        }
      } else if (key.control !== undefined) { sendKey({control: key.control, down: false}); }
      else { sendKey({layout: key.layout, down: false}); }
      event.preventDefault();
    }
  });
  function releasePressedKeyboard() {
    resetControlCopy();
    // 工具栏/聊天取得焦点时立即释放，不能把修饰键留在远端。
    var releases = Array.from(pressedKeyboard.values()).filter(function (key) { return !key.deferred; })
      .map(function (key) {
        return key.control === undefined ? {type: 'key', layout: key.layout, down: false}
          : {type: 'key', control: key.control, down: false};
      });
    pressedKeyboard.clear();
    if (releases.length) { sendInputs(releases); }
  }
  canvas.addEventListener('blur', releasePressedKeyboard);

  // 全屏且画布获得控制焦点时请求所有系统允许的物理键（含Win/Command）。
  // 权限响应可能晚到；请求串行且按会话/输入代次失效，不能在退出后重新锁住本机键盘。
  var keyboardLockVersion = 0;
  var keyboardLockKey = '';
  var keyboardLockTail = Promise.resolve();
  function updateKeyboardLock() {
    var keyboard = navigator.keyboard;
    var supported = !!(keyboard && keyboard.lock && keyboard.unlock);
    var wanted = !!(supported && document.fullscreenElement && document.hasFocus() && !document.hidden &&
      document.activeElement === canvas && state.sessionId && !state.readonly && !state.modePending &&
      state.capabilities && state.capabilities.keyboard);
    var key = wanted ? state.sessionId + ':' + state.inputVersion : '';
    var diagnostic = window.__yoodeskDesk = window.__yoodeskDesk || {};
    diagnostic.keyboardLockSupported = supported;
    if (diagnostic.keyboardLocked === undefined) { diagnostic.keyboardLocked = false; }
    if (key === keyboardLockKey) {
      if (!document.fullscreenElement) { el('fullscreen').title = '全屏'; }
      else if (!supported) { el('fullscreen').title = '退出全屏 · 当前浏览器不支持键盘锁定'; }
      return;
    }
    keyboardLockKey = key;
    var version = ++keyboardLockVersion;
    if (supported) { keyboard.unlock(); }
    diagnostic.keyboardLocked = false;
    diagnostic.keyboardLockError = null;
    el('fullscreen').title = document.fullscreenElement ? '退出全屏' : '全屏';
    if (!wanted) { return; }
    keyboardLockTail = keyboardLockTail.then(async function () {
      if (version !== keyboardLockVersion) { return; }
      try {
        await keyboard.lock();
        if (version !== keyboardLockVersion) { keyboard.unlock(); return; }
        diagnostic.keyboardLocked = true;
        el('fullscreen').title = '退出全屏 · 键盘已锁定（长按 Esc 退出）';
      } catch (error) {
        if (version !== keyboardLockVersion) { return; }
        diagnostic.keyboardLockError = error.message;
        el('fullscreen').title = '退出全屏 · 键盘锁定未获允许';
        say('未能锁定键盘：' + error.message, true);
      }
    });
  }
  window.addEventListener('blur', function () { releasePressedKeyboard(); updateKeyboardLock(); });
  window.addEventListener('focus', updateKeyboardLock);
  document.addEventListener('visibilitychange', function () {
    if (document.hidden) { releasePressedKeyboard(); }
    updateKeyboardLock();
  });
  document.addEventListener('focusin', updateKeyboardLock);
  document.addEventListener('focusout', function () { queueMicrotask(updateKeyboardLock); });
  canvas.tabIndex = 0;

  // Ctrl+C/X 正常发给远端应用；Ctrl+V 在自动同步完成后发完整粘贴组合键。
  function handleShortcut(event) {
    var caps = state.capabilities || {};
    var key = /^Key[A-Z]$/.test(event.code) ? event.code.slice(-1).toLowerCase() : event.key.toLowerCase();
    var targetMac = state.platform.os === 'macos';
    var viewerMac = isMacViewerKeyboard();
    var plainControlCopy = targetMac && key === 'c' && event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey;
    if (key === 'c') {
      if (!copyKeyDown && !event.repeat) {
        copyKeyDown = true;
        var now = performance.now();
        rawCopyDown = plainControlCopy && controlCopyAt !== null && now - controlCopyAt <= 1000;
        controlCopyAt = plainControlCopy ? now : null;
      }
      if (rawCopyDown) {
        if (event.repeat) { event.preventDefault(); return true; }
        return false;
      }
    } else if (!/^(Control|Meta|OS)(Left|Right)$/.test(event.code) &&
               !['control', 'meta', 'os'].includes(key)) {
      controlCopyAt = null;
    }
    if (viewerMac && state.platform.os === 'windows' && !event.ctrlKey &&
        event.metaKey !== event.altKey) {
      var navigation = event.metaKey ? {arrowleft: 16, arrowright: 7}
        : {arrowleft: 17, arrowright: 23, backspace: 2};
      var destination = navigation[key];
      if (Object.hasOwn(navigation, key)) {
        event.preventDefault();
        if (event.metaKey) { markDeferredMetaUsed(); }
        sendInputs(remapShortcutKeys({control: destination}, event.metaKey ? [18, 33] : [1, 32],
          event.altKey ? [4, 30] : []));
        return true;
      }
    }
    if (event.ctrlKey === event.metaKey || event.altKey || (event.metaKey && !viewerMac && !targetMac)) { return false; }
    var clipboardKey = ['c', 'x', 'v'].includes(key);
    var crossPlatform = targetMac !== viewerMac;
    var commonEdit = clipboardKey || key === 'a' || (crossPlatform && ['z', 's', 'f'].includes(key));
    var redoMac = crossPlatform && targetMac && event.ctrlKey && key === 'y' && !event.shiftKey;
    var redoWin = crossPlatform && !targetMac && event.metaKey && key === 'z' && event.shiftKey;
    if (!commonEdit && !redoMac && !redoWin) { return false; }
    if (event.metaKey && state.platform.os === 'windows' && viewerMac) {
      markDeferredMetaUsed();
    }
    if (key === 'v' && !event.shiftKey && caps.clipboard_write && !state.readonly) {
      event.preventDefault();
      if (!event.repeat) { clipboard.paste(function () { return editShortcutKeys('v'); }); }
      return true;
    }
    if ((key === 'c' || key === 'x') && !event.shiftKey && caps.clipboard_read && !event.repeat) {
      clipboard.copiedRemote();
    }
    var needsMapping = targetMac ? event.ctrlKey : event.metaKey;
    if (needsMapping) {
      event.preventDefault();
      if (!event.repeat) { sendInputs(editShortcutKeys(redoMac ? 'z' : redoWin ? 'y' : key, redoMac ? true : redoWin ? false : undefined)); }
      return true;
    }
    return false;
  }

  function editShortcutKeys(letter, shift) {
    var mac = state.platform.os === 'macos';
    return remapShortcutKeys({layout: letter}, mac ? [4, 30] : [18, 33],
      mac ? [18, 33] : [4, 30], shift);
  }
  function markDeferredMetaUsed() {
    pressedKeyboard.forEach(function (held) { if (held.deferred) { held.usedForEdit = true; } });
  }
  function remapShortcutKeys(key, source, target, shift) {
    var held = Array.from(pressedKeyboard.values()).filter(function (key) { return !key.deferred; })
      .map(function (key) { return key.control; });
    var wrong = source.filter(function (key) { return held.includes(key); });
    var temporary = target.length && !target.some(function (key) { return held.includes(key); });
    var heldShift = [25, 31].filter(function (key) { return held.includes(key); });
    if (shift === false) { wrong = wrong.concat(heldShift); }
    var temporaryShift = shift === true && !heldShift.length;
    function control(key, down) { return {type: 'key', control: key, down: down}; }
    var events = wrong.map(function (key) { return control(key, false); });
    if (temporary) { events.push(control(target[0], true)); }
    if (temporaryShift) { events.push(control(25, true)); }
    events.push(Object.assign({type: 'key', down: true}, key), Object.assign({type: 'key', down: false}, key));
    if (temporary) { events.push(control(target[0], false)); }
    if (temporaryShift) { events.push(control(25, false)); }
    return events.concat(wrong.map(function (key) { return control(key, true); }));
  }

  // 键盘事件走与鼠标相同的 input 端点，但字段名不同：服务端按 `control`/`layout`/`text` 区分。
  function sendKey(payload) {
    if (!state.sessionId || state.readonly) { return Promise.resolve(); }
    var body = { type: 'key' };
    Object.keys(payload).forEach(function (key) { body[key] = payload[key]; });
    return sendInput(body);
  }



  el('login-form').addEventListener('submit', function (event) {
    event.preventDefault();
    var credential = el('credential').value;
    if (credential.length < 6) { el('login-error').textContent = '密钥至少 6 个字符'; return; }
    login(credential).catch(function () {});
  });
  // 脚本完成初始化后才允许提交，避免资源未就绪时触发表单的默认导航。
  el('login-form').dataset.ready = '1';
  el('login-form').querySelector('button[type=submit]').disabled = false;
  el('logout').addEventListener('click', function () { logout().catch(function () {}); });

  var accessRenewalTimer;
  var accessRenewalGeneration = 0;
  function stopAccessRenewal() {
    accessRenewalGeneration += 1;
    clearTimeout(accessRenewalTimer);
  }
  function scheduleAccessRenewal(info) {
    clearTimeout(accessRenewalTimer);
    var generation = accessRenewalGeneration;
    var delay = Math.max(1000, Math.min(300000, (Date.parse(info.expires_at) - Date.now()) / 2));
    accessRenewalTimer = setTimeout(function () {
      if (!connectionWanted || !state.csrf || generation !== accessRenewalGeneration) { return; }
      request('POST', access + '/session/renew/').then(function (renewed) {
        if (connectionWanted && state.csrf && generation === accessRenewalGeneration) { scheduleAccessRenewal(renewed); }
      }).catch(function (error) {
        if (!connectionWanted || !state.csrf || generation !== accessRenewalGeneration) { return; }
        say('访问续期失败：' + error.message, true);
        accessRenewalTimer = setTimeout(function () {
          if (generation === accessRenewalGeneration) { scheduleAccessRenewal(info); }
        }, 10000);
      });
    }, delay);
  }
  function afterLogin(info) {
    if (!info.accessInstance) { throw new Error('服务器缺少访问会话实例标识，请更新服务端'); }
    state.accessInstance = info.accessInstance;
    state.controlAllowed = (info.scopes || []).includes('input.pointer') && (info.scopes || []).includes('input.keyboard');
    showLogin(false);
    connectionWanted = true;
    stopAccessRenewal();
    scheduleAccessRenewal(info);
    revealToolbar();
    restartConnection();
    return Promise.resolve();
  }
  function updateReadonlyControl() {
    var caps = state.capabilities || {};
    var canInput = state.controlAllowed && (!!caps.pointer || !!caps.keyboard);
    el('mode').disabled = !canInput;
    // worker 切换时能力会暂时不可用；不能因此覆盖用户选择的控制/只看意图。
    el('mode').value = canInput ? state.viewerMode : 'view';
    state.readonly = el('mode').value === 'view' || !canInput;
    ['audio', 'privacy', 'files', 'chat'].forEach(function (id) { el(id).disabled = !state.sessionId || state.readonly || state.modePending; });
    extensions.updateAvailability();
    if (state.readonly) { clipboard.stop(); chat.stop(); el('file-panel').hidden = true; el('chat-panel').hidden = true; }
    document.body.classList.toggle('view-only', state.readonly);
    if (window.__yoodeskDesk) { window.__yoodeskDesk.readOnly = state.readonly; }
    updateKeyboardLock();
  }

  if (invitationCredential) {
    var invitationInput = invitationCredential;
    invitationCredential = '';
    login(invitationInput).catch(function () {});
  } else { loadSession().then(function (info) {
    if (!info) { showLogin(true); return; }
    return afterLogin(info);
  }).catch(function (error) {
    if (transportRecoveryPending) { return; }
    el('login-error').textContent = error.message;
    showLogin(true);
  }); }
}());
