#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { isIP } from 'node:net';

const MAX_BODY_BYTES = 8 * 1024;
const DEFAULT_RESERVATION_TTL_MS = 60_000;
const DEFAULT_MAX_RETAINED_OPERATIONS = 10_000;
const DEFAULT_MAX_RETAINED_BUDGET_KEYS = 10_000;
const ALLOWED_OUTCOMES = new Set([
  'authorization_denied',
  'invalid_arguments',
  'pre_dispatch_rejected',
  'pre_dispatch_no_effect',
  'cancelled_before_dispatch',
  'completed',
  'proven_no_effect',
  'dispatched_conservative',
  'cancelled_after_dispatch',
]);

function positiveInt(name, fallback) {
  const raw = process.env[name] ?? fallback;
  if (raw === undefined || !/^\d+$/.test(String(raw)) || Number(raw) <= 0 || !Number.isSafeInteger(Number(raw))) {
    throw new Error(`${name} must be a positive integer`);
  }
  return Number(raw);
}

function parseBind(raw = '127.0.0.1:8787') {
  const bracketed = raw.match(/^\[([^\]]+)\]:(\d+)$/);
  const plain = raw.match(/^([^:]+):(\d+)$/);
  const match = bracketed ?? plain;
  if (!match) throw new Error('CUMG_USAGE_BIND must be a literal loopback IP plus port');
  const host = match[1];
  const port = Number(match[2]);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('invalid CUMG_USAGE_BIND port');
  if ((isIP(host) === 4 && host !== '127.0.0.1') || (isIP(host) === 6 && host !== '::1') || isIP(host) === 0) {
    throw new Error('CUMG_USAGE_BIND must use 127.0.0.1 or ::1');
  }
  return { host, port };
}

function exactKeys(value, keys) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function nonEmptyString(value, max = 1024) {
  return typeof value === 'string' && value.length > 0 && value.length <= max;
}

function principalBudgetKey(issuer, subject) {
  return `runtime:principal:${createHash('sha256').update(issuer).update('\0').update(subject).digest('hex')}`;
}

async function readJson(req) {
  let size = 0;
  const chunks = [];
  for await (const chunk of req) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) throw Object.assign(new Error('request_too_large'), { statusCode: 413 });
    chunks.push(chunk);
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  } catch {
    throw Object.assign(new Error('invalid_json'), { statusCode: 400 });
  }
}

function json(res, statusCode, body) {
  const payload = JSON.stringify(body);
  res.writeHead(statusCode, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(payload),
    'cache-control': 'no-store',
  });
  res.end(payload);
}

export async function createUsageSidecar(options = {}) {
  const moduleSpecifier = options.moduleSpecifier ?? process.env.CUMG_MCP_USAGE_CONTROL_MODULE ?? 'mcp-usage-control';
  const { MemoryUsageStore, UsageControl } = await import(moduleSpecifier);
  const limit = options.limit ?? positiveInt('CUMG_USAGE_LIMIT_PER_PRINCIPAL');
  const reservationTtlMs = options.reservationTtlMs ?? positiveInt('CUMG_USAGE_RESERVATION_TTL_MS', DEFAULT_RESERVATION_TTL_MS);
  const maxRetainedOperations = options.maxRetainedOperations ?? positiveInt('CUMG_USAGE_MAX_RETAINED_OPERATIONS', DEFAULT_MAX_RETAINED_OPERATIONS);
  const maxRetainedBudgetKeys = options.maxRetainedBudgetKeys ?? positiveInt('CUMG_USAGE_MAX_RETAINED_BUDGET_KEYS', DEFAULT_MAX_RETAINED_BUDGET_KEYS);

  const store = new MemoryUsageStore({ maxRetainedOperations, maxRetainedBudgetKeys });
  const policy = {
    quote(request) {
      return {
        decision: 'allow',
        units: 1,
        reservationTtlMs,
        budget: {
          key: principalBudgetKey(request.principal.tenantId ?? '', request.principal.id),
          limit,
        },
      };
    },
  };
  const control = new UsageControl(store, policy, { defaultReservationTtlMs: reservationTtlMs });

  const server = createServer(async (req, res) => {
    // Never log request headers or bodies here. The bridge contract intentionally
    // contains only verified principal identity, operation ID, tool, reservation ID,
    // bounded outcome, and 0/1 units.
    try {
      if (req.method === 'GET' && req.url === '/healthz') {
        return json(res, 200, { ok: true, store: 'memory', durable: false, stats: store.stats() });
      }
      if (req.method !== 'POST') return json(res, 404, { error: 'not_found' });
      const body = await readJson(req);

      if (req.url === '/v1/reserve') {
        if (!exactKeys(body, ['operationId', 'principal', 'tool'])
          || !exactKeys(body.principal, ['issuer', 'subject'])
          || !nonEmptyString(body.operationId, 256)
          || !nonEmptyString(body.principal.issuer, 2048)
          || !nonEmptyString(body.principal.subject, 1024)
          || !nonEmptyString(body.tool, 256)) {
          return json(res, 400, { error: 'invalid_request' });
        }
        const admission = await control.reserve({
          operationId: body.operationId,
          principal: { id: body.principal.subject, tenantId: body.principal.issuer },
          tool: body.tool,
          args: {},
        });
        if (!admission.allowed) {
          return json(res, 200, {
            allowed: false,
            reason: admission.reason,
            ...(admission.remaining === undefined ? {} : { remaining: admission.remaining }),
          });
        }
        return json(res, 200, {
          allowed: true,
          reservationId: admission.lease.reservation.id,
          renewAfterMs: Math.max(1, Math.floor(reservationTtlMs / 3)),
        });
      }

      if (req.url === '/v1/mark-liable') {
        if (!exactKeys(body, ['reservationId']) || !nonEmptyString(body.reservationId, 512)) {
          return json(res, 400, { error: 'invalid_request' });
        }
        await store.markLiable({ reservationId: body.reservationId });
        return json(res, 200, { ok: true });
      }

      if (req.url === '/v1/renew') {
        if (!exactKeys(body, ['reservationId']) || !nonEmptyString(body.reservationId, 512)) {
          return json(res, 400, { error: 'invalid_request' });
        }
        await store.renew({ reservationId: body.reservationId, ttlMs: reservationTtlMs });
        return json(res, 200, { ok: true });
      }

      if (req.url === '/v1/settle') {
        if (!exactKeys(body, ['actualUnits', 'outcome', 'reservationId'])
          || !nonEmptyString(body.reservationId, 512)
          || !Number.isInteger(body.actualUnits)
          || (body.actualUnits !== 0 && body.actualUnits !== 1)
          || !ALLOWED_OUTCOMES.has(body.outcome)) {
          return json(res, 400, { error: 'invalid_request' });
        }
        await store.settle({
          reservationId: body.reservationId,
          actualUnits: body.actualUnits,
          outcome: body.outcome,
        });
        return json(res, 200, { ok: true });
      }

      return json(res, 404, { error: 'not_found' });
    } catch (error) {
      const status = Number.isInteger(error?.statusCode) ? error.statusCode : 503;
      return json(res, status, { error: status === 503 ? 'usage_state_unavailable' : error.message });
    }
  });

  return { server, store, control };
}

async function main() {
  const bind = parseBind(process.env.CUMG_USAGE_BIND);
  const { server } = await createUsageSidecar();
  server.listen(bind.port, bind.host, () => {
    // Configuration only; no principal, reservation, operation, request body, or headers.
    console.log(`CUMG MCPUsage sidecar listening on ${bind.host}:${bind.port} (MemoryUsageStore, non-durable)`);
  });
  const close = () => server.close(() => process.exit(0));
  process.once('SIGINT', close);
  process.once('SIGTERM', close);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(`CUMG MCPUsage sidecar startup failed: ${error?.message ?? 'unknown_error'}`);
    process.exit(1);
  });
}
