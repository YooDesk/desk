const headerSize = 11;
const payloadLimit = 16384 - headerSize;
const windowSize = 262144;
const maxWindowSize = 4 * 1024 * 1024;
let spareWindow = 32 * 1024 * 1024;

function encode(type, offset, payload) {
  const frame = new Uint8Array(headerSize + (payload?.length ?? 0));
  frame[0] = type;
  new DataView(frame.buffer).setBigUint64(1, BigInt(offset));
  new DataView(frame.buffer).setUint16(9, payload?.length ?? 0);
  if (payload) frame.set(payload, headerSize);
  return frame;
}

export function carrierDecoder(receive) {
  const buffer = new Uint8Array(16384);
  const view = new DataView(buffer.buffer);
  let used = 0;
  let target = headerSize;
  return (bytes) => {
    while (bytes.length) {
      const count = Math.min(bytes.length, target - used);
      buffer.set(bytes.subarray(0, count), used);
      bytes = bytes.subarray(count);
      used += count;
      if (used !== target) continue;
      if (target === headerSize) {
        const size = view.getUint16(9);
        if (size > payloadLimit) throw new RangeError('carrier record too large');
        target += size;
        if (used !== target) continue;
      }
      receive(buffer.subarray(0, target));
      used = 0;
      target = headerSize;
    }
  };
}

export class ResumableCarrier {
  #carrier;
  #pending = [];
  #written = 0;
  #sent = 0;
  #acked = 0;
  #received = 0;
  #closed = false;
  #borrowed = 0;

  constructor(deliver, writable) {
    this.deliver = deliver;
    this.writable = writable;
  }

  attach(send) {
    if (this.#closed) throw new Error('logical carrier closed');
    const carrier = { send, cursor: this.#acked, ack: -1 };
    this.#carrier = carrier;
    return {
      flush: () => this.#flush(carrier),
      receive: (frame) => this.#receive(carrier, frame),
      detach: () => {
        if (this.#carrier === carrier) this.#carrier = undefined;
      },
    };
  }

  write(bytes, reserve = 0) {
    const end = this.#written + bytes.length;
    if (this.#closed) return false;
    if (this.#written - this.#acked >= windowSize / 2) {
      const growth = Math.min(spareWindow, maxWindowSize - windowSize - this.#borrowed);
      this.#borrowed += growth;
      spareWindow -= growth;
    }
    if (end - this.#acked > windowSize + this.#borrowed - reserve) return false;
    if (!Number.isSafeInteger(end)) throw new RangeError('carrier offset exhausted');
    for (let start = 0; start < bytes.length; start += payloadLimit) {
      const payload = bytes.slice(start, start + payloadLimit);
      this.#pending.push({ offset: this.#written, payload });
      this.#written += payload.length;
    }
    if (this.#carrier) this.#flush(this.#carrier);
    return true;
  }

  close() {
    this.#closed = true;
    this.#carrier = undefined;
    this.#pending = [];
    this.#releaseWindow();
  }

  #releaseWindow() {
    spareWindow += this.#borrowed;
    this.#borrowed = 0;
  }

  #flush(carrier) {
    if (this.#carrier !== carrier) return;
    if (carrier.ack !== this.#received) {
      if (!carrier.send(encode(1, this.#received))) return;
      carrier.ack = this.#received;
    }
    for (const { offset, payload } of this.#pending) {
      const end = offset + payload.length;
      const start = Math.max(offset, carrier.cursor, this.#acked);
      if (start >= end) continue;
      if (!carrier.send(encode(0, start, payload.subarray(start - offset)))) return;
      carrier.cursor = end;
      this.#sent = Math.max(this.#sent, end);
    }
  }

  #receive(carrier, frame) {
    if (this.#carrier !== carrier) return;
    if (!(frame instanceof Uint8Array) || frame.length < headerSize)
      throw new TypeError('invalid carrier frame');
    const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
    const offset = Number(view.getBigUint64(1));
    if (view.getUint16(9) !== frame.length - headerSize)
      throw new RangeError('invalid carrier length');
    if (!Number.isSafeInteger(offset)) throw new RangeError('invalid carrier offset');
    if (frame[0] === 1) {
      if (frame.length !== headerSize || offset > this.#sent)
        throw new RangeError('invalid carrier acknowledgment');
      if (offset <= this.#acked) return;
      this.#acked = offset;
      const remaining = this.#pending.findIndex(({ offset, payload }) => offset + payload.length > this.#acked);
      this.#pending = remaining < 0 ? [] : this.#pending.slice(remaining);
      if (!this.#pending.length) this.#releaseWindow();
      this.#flush(carrier);
      this.writable();
      return;
    }
    const size = frame.length - headerSize;
    const end = offset + size;
    if (frame[0] !== 0 || !size || size > payloadLimit || offset > this.#received || !Number.isSafeInteger(end))
      throw new RangeError('invalid carrier data');
    if (end > this.#received) {
      const previous = this.#received;
      this.#received = end;
      try {
        this.deliver(frame.subarray(headerSize + previous - offset));
      } catch (error) {
        this.close();
        throw error;
      }
    }
    this.#flush(carrier);
  }
}
