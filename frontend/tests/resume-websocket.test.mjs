import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { carrierDecoder, ResumableCarrier } from '../packages/transport/resume.js';

const fixturePath = process.env.YOODESK_RESUME_FIXTURE;

test('Connector and JS replay bytes across real loopback WebSockets', { skip: !fixturePath, timeout: 10000 }, async () => {
  const child = spawn(fixturePath, [], { stdio: ['ignore', 'pipe', 'inherit'] });
  const lines = createInterface({ input: child.stdout });
  const sockets = [];
  let notify;
  const received = [];
  const carrier = new ResumableCarrier((bytes) => {
    received.push(...bytes);
    notify?.();
  }, () => {});
  try {
    const [port] = await once(lines, 'line');
    async function attach(generation) {
      const socket = new WebSocket('ws://127.0.0.1:' + port);
      sockets.push(socket);
      socket.binaryType = 'arraybuffer';
      await once(socket, 'open');
      const preface = Uint8Array.of(89, 67, 82, 49, 0, 0, 0, 0, 0, 0, 0, generation);
      const confirmation = once(socket, 'message');
      socket.send(preface);
      const [echo] = await confirmation;
      assert.deepEqual(new Uint8Array(echo.data), preface);
      const link = carrier.attach((frame) => {
        if (socket.readyState !== WebSocket.OPEN) return false;
        socket.send(frame);
        return true;
      });
      const decode = carrierDecoder(link.receive);
      socket.addEventListener('message', (event) => decode(new Uint8Array(event.data)));
      link.flush();
      return socket;
    }
    const old = await attach(1);
    carrier.write(Uint8Array.of(1, 2, 3));
    old.close();
    carrier.write(Uint8Array.of(4, 5, 6));
    await attach(2);
    if (received.length < 6) await new Promise((resolve) => {
      notify = () => { if (received.length >= 6) resolve(); };
    });
    assert.deepEqual(received, [1, 2, 3, 4, 5, 6]);
  } finally {
    carrier.close();
    for (const socket of sockets) socket.close();
    lines.close();
    child.kill();
  }
});
