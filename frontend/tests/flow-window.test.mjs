import assert from 'node:assert/strict';
import test from 'node:test';
import { YamuxConnection, YAMUX } from '../packages/transport/yamux.js';
import { ResumableCarrier } from '../packages/transport/resume.js';

test('only consuming streams borrow bounded receive credit and slow bodies retain it', () => {
  const mux = new YamuxConnection(() => true, { autoConsume: false });
  const original = mux.spareReceiveCredit;
  const control = mux.openStream();
  const data = mux.openStream();
  data._onFrame(0, new Uint8Array(65536));
  data.consume(65536);
  assert.equal(data.recvWindow, 4 * 1024 * 1024);
  assert.equal(control.recvWindow, YAMUX.DEFAULT_CREDIT);
  assert.equal(mux.spareReceiveCredit, 0);
  while (mux.streams.size < 256) mux.openStream();
  assert.equal(
    [...mux.streams.values()].reduce((n, s) => n + s.recvWindow + s.pendingReceived, 0),
    256 * YAMUX.DEFAULT_CREDIT + original,
  );
  data._onFrame(0, new Uint8Array(65536));
  data.closeWrite();
  data._onFrame(YAMUX.FLAG_FIN, new Uint8Array());
  assert.equal(mux.spareReceiveCredit, 0, 'unread response still owns its receive credit');
  data.consume(65536);
  assert.equal(mux.spareReceiveCredit, original);
  data._terminate();
  data._terminate();
  assert.equal(mux.spareReceiveCredit, original, 'retirement must not refund twice');
  mux._close();
});

test('resumable senders share a bounded spare and return it on close and ACK', () => {
  const senders = Array.from(
    { length: 10 },
    () =>
      new ResumableCarrier(
        () => {},
        () => {},
      ),
  );
  const block = new Uint8Array(65536);
  let total = 0;
  for (const sender of senders) {
    let sent = 0;
    while (sender.write(block)) {
      total += block.length;
      sent += block.length;
    }
    assert(sent >= 262144 && sent <= 4 * 1024 * 1024);
  }
  assert(total <= 10 * 262144 + 32 * 1024 * 1024);
  for (const sender of senders) sender.close();
  const sender = new ResumableCarrier(
    () => {},
    () => {},
  );
  let size = 0;
  while (sender.write(block)) size += block.length;
  assert.equal(size, 4 * 1024 * 1024);
  const link = sender.attach(() => true);
  link.flush();
  const ack = new Uint8Array(11);
  ack[0] = 1;
  new DataView(ack.buffer).setBigUint64(1, BigInt(size));
  link.receive(ack);
  assert(sender.write(block));
  sender.close();
});
