import { dataChannelMux, bindMux } from './channel.js';
import { RemoteHTTP, validateContext } from './http.js';
import { RemoteSocket } from './socket.js';

export class ConnectivityError extends Error {}

// signaling 由授权控制面提供：exchangeOffer 返回 answer，onCandidate 订阅对端候选。
// 业务 SDK 不接触账号 Cookie/刷新凭据，也不自行补公共 ICE 节点。
export async function connectRTC({
  configuration,
  signaling,
  context,
  grant,
  getGrant,
  acceptCarrier,
  timeoutMs = 15000,
  signal,
}) {
  validateContext(context);
  if (!configuration || !signaling || typeof signaling.onClose !== 'function' || timeoutMs <= 0)
    throw new TypeError('explicit RTC configuration and signaling required');
  const pc = new RTCPeerConnection(configuration);
  const label = acceptCarrier ? 'raccess.resume.v1' : 'raccess.data.v2';
  const channel = pc.createDataChannel(label, {
    ordered: true,
    protocol: label,
  });
  let resolve;
  let reject;
  let done = false;
  let mux;
  let unsubscribe = () => {};
  let unsubscribeClosed = () => {};
  let unsubscribeLocal = () => {};
  const abort = () => fail(signal.reason);
  const ready = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  const cleanup = () => {
    clearTimeout(timer);
    unsubscribe();
    unsubscribeClosed();
    unsubscribeLocal();
    signal?.removeEventListener('abort', abort);
  };
  const closePeer = () => {
    cleanup();
    pc.close();
  };
  const selectedPair = async () => {
    const stats = await pc.getStats();
    const transport = [...stats.values()].find(
      (entry) => entry.type === 'transport' && entry.selectedCandidatePairId,
    );
    const pair = transport && stats.get(transport.selectedCandidatePairId);
    const local = pair && stats.get(pair.localCandidateId);
    const remote = pair && stats.get(pair.remoteCandidateId);
    if (!pair || !local || !remote) throw new Error('selected ICE candidate pair unavailable');
    return {
      local: local.candidateType,
      remote: remote.candidateType,
      protocol: local.protocol || remote.protocol || null,
      relayProtocol: local.relayProtocol || remote.relayProtocol || null,
      rttMs: Number.isFinite(pair.currentRoundTripTime) ? pair.currentRoundTripTime * 1000 : null,
      bytesSent: pair.bytesSent,
      bytesReceived: pair.bytesReceived,
      diagnostics: {
        pairs: [...stats.values()].filter((s) => s.type === 'candidate-pair').slice(0, 64).map((s) => ({
          selected: s.id === pair.id, nominated: !!s.nominated, state: s.state,
          sent: s.bytesSent || 0, received: s.bytesReceived || 0,
          rttMs: Number.isFinite(s.currentRoundTripTime) ? s.currentRoundTripTime * 1000 : null,
        })),
        dataSent: [...stats.values()].filter((s) => s.type === 'data-channel').reduce((sum, s) => sum + (s.bytesSent || 0), 0),
        dataReceived: [...stats.values()].filter((s) => s.type === 'data-channel').reduce((sum, s) => sum + (s.bytesReceived || 0), 0),
      },
    };
  };
  const fail = (error) => {
    if (done) {
      cleanup();
      mux?._close();
      pc.close();
      return;
    }
    done = true;
    cleanup();
    channel.close();
    pc.close();
    reject(error);
  };
  const timer = setTimeout(
    () => fail(new ConnectivityError('ICE connection deadline exceeded')),
    timeoutMs,
  );
  signal?.addEventListener('abort', abort, { once: true });
  if (signal?.aborted) {
    abort();
    return ready;
  }
  pc.addEventListener('connectionstatechange', () => {
    if (pc.connectionState === 'failed') fail(new ConnectivityError('ICE connection failed'));
  });
  const closeUnsubscribe = signaling.onClose(fail);
  if (typeof closeUnsubscribe !== 'function') {
    fail(new TypeError('signaling close observation must support unsubscribe'));
    return ready;
  }
  unsubscribeClosed = closeUnsubscribe;
  if (done) return ready;
  let remoteSet = false;
  const pending = [];
  let candidateChain = Promise.resolve();
  let candidateCount = 0;
  const candidateUnsubscribe = signaling.onCandidate((candidate) => {
    if (candidate?.candidate?.length > 4096 || ++candidateCount > 64) {
      fail(new RangeError('ICE candidate limit exceeded'));
      return;
    }
    if (!remoteSet) {
      pending.push(candidate);
      return;
    }
    candidateChain = candidateChain.then(() => pc.addIceCandidate(candidate)).catch(fail);
  });
  if (typeof candidateUnsubscribe !== 'function') {
    fail(new TypeError('signaling must support unsubscribe'));
    return ready;
  }
  unsubscribe = candidateUnsubscribe;
  channel.addEventListener(
    'open',
    () => {
      void (async () => {
        if (done) return;
        if (acceptCarrier) {
          channel.addEventListener('close', closePeer, { once: true });
          await acceptCarrier(channel, selectedPair);
          if (done) return;
          done = true;
          unsubscribe();
          unsubscribeLocal();
          clearTimeout(timer);
          signal?.removeEventListener('abort', abort);
          resolve({ selectedPair, close: closePeer });
          return;
        }
        mux = dataChannelMux(channel, () => {
          cleanup();
          pc.close();
        });
        // 握手期间可能已经续租；绑定前交接原续租维护，使用已确认的最新 grant。
        const leaseVersion = await bindMux(mux, getGrant ? await getGrant() : grant);
        if (done) {
          mux._close();
          return;
        }
        done = true;
        clearTimeout(timer);
        signal?.removeEventListener('abort', abort);
        resolve({
          mux,
          http: new RemoteHTTP(mux, context),
          openSocket(url, protocols) {
            return new RemoteSocket(this.http, url, protocols);
          },
          leaseVersion,
          close() {
            cleanup();
            mux._close();
            pc.close();
          },
          selectedPair,
        });
      })().catch(fail);
    },
    { once: true },
  );
  const trickle = typeof signaling.sendCandidate === 'function';
  let offerSent = false;
  let localCount = 0;
  const localCandidates = [];
  if (trickle) {
    const gathered = ({ candidate }) => {
      if (done && pc.connectionState === 'closed') return;
      try {
        if (++localCount > 64) throw new RangeError('local ICE candidate limit exceeded');
        const value = candidate?.toJSON() ?? null;
        if (offerSent) signaling.sendCandidate(value);
        else localCandidates.push(value);
      } catch (error) {
        fail(error);
      }
    };
    pc.addEventListener('icecandidate', gathered);
    unsubscribeLocal = () => pc.removeEventListener('icecandidate', gathered);
  }
  void (async () => {
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    if (!trickle && pc.iceGatheringState !== 'complete')
      await new Promise((yes, no) => {
        const change = () => {
          if (pc.iceGatheringState === 'complete') {
            remove();
            yes();
          }
        };
        const state = () => {
          if (pc.connectionState === 'closed') {
            remove();
            no(new ConnectivityError('ICE closed during gathering'));
          }
        };
        const remove = () => {
          pc.removeEventListener('icegatheringstatechange', change);
          pc.removeEventListener('connectionstatechange', state);
        };
        pc.addEventListener('icegatheringstatechange', change);
        pc.addEventListener('connectionstatechange', state);
        change();
      });
    if (done) return;
    const answering = signaling.exchangeOffer(trickle ? offer : pc.localDescription);
    // The offer is sent synchronously by the signaling adapter before candidates.
    offerSent = true;
    if (trickle) {
      try {
        for (const candidate of localCandidates.splice(0)) signaling.sendCandidate(candidate);
      } catch (error) {
        void answering.catch(() => {});
        throw error;
      }
    }
    const answer = await answering;
    if (done) return;
    await pc.setRemoteDescription(answer);
    for (const candidate of pending.splice(0)) {
      candidateChain = candidateChain.then(() => pc.addIceCandidate(candidate));
    }
    remoteSet = true;
    await candidateChain;
  })().catch(fail);
  return ready;
}
