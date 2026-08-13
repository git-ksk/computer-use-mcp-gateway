import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { createUsageSidecar } from './server.mjs';

const moduleSpecifier = process.env.CUMG_MCP_USAGE_CONTROL_MODULE;
if (!moduleSpecifier) throw new Error('CUMG_MCP_USAGE_CONTROL_MODULE is required for sidecar tests');

let server;
let base;

before(async () => {
  ({ server } = await createUsageSidecar({
    moduleSpecifier,
    limit: 2,
    reservationTtlMs: 60_000,
    maxRetainedOperations: 100,
    maxRetainedBudgetKeys: 100,
  }));
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  base = `http://127.0.0.1:${server.address().port}`;
});

after(async () => {
  await new Promise(resolve => server.close(resolve));
});

async function post(path, body) {
  const response = await fetch(`${base}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  return { status: response.status, body: await response.json() };
}

const principal = { issuer: 'https://issuer.example', subject: 'alice' };

test('MemoryUsageStore allows, settles zero, and reclaims pre-dispatch reservation', async () => {
  const first = await post('/v1/reserve', { operationId: 'pre-deny-1', principal, tool: 'click' });
  assert.equal(first.status, 200);
  assert.equal(first.body.allowed, true);
  assert.ok(first.body.reservationId);
  assert.deepEqual(await post('/v1/settle', {
    reservationId: first.body.reservationId,
    actualUnits: 0,
    outcome: 'authorization_denied',
  }), { status: 200, body: { ok: true } });

  const second = await post('/v1/reserve', { operationId: 'after-release-1', principal, tool: 'click' });
  assert.equal(second.body.allowed, true);
});

test('completed operations consume one unit and quota exhaustion denies', async () => {
  const first = await post('/v1/reserve', { operationId: 'charge-1', principal, tool: 'click' });
  assert.equal(first.body.allowed, true);
  assert.deepEqual(await post('/v1/mark-liable', { reservationId: first.body.reservationId }), {
    status: 200,
    body: { ok: true },
  });
  assert.deepEqual(await post('/v1/settle', {
    reservationId: first.body.reservationId,
    actualUnits: 1,
    outcome: 'completed',
  }), { status: 200, body: { ok: true } });

  const denied = await post('/v1/reserve', { operationId: 'charge-2', principal, tool: 'click' });
  assert.equal(denied.status, 200);
  assert.equal(denied.body.allowed, false);
  assert.equal(denied.body.reason, 'quota_exceeded');
});

test('duplicate operationId fails closed in the same replay scope', async () => {
  const other = { issuer: 'https://issuer.example', subject: 'bob' };
  const first = await post('/v1/reserve', { operationId: 'duplicate-1', principal: other, tool: 'click' });
  assert.equal(first.body.allowed, true);
  const duplicate = await post('/v1/reserve', { operationId: 'duplicate-1', principal: other, tool: 'click' });
  assert.equal(duplicate.body.allowed, false);
  assert.equal(duplicate.body.reason, 'duplicate_operation');
});

test('MemoryUsageStore state is lost when the sidecar process/store is recreated', async () => {
  async function isolated() {
    const instance = await createUsageSidecar({
      moduleSpecifier,
      limit: 1,
      reservationTtlMs: 60_000,
      maxRetainedOperations: 100,
      maxRetainedBudgetKeys: 100,
    });
    await new Promise((resolve, reject) => {
      instance.server.once('error', reject);
      instance.server.listen(0, '127.0.0.1', resolve);
    });
    return {
      ...instance,
      base: `http://127.0.0.1:${instance.server.address().port}`,
    };
  }
  async function isolatedPost(instance, path, body) {
    const response = await fetch(`${instance.base}${path}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
    return { status: response.status, body: await response.json() };
  }
  const resetPrincipal = { issuer: 'https://issuer.example', subject: 'restart-user' };
  const first = await isolated();
  const admitted = await isolatedPost(first, '/v1/reserve', {
    operationId: 'before-restart', principal: resetPrincipal, tool: 'click',
  });
  assert.equal(admitted.body.allowed, true);
  await isolatedPost(first, '/v1/mark-liable', { reservationId: admitted.body.reservationId });
  await isolatedPost(first, '/v1/settle', {
    reservationId: admitted.body.reservationId, actualUnits: 1, outcome: 'completed',
  });
  const exhausted = await isolatedPost(first, '/v1/reserve', {
    operationId: 'exhausted-before-restart', principal: resetPrincipal, tool: 'click',
  });
  assert.equal(exhausted.body.allowed, false);
  await new Promise(resolve => first.server.close(resolve));

  const second = await isolated();
  const afterRestart = await isolatedPost(second, '/v1/reserve', {
    operationId: 'after-restart', principal: resetPrincipal, tool: 'click',
  });
  assert.equal(afterRestart.body.allowed, true);
  await new Promise(resolve => second.server.close(resolve));
});

test('bridge rejects accidental payload or bearer-like extra fields', async () => {
  const response = await post('/v1/reserve', {
    operationId: 'payload-leak-1',
    principal,
    tool: 'click',
    args: { text: 'secret payload' },
  });
  assert.equal(response.status, 400);
  assert.equal(response.body.error, 'invalid_request');
});
