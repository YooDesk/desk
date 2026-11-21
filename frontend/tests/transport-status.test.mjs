import assert from 'node:assert/strict';
import test from 'node:test';
import { describeTransport } from '../packages/transport/status.js';

test('labels actual relay and direct paths without exposing network addresses', () => {
  assert.deepEqual(describeTransport('websocket', {}), {label: 'TCP', detail: '网关 WebSocket 转发', direct: false});
  assert.equal(describeTransport('stun', {local: 'host', remote: 'srflx', protocol: 'udp'}).label, 'UDP P2P');
  assert.equal(describeTransport('stun', {local: 'host', remote: 'host', protocol: 'tcp'}).label, 'TCP P2P');
  assert.equal(describeTransport('turn', {local: 'relay', remote: 'relay', protocol: 'udp', relayProtocol: 'udp'}).label, 'UDP');
  assert.equal(describeTransport('turn', {local: 'relay', remote: 'relay', protocol: 'udp', relayProtocol: 'tcp'}).label, 'TCP');
  assert.equal(describeTransport('turn', {local: 'relay', remote: 'relay', protocol: 'udp', relayProtocol: 'tls'}).label, 'TCP');
  assert.equal(describeTransport('stun', null).direct, null);
  assert.equal(describeTransport('stun', {local: 'host', remote: 'host'}).label, '检测中');
});
