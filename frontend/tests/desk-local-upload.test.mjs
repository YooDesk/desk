import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';

const source = await readFile(new URL('../../browser_static/app.js', import.meta.url), 'utf8');
const snippet = source.slice(source.indexOf('  function uploadLocalFile('), source.indexOf('  function showLogin('));
function fixture(connectorAccess = false) {
  const sent = [], fetched = [];
  class XHR {
    constructor() { this.headers = {}; sent.push(this); }
    open(method, url) { this.method = method; this.url = url; }
    setRequestHeader(name, value) { this.headers[name] = value; }
    send(body) { this.body = body; this.sends = (this.sends || 0) + 1; }
    abort() { this.aborts = (this.aborts || 0) + 1; this.onabort?.(); }
    getAllResponseHeaders() { return 'Content-Type: application/json\r\nX-Access-Instance: instance-1\r\n'; }
    respond(status, payload) { this.status = status; this.responseText = JSON.stringify(payload); this.onload(); }
  }
  const context = vm.createContext({XMLHttpRequest: XHR, Headers, Response, DOMException, TypeError,
    connectorAccess, state: {accessInstance: 'instance-1'}, connectionWanted: false,
    api: '/desk/api', access: '/access',
    headers: () => ({'X-CSRF-Token': 'csrf', 'X-Request-ID': 'initial'}),
    apiError: (payload, fallback) => payload?.error?.message || fallback,
    fetch: async (url, init) => { fetched.push({url, init}); return Response.json({data: {offset: 4}}); },
  });
  vm.runInContext(snippet, context);
  return {context, sent, fetched};
}

test('LAN upload preserves raw bytes, credentials, request binding and response parsing', async () => {
  const {context, sent, fetched} = fixture();
  const body = Uint8Array.of(0, 128, 255, 0);
  const result = context.rawRequest('POST', '/desk/api/endpoint/', body, undefined, 'request-1', 'instance-1', false, 7);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].body, body);
  assert.equal(sent[0].withCredentials, true);
  assert.equal(sent[0].headers['X-Desk-Transfer-ID'], '7');
  assert.equal(sent[0].headers['X-Request-ID'], 'request-1');
  assert.equal(sent[0].headers['X-Access-Instance'], 'instance-1');
  assert.equal(sent[0].headers['X-CSRF-Token'], 'csrf');
  assert.equal(sent[0].headers['Content-Type'], 'application/octet-stream');
  sent[0].respond(200, {data: {offset: 4}});
  assert.equal((await result).offset, 4);
  assert.equal(fetched.length, 0);
});

test('Connector upload stays on the SDK fetch path', async () => {
  const {context, sent, fetched} = fixture(true);
  assert.equal((await context.rawRequest('POST', '/desk/api/endpoint/', Uint8Array.of(1), undefined, 'request-1', 'instance-1', false, 7)).offset, 4);
  assert.equal(sent.length, 0);
  assert.equal(fetched.length, 1);
  assert.equal(fetched[0].init.headers['X-Desk-Transfer-ID'], '7');
});

test('upload cancellation sends no pre-aborted request and stops an active request once', async () => {
  const {context, sent} = fixture();
  const before = new AbortController(); before.abort();
  await assert.rejects(context.rawRequest('POST', '/endpoint/', Uint8Array.of(1), before.signal, 'r', 'i', false, 7), error => error.name === 'AbortError');
  assert.equal(sent.length, 0);
  const active = new AbortController();
  const result = context.rawRequest('POST', '/endpoint/', Uint8Array.of(1), active.signal, 'r', 'i', false, 7);
  active.abort();
  await assert.rejects(result, error => error.name === 'AbortError');
  assert.equal(sent[0].aborts, 1);
  assert.equal(sent[0].sends, 1);
});

test('server rejection retains CSRF classification and network errors never replay data', async () => {
  const {context, sent} = fixture();
  const response = context.rawRequest('POST', '/endpoint/', Uint8Array.of(1), undefined, 'r', 'i', false, 7);
  sent[0].respond(400, {error: {message: 'expired CSRF', code: 'invalid_request', retryable: true, details: {stage: 'csrf', field: 'x-csrf-token'}}});
  await assert.rejects(response, error => error.status === 400 && error.csrfRejected === true);
  const network = context.rawRequest('POST', '/endpoint/', Uint8Array.of(1), undefined, 'r2', 'i', false, 7);
  sent[1].onerror();
  await assert.rejects(network, /network failure/);
  assert.equal(sent[1].sends, 1);
  assert.equal(sent.length, 2);
});
