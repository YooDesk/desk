import assert from 'node:assert/strict';
import test from 'node:test';
import { ResumableCarrier } from '../packages/transport/resume.js';
import { resumableMux } from '../packages/transport/resume-mux.js';

test('invalid confirmation closes only the candidate', async () => {
  class Candidate extends EventTarget {
    readyState = 1;
    bufferedAmount = 0;
    sent = [];
    send(frame) { this.sent.push(frame); }
    close() { this.readyState = 3; }
    receive(frame) {
      this.dispatchEvent(new MessageEvent('message', { data: frame.slice().buffer }));
    }
  }
  const session = resumableMux();
  const primary = new Candidate();
  const first = session.confirm(1, primary);
  const [preface] = primary.sent;
  primary.receive(preface.subarray(0, 3));
  primary.receive(preface.subarray(3));
  await first;
  const candidate = new Candidate();
  const second = session.confirm(2, candidate);
  candidate.receive(preface);
  await assert.rejects(second, TypeError);
  assert.equal(candidate.readyState, 3);
  assert.equal(primary.readyState, 1);
  assert.equal(session.mux.closed, false);
  session.close();
});

test('keeps one yamux stream open across six simulated carrier replacements', async () => {
  const browser = resumableMux();
  const connector = resumableMux();
  const received = [];
  connector.mux.onStream = (stream) => {
    stream.onData = (data) => {
      received.push(...data);
      stream.consume(data.length);
    };
  };
  class Channel extends EventTarget {
    bufferedAmount = 0;
    send(frame) {
      queueMicrotask(() => {
        if (this.peer.readyState !== 'closed') {
          const fragments = this.readyState === 1 ? [frame.slice(0, 5), frame.slice(5)] : [frame.slice()];
          for (const fragment of fragments)
            this.peer.dispatchEvent(new MessageEvent('message', { data: fragment.buffer }));
        }
      });
    }
    close() {
      this.readyState = 'closed';
    }
  }
  const paths = ['wss', 'stun', 'turn', 'wss', 'turn', 'stun', 'wss'];
  const stream = browser.mux.openStream();
  try {
    for (const [index, path] of paths.entries()) {
      const left = new Channel();
      const right = new Channel();
      left.peer = right;
      right.peer = left;
      left.readyState = right.readyState = path === 'wss' ? 1 : 'open';
      connector.activate(index + 1, right);
      browser.activate(index + 1, left);
      await stream.writeAsync(Uint8Array.of(index));
    }
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(received, [0, 1, 2, 3, 4, 5, 6]);
    assert.equal(browser.mux.closed, false);
  } finally {
    browser.close();
    connector.close();
  }
});

test('replays unconfirmed bytes in both directions without duplicate delivery', () => {
  const leftBytes = [];
  const rightBytes = [];
  const left = new ResumableCarrier((bytes) => leftBytes.push(...bytes), () => {});
  const right = new ResumableCarrier((bytes) => rightBytes.push(...bytes), () => {});
  function connect() {
    const toLeft = [];
    const toRight = [];
    const leftLink = left.attach((frame) => Boolean(toRight.push(frame)));
    const rightLink = right.attach((frame) => Boolean(toLeft.push(frame)));
    return { toLeft, toRight, leftLink, rightLink };
  }
  let link = connect();
  for (let step = 0; step < 9; step += 1) {
    left.write(Uint8Array.of(step));
    right.write(Uint8Array.of(step + 10));
    while (link.toRight.length) link.rightLink.receive(link.toRight.shift());
    const old = link;
    link = connect();
    while (old.toLeft.length) old.leftLink.receive(old.toLeft.shift());
    old.leftLink.detach();
    link.leftLink.flush();
    link.rightLink.flush();
    while (link.toLeft.length || link.toRight.length) {
      while (link.toLeft.length) link.leftLink.receive(link.toLeft.shift());
      while (link.toRight.length) link.rightLink.receive(link.toRight.shift());
    }
  }
  assert.deepEqual(leftBytes, Array.from({ length: 9 }, (_, index) => index + 10));
  assert.deepEqual(rightBytes, Array.from({ length: 9 }, (_, index) => index));
});

test('stalls at the window limit and wakes after acknowledgment', () => {
  let wakeups = 0;
  const sender = new ResumableCarrier(() => {}, () => wakeups++);
  const frame = new Uint8Array(262144);
  assert.equal(sender.write(frame), true);
  assert.equal(sender.write(Uint8Array.of(1)), false);
  const link = sender.attach(() => true);
  link.flush();
  const ack = new Uint8Array(11);
  ack[0] = 1;
  new DataView(ack.buffer).setBigUint64(1, BigInt(frame.length));
  link.receive(ack);
  assert.equal(wakeups, 1);
  assert.equal(sender.write(Uint8Array.of(1)), true);
});
