import { carrierDecoder, ResumableCarrier } from './resume.js';
import { YamuxConnection, DC_MAX_FRAME_BODY } from './yamux.js';

export function resumableMux({ onCarrierLost, onClose } = {}) {
  let active;
  let generation = 0;
  const carrier = new ResumableCarrier((bytes) => mux.feed(bytes), () => mux.resumeWrites());
  const mux = new YamuxConnection(
    (frame) => carrier.write(frame, frame[1] === 0 && frame[3] === 0 ? 16384 : 0),
    { maxFrameBody: DC_MAX_FRAME_BODY, autoConsume: false, maxStreams: 256, maxSendQueue: 262144 },
  );
  mux.onClose = () => {
    carrier.close();
    active?.dispose();
    onClose?.();
  };

  function activate(next, channel) {
    const isOpen = () => channel.readyState === 1 || channel.readyState === 'open';
    if (mux.closed || !Number.isSafeInteger(next) || next <= generation || !isOpen())
      throw new TypeError('invalid carrier activation');
    channel.binaryType = 'arraybuffer';
    const isRTC = typeof channel.readyState === 'string';
    if (isRTC) channel.bufferedAmountLowThreshold = 32768;
    active?.dispose();
    generation = next;
    let sampler;
    const link = carrier.attach((frame) => {
      if (!isOpen()) return false;
      if (channel.bufferedAmount + frame.length > 65536) {
        if (!isRTC && sampler === undefined) sampler = setTimeout(sample, 4);
        return false;
      }
      try {
        channel.send(frame);
        return true;
      } catch {
        queueMicrotask(lost);
        return false;
      }
    });
    const sample = () => {
      sampler = undefined;
      link.flush();
    };
    const receive = carrierDecoder(link.receive);
    const message = (event) => {
      try {
        if (!(event.data instanceof ArrayBuffer)) throw new TypeError('binary carrier required');
        receive(new Uint8Array(event.data));
      } catch {
        mux._close();
      }
    };
    const lost = () => {
      if (active !== attachment) return;
      active = undefined;
      attachment.dispose();
      onCarrierLost?.(next);
    };
    const listeners = { message, bufferedamountlow: sample, close: lost, error: lost };
    const attachment = {
      dispose() {
        link.detach();
        clearTimeout(sampler);
        for (const [event, listener] of Object.entries(listeners))
          channel.removeEventListener(event, listener);
        channel.close();
      },
    };
    active = attachment;
    for (const [event, listener] of Object.entries(listeners)) channel.addEventListener(event, listener);
    link.flush();
    return receive;
  }

  function confirm(next, channel, signal = AbortSignal.timeout(5000)) {
    if (mux.closed || !Number.isSafeInteger(next) || next <= generation)
      throw new TypeError('invalid carrier generation');
    signal.throwIfAborted();
    channel.binaryType = 'arraybuffer';
    const preface = Uint8Array.of(89, 67, 82, 49, 0, 0, 0, 0, 0, 0, 0, 0);
    new DataView(preface.buffer).setBigUint64(4, BigInt(next));
    return new Promise((resolve, reject) => {
      let matched = 0;
      const cleanup = () => {
        signal.removeEventListener('abort', abort);
        channel.removeEventListener('message', message);
        channel.removeEventListener('close', closed);
        channel.removeEventListener('error', closed);
      };
      const fail = (error) => {
        cleanup();
        channel.close();
        reject(error);
      };
      const abort = () => fail(signal.reason);
      const closed = () => fail(new Error('candidate closed before confirmation'));
      const message = (event) => {
        if (!(event.data instanceof ArrayBuffer)) {
          fail(new TypeError('binary confirmation required'));
          return;
        }
        const bytes = new Uint8Array(event.data);
        const count = Math.min(bytes.length, preface.length - matched);
        for (let index = 0; index < count; index++) {
          if (bytes[index] !== preface[matched++]) {
            fail(new TypeError('invalid carrier confirmation'));
            return;
          }
        }
        if (matched !== preface.length) return;
        cleanup();
        try {
          const receive = activate(next, channel);
          receive(bytes.subarray(count));
          resolve();
        } catch (error) {
          if (generation === next) mux._close();
          fail(error);
        }
      };
      signal.addEventListener('abort', abort, { once: true });
      channel.addEventListener('message', message);
      channel.addEventListener('close', closed);
      channel.addEventListener('error', closed);
      try {
        channel.send(preface);
      } catch (error) {
        fail(error);
      }
    });
  }

  return { mux, activate, confirm, get attached() { return active !== undefined; }, close: () => mux._close() };
}
