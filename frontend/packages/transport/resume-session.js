import { bindMux } from './channel.js';
import { RemoteHTTP, validateContext } from './http.js';
import { connectRelay } from './relay.js';
import { resumableMux } from './resume-mux.js';
import { connectRTC, ConnectivityError } from './rtc.js';
import { RemoteSocket } from './socket.js';

const turnRefreshLeadMs = 75000;
const turnRefreshRetryMs = 5000;

export async function connectResumableSession({ context, grant, prepareCarrier, policy, onProbeError, signal }) {
  validateContext(context);
  if (typeof prepareCarrier !== 'function' || policy?.allowWSS !== true)
    throw new TypeError('authorized WSS bootstrap required');
  const lifetime = new AbortController();
  let active;
  let turnRefreshTimer;
  const session = resumableMux({ onCarrierLost: recover, onClose() {
    lifetime.abort();
    clearTimeout(turnRefreshTimer);
    active?.close();
  } });
  const outerSignal = signal ? AbortSignal.any([lifetime.signal, signal]) : lifetime.signal;
  let activePath;
  const attempts = [];
  const probes = [];
  let pending;
  let recovering;
  let bound = false;
  let generation = 0;
  const http = new RemoteHTTP(session.mux, context);

  async function switchTo(path) {
    return switchCarrier(path);
  }

  async function switchCarrier(path, previousTurnExpiry) {
    if (!['websocket', 'stun', 'turn', 'mixed'].includes(path) || (['turn', 'mixed'].includes(path) && policy.allowTURN !== true))
      throw new TypeError('unapproved carrier path');
    if (pending) throw new Error('carrier negotiation already running');
    pending = dialCarrier(path, previousTurnExpiry);
    try {
      await pending;
    } catch (error) {
      attempts.push({ path, outcome: 'failed', reason: error.name });
      if (attempts.length > 40) attempts.shift();
      throw error;
    } finally {
      pending = undefined;
    }
  }

  async function dialCarrier(path, previousTurnExpiry) {
    const deadline = AbortSignal.timeout(['turn', 'mixed'].includes(path) ? 35000 : 15000);
    const signal = AbortSignal.any([outerSignal, deadline]);
    signal.throwIfAborted();
    const next = ++generation;
    const dial = path === 'websocket' ? connectRelay : connectRTC;
    let selectedPath = path;
    let turnExpiry;
    const replacement = await Promise.resolve(prepareCarrier(path, { generation: next, context, signal })).then((approved) => {
      signal.throwIfAborted();
      turnExpiry = approved.turnCredentialExpiresAt;
      if (previousTurnExpiry !== undefined && (!Number.isFinite(turnExpiry) || turnExpiry <= previousTurnExpiry))
        throw new ConnectivityError('replacement TURN credentials not yet available');
      return dial({
        ...approved,
        onDiagnostic(report) {
          probes.push(report);
          if (probes.length > 8) probes.shift();
        },
        signal,
        context,
        timeoutMs: path === 'mixed' ? 35000 : undefined,
        async acceptCarrier(channel, selectedPair) {
          const pair = await selectedPair();
          const relayed = pair.local === 'relay' || pair.remote === 'relay';
          if (path === 'mixed') selectedPath = relayed ? 'turn' : 'stun';
          if ((path === 'stun' && relayed) || (path === 'turn' && (pair.local !== 'relay' || pair.remote !== 'relay')))
            throw new ConnectivityError('unexpected selected ICE path');
          await session.confirm(next, channel, signal);
        },
      });
    }).catch((error) => {
      if (path === 'stun' && deadline.aborted && !outerSignal.aborted && error === deadline.reason)
        throw new ConnectivityError('direct WebRTC probe deadline exceeded');
      throw error;
    });
    if (outerSignal.aborted) {
      replacement.close();
      outerSignal.throwIfAborted();
    }
    active?.close();
    active = replacement;
    activePath = selectedPath;
    clearTimeout(turnRefreshTimer);
    if (selectedPath === 'turn' && Number.isFinite(turnExpiry)) scheduleTURNRefresh(replacement, turnExpiry);
    attempts.push({ path: selectedPath, outcome: 'connected' });
    if (attempts.length > 40) attempts.shift();
  }

  function scheduleTURNRefresh(carrier, expiry, retry = false) {
    const remaining = expiry - Date.now();
    if (remaining <= 0 || outerSignal.aborted || active !== carrier || !session.attached) return;
    const delay = retry ? Math.min(turnRefreshRetryMs, remaining) : Math.max(0, remaining - turnRefreshLeadMs);
    turnRefreshTimer = setTimeout(async () => {
      if (expiry <= Date.now() || outerSignal.aborted || active !== carrier || !session.attached) return;
      if (!pending && !recovering) {
        try {
          await switchCarrier('turn', expiry);
          return;
        } catch (error) {
          if (!outerSignal.aborted) onProbeError?.(error);
        }
      }
      scheduleTURNRefresh(carrier, expiry, true);
    }, delay);
  }

  function recover() {
    clearTimeout(turnRefreshTimer);
    active?.close();
    if (!bound || recovering || session.attached || outerSignal.aborted) return;
    recovering = Promise.resolve().then(async () => {
      await pending?.catch(() => {});
      const paths = ['websocket', 'websocket', 'stun', ...(policy.allowTURN === true ? ['turn'] : [])];
      let failure;
      for (const path of paths) {
        if (session.attached || outerSignal.aborted) return;
        try {
          await switchTo(path);
        } catch (error) {
          failure = error;
        }
      }
      if (!session.attached && !outerSignal.aborted) {
        session.close();
        if (failure) onProbeError?.(failure);
      }
    }).finally(() => { recovering = undefined; });
  }

  outerSignal.addEventListener('abort', session.close, { once: true });

  try {
    await switchTo('websocket');
    const bindingTimeout = setTimeout(session.close, 15000);
    try {
      await bindMux(session.mux, grant);
      bound = true;
      if (!session.attached) recover();
    } finally {
      clearTimeout(bindingTimeout);
    }
  } catch (error) {
    session.close();
    throw error;
  }
  const probe = async () => {
    try {
      await switchTo('stun');
    } catch (error) {
      if (recovering) await recovering;
      else if (!outerSignal.aborted && error instanceof ConnectivityError)
        await switchTo(policy.allowTURN === true ? 'mixed' : 'stun');
      else throw error;
    }
  };
  const background = probe().catch((error) => {
    if (!lifetime.signal.aborted) onProbeError?.(error);
  });
  return {
    mux: session.mux,
    get path() { return activePath; },
    attempts,
    http,
    background,
    switchTo,
    async selectedPair() {
      const pair = await active.selectedPair();
      return { ...pair, diagnostics: { pairs: [], dataSent: 0, dataReceived: 0,
        ...pair.diagnostics, probes: probes.map(p => ({ ...p })) } };
    },
    openSocket: (url, protocols) => new RemoteSocket(http, url, protocols),
    close: session.close,
  };
}
