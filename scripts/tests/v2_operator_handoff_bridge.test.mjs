import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { PassThrough } from "node:stream";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { AppendOnlyAbandonmentAudit, HandoffBridge, TerminalPtyHandoffBridge, WebRtcHandoffSurface, handoffBeginFailureCode, serveStdio } from "../v2_operator_handoff_bridge.mjs";

const HANDOFF_ROOT = process.env.CUMG_V2_HANDOFF_ROOT;
const api = HANDOFF_ROOT
  ? await import(pathToFileURL(path.join(HANDOFF_ROOT, "dist/index.js")).href)
  : await import("./v2_operator_handoff_test_double.mjs");

class FakeNativeSurface {
  constructor() {
    this.kind = "native";
    this.sessionID = "session-12345678";
    this.revoked = [];
  }

  create(intervention, binding) {
    this.intervention = { ...intervention };
    this.binding = structuredClone(binding);
    return `http://127.0.0.1:48771/takeover/${this.sessionID}`;
  }

  async handle(request, principalBinding) {
    assert.equal(principalBinding, this.binding.principalBinding);
    const pathname = new URL(request.url).pathname;
    const match = /^\/takeover\/api\/(claim|reconnect|done|cancel)\//.exec(pathname);
    if (!match) return new Response("{}", { status: 404 });
    return new Response(JSON.stringify({ ok: true, operation: match[1] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }

  async revoke(interventionId) {
    this.revoked.push(interventionId);
  }

  revokeUnclaimed(interventionId) {
    this.revoked.push(interventionId);
  }

  lifecycle(pathname) {
    const match = /^\/takeover\/api\/(claim|reconnect|done|cancel)\//.exec(pathname);
    if (!match) return undefined;
    if (match[1] === "claim") return "claim";
    if (match[1] === "done" || match[1] === "cancel") return "complete";
    return undefined;
  }
}

class FakeWebRtcSurface {
  constructor() {
    this.kind = "webrtc";
    this.sessionID = "webrtc-session-12345678";
    this.revoked = [];
  }

  create(intervention, binding) {
    this.intervention = { ...intervention };
    this.binding = structuredClone(binding);
    return `https://handoff.example/takeover/${this.sessionID}`;
  }

  async handle(request, principalBinding) {
    assert.equal(principalBinding, this.binding.principalBinding);
    const pathname = new URL(request.url).pathname;
    if (pathname === `/takeover/${this.sessionID}` || pathname === "/takeover/webrtc-client.js") {
      return new Response("ok", { status: 200 });
    }
    const match = /^\/takeover\/api\/(webrtc-prepare-claim|webrtc-prepare-reconnect|webrtc-connect|webrtc-suspend|done|cancel)\//.exec(pathname);
    if (!match) return new Response("{}", { status: 404 });
    return new Response(JSON.stringify({ ok: true, operation: match[1] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }

  async revoke(interventionId) { this.revoked.push(interventionId); }
  revokeUnclaimed(interventionId) { this.revoked.push(interventionId); }

  lifecycle(pathname) {
    const match = /^\/takeover\/api\/(webrtc-connect|done|cancel)\//.exec(pathname);
    if (!match) return undefined;
    return match[1] === "webrtc-connect" ? "connect" : "complete";
  }
}

function fixture(surface = new FakeNativeSurface()) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "cumg-handoff-test-"));
  const checkpoint = path.join(root, "handoff.json");
  const key = Buffer.alloc(32, 7);
  let now = 1_800_000_000_000;
  const store = new api.SignedFileHandoffCheckpointStore(checkpoint, key, () => now);
  const audit = new AppendOnlyAbandonmentAudit(path.join(root, "audit", "abandonment.jsonl"));
  const bridge = new HandoffBridge(api, store, () => now, surface, audit);
  const request = {
    action: "admit_agent",
    protocol: 1,
    principal_binding: "1".repeat(64),
    device_binding: "2".repeat(64),
    generation: 116,
    capability_revision: 5,
    exact_window: {
      context_binding: "3".repeat(64),
      process_id: 1234,
      window_id: 5678,
    },
    verification_candidate: false,
  };
  return {
    root, checkpoint, store, audit, bridge, surface, native: surface, request,
    tick(ms = 1) { now += ms; },
    cleanup() { fs.rmSync(root, { recursive: true, force: true }); },
  };
}


function managedAuthority(request) {
  const { action: _action, ...authority } = request;
  return authority;
}

function contextBinding(contextId) {
  return createHash("sha256")
    .update("cumg/operator-handoff/context/v1\0", "utf8")
    .update(contextId, "utf8")
    .digest("hex");
}

function ctl(bridge, action, intervention, extra = {}) {
  return bridge.handle({
    action,
    ...(intervention ? { intervention_id: intervention.intervention_id, epoch: intervention.epoch } : {}),
    ...extra,
  });
}

async function nativeControl(bridge, locator, operation) {
  const url = new URL(locator);
  const sessionID = url.pathname.split("/").at(-1);
  return bridge.handleSurface(new Request(
    `${url.origin}/takeover/api/${operation}/${sessionID}`,
    {
      method: "POST",
      headers: {
        "x-takeover-native-client": "1",
        "x-takeover-client": "dogfood-client-binding-aaaaaaaa",
        "x-mcp-takeover-capability": "a".repeat(48),
      },
    },
  ));
}

async function webRtcControl(bridge, locator, operation) {
  const url = new URL(locator);
  const sessionID = url.pathname.split("/").at(-1);
  return bridge.handleSurface(new Request(
    `${url.origin}/takeover/api/${operation}/${sessionID}`,
    {
      method: "POST",
      headers: {
        origin: url.origin,
        "x-takeover-client": "dogfood-webrtc-binding-aaaaaaaa",
        "x-mcp-takeover-capability": "b".repeat(48),
      },
      body: operation === "webrtc-connect" ? JSON.stringify({ type: "offer", sdp: "v=0\r\n" }) : undefined,
    },
  ));
}


test("first-class stdio runtime keeps the compatibility bridge protocol off the Agent socket path", async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  let response = "";
  output.setEncoding("utf8");
  output.on("data", (chunk) => { response += chunk; });
  const serving = serveStdio({
    handle(request) {
      assert.deepEqual(request, { action: "status" });
      return { ok: true, runtime: "ready" };
    },
  }, input, output);
  input.end('{"action":"status"}\n');
  await serving;
  assert.equal(response, '{"ok":true,"runtime":"ready"}\n');
});

test("managed control requires and uses the explicit CUMG-selected exact Window binding", async () => {
  const f = fixture();
  try {
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "allow" });
    assert.deepEqual(f.bridge.handleManaged({ action: "begin" }), { ok: false });

    const { action: _ignored, ...authority } = f.request;
    authority.exact_window = { ...authority.exact_window, window_id: authority.exact_window.window_id + 9 };
    const begun = f.bridge.handleManaged({ action: "begin", authority });
    assert.equal(begun.ok, true);
    assert.equal(begun.status, "awaiting_human");
    assert.equal(f.bridge.activeBinding.exactWindow.windowId, authority.exact_window.window_id);
    assert.notEqual(f.bridge.activeBinding.exactWindow.windowId, f.bridge.latestObservation.exactWindow.windowId);
  } finally {
    f.cleanup();
  }
});

test("Agent -> Native Human -> verifying -> explicit same-window resume preserves exclusive authority", async () => {
  const f = fixture();
  try {
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "allow" });
    const begun = ctl(f.bridge, "begin");
    assert.equal(begun.ok, true);
    assert.equal(begun.status, "awaiting_human");
    assert.ok(begun.native_locator);
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    const claim = await nativeControl(f.bridge, begun.native_locator, "claim");
    assert.equal(claim.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "human_active");
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    // Disconnect/no Done leaves Human authority active; Agent is not restored automatically.
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    const staleGeneration = structuredClone(f.request);
    staleGeneration.generation = 115;
    assert.deepEqual(f.bridge.handle(staleGeneration), { ok: true, decision: "deny" });

    const done = await nativeControl(f.bridge, begun.native_locator, "done");
    assert.equal(done.status, 200);
    const verifying = f.bridge.handle({ action: "status" }).active;
    assert.equal(verifying.status, "verifying");
    assert.ok(verifying.epoch > begun.epoch);
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    const verification = structuredClone(f.request);
    verification.verification_candidate = true;
    const admitted = f.bridge.handle(verification);
    assert.equal(admitted.decision, "verification");
    assert.equal(admitted.intervention_id, verifying.intervention_id);
    assert.equal(admitted.epoch, verifying.epoch);

    const wrongWindow = structuredClone(verification);
    wrongWindow.exact_window.window_id += 1;
    assert.deepEqual(f.bridge.handle(wrongWindow), { ok: true, decision: "deny" });

    assert.deepEqual(f.bridge.handle({
      ...verification,
      action: "report_verification",
      intervention_id: admitted.intervention_id,
      epoch: admitted.epoch - 1,
      satisfied: true,
    }), { ok: false });

    assert.deepEqual(f.bridge.handle({
      ...verification,
      action: "report_verification",
      intervention_id: admitted.intervention_id,
      epoch: admitted.epoch,
      satisfied: false,
    }), { ok: true });
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "verifying");

    assert.deepEqual(f.bridge.handle({
      ...verification,
      action: "report_verification",
      intervention_id: admitted.intervention_id,
      epoch: admitted.epoch,
      satisfied: true,
    }), { ok: true });
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "ready_to_resume");
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    const ready = f.bridge.handle({ action: "status" }).active;
    assert.equal(ctl(f.bridge, "request_resume", ready).ok, true);
    assert.deepEqual(f.bridge.handle(staleGeneration), { ok: true, decision: "deny" });
    assert.deepEqual(f.bridge.handle(wrongWindow), { ok: true, decision: "deny" });
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "ready_to_resume");

    // Only a fresh CUMG admission for the exact same window may consume explicit resume.
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "allow" });
    assert.equal(f.bridge.handle({ action: "status" }).active, null);
  } finally {
    f.cleanup();
  }
});

test("runtime shutdown revokes live Human surface and preserves checkpoint for explicit recovery", async () => {
  const f = fixture();
  try {
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "allow" });
    const begun = ctl(f.bridge, "begin");
    assert.equal(begun.ok, true);
    const claim = await nativeControl(f.bridge, begun.native_locator, "claim");
    assert.equal(claim.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "human_active");
    assert.equal(fs.existsSync(f.checkpoint), true);

    await f.bridge.shutdown();
    assert.deepEqual(f.native.revoked, [begun.intervention_id]);
    assert.equal(f.bridge.handle({ action: "status" }).locator, null);
    assert.equal(fs.existsSync(f.checkpoint), true);

    const recovered = new HandoffBridge(api, f.store, () => 1_800_000_000_010, new FakeNativeSurface());
    assert.equal(recovered.handle({ action: "status" }).recovery_required, true);
    assert.deepEqual(recovered.handle(f.request), { ok: true, decision: "deny" });
  } finally {
    f.cleanup();
  }
});

test("checkpoint recovery never restores Agent or Human authority automatically", async () => {
  const f = fixture();
  try {
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    const claim = await nativeControl(f.bridge, begun.native_locator, "claim");
    assert.equal(claim.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "human_active");

    const checkpointText = fs.readFileSync(f.checkpoint, "utf8");
    assert.doesNotMatch(checkpointText, /process_id|window_id|frame|clipboard|typed|secret/i);

    const recoveredNative = new FakeNativeSurface();
    const recovered = new HandoffBridge(api, f.store, () => 1_800_000_000_010, recoveredNative);
    assert.equal(recovered.handle({ action: "status" }).recovery_required, true);
    assert.deepEqual(recovered.handle(f.request), { ok: true, decision: "deny" });
    const reissued = recovered.handle({ action: "recover_reissue" });
    assert.equal(reissued.ok, true);
    assert.equal(reissued.status, "awaiting_human");
    assert.ok(reissued.epoch > begun.epoch);
    assert.ok(reissued.native_locator);
    assert.deepEqual(recovered.handle(f.request), { ok: true, decision: "deny" });
  } finally {
    f.cleanup();
  }
});

test("checkpoint recovery can rebind an expired interaction context only with exact prior owner proof", async () => {
  const f = fixture();
  const priorContextId = "ctx_0123456789abcdef0123456789abcdef";
  try {
    f.request.exact_window.context_binding = contextBinding(priorContextId);
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "human_active");

    const recovered = new HandoffBridge(api, f.store, () => 1_800_000_000_010, new FakeNativeSurface());
    const fresh = structuredClone(f.request);
    fresh.exact_window.context_binding = contextBinding("ctx_fedcba9876543210fedcba9876543210");
    assert.deepEqual(recovered.handle(fresh), { ok: true, decision: "deny" });

    assert.deepEqual(recovered.handle({ action: "recover_rebind", prior_context_id: "ctx_ffffffffffffffffffffffffffffffff" }), { ok: false });

    const wrongTarget = structuredClone(fresh);
    wrongTarget.exact_window.window_id += 1;
    assert.deepEqual(recovered.handle(wrongTarget), { ok: true, decision: "deny" });
    assert.deepEqual(recovered.handle({ action: "recover_rebind", prior_context_id: priorContextId }), { ok: false });

    assert.deepEqual(recovered.handle(fresh), { ok: true, decision: "deny" });
    const reissued = recovered.handle({ action: "recover_rebind", prior_context_id: priorContextId });
    assert.equal(reissued.ok, true);
    assert.equal(reissued.status, "awaiting_human");
    assert.ok(reissued.epoch > begun.epoch);
    assert.equal(recovered.handle({ action: "status" }).recovery_required, false);
    assert.deepEqual(recovered.handle(fresh), { ok: true, decision: "deny" });
  } finally {
    f.cleanup();
  }
});



test("live intervention rebinds only to a strictly newer generation for the same exact OS Window", async () => {
  const f = fixture();
  try {
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "allow" });
    const begun = ctl(f.bridge, "begin");
    assert.equal(begun.ok, true);
    const claim = await nativeControl(f.bridge, begun.native_locator, "claim");
    assert.equal(claim.status, 200);
    const before = f.bridge.handle({ action: "status" }).active;
    assert.equal(before.status, "human_active");

    const fresh = structuredClone(f.request);
    fresh.generation += 1;
    fresh.exact_window.context_binding = "4".repeat(64);
    assert.deepEqual(f.bridge.handle(fresh), { ok: true, decision: "deny" });

    const wrongTarget = structuredClone(fresh);
    wrongTarget.exact_window.window_id += 1;
    assert.deepEqual(f.bridge.handleManaged({
      action: "rebind_live",
      authority: managedAuthority(wrongTarget),
    }), { ok: false });

    assert.deepEqual(f.bridge.handleManaged({
      action: "rebind_live",
      authority: managedAuthority(f.request),
    }), { ok: false });

    const rebound = f.bridge.handleManaged({
      action: "rebind_live",
      authority: managedAuthority(fresh),
    });
    assert.equal(rebound.ok, true);
    assert.equal(rebound.intervention_id, begun.intervention_id);
    assert.equal(rebound.epoch, begun.epoch);
    assert.equal(rebound.status, "human_active");

    // The old generation can never regain authority, while the fresh generation
    // remains Human-denied until Done -> verification -> explicit resume.
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });
    assert.deepEqual(f.bridge.handle(fresh), { ok: true, decision: "deny" });
    const done = await nativeControl(f.bridge, begun.native_locator, "done");
    assert.equal(done.status, 200);
    const verifying = f.bridge.handle({ action: "status" }).active;
    assert.equal(verifying.status, "verifying");

    const verification = structuredClone(fresh);
    verification.verification_candidate = true;
    const admitted = f.bridge.handle(verification);
    assert.equal(admitted.decision, "verification");
    assert.equal(admitted.intervention_id, begun.intervention_id);
    assert.equal(admitted.epoch, verifying.epoch);
    assert.deepEqual(f.bridge.handle({
      ...verification,
      action: "report_verification",
      intervention_id: admitted.intervention_id,
      epoch: admitted.epoch,
      satisfied: true,
    }), { ok: true });
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "ready_to_resume");
  } finally {
    f.cleanup();
  }
});

test("checkpoint recovery can prove the prior owner across a monotonic device generation rollover", async () => {
  const f = fixture();
  const priorContextId = "ctx_11111111111111111111111111111111";
  try {
    f.request.exact_window.context_binding = contextBinding(priorContextId);
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");

    const recovered = new HandoffBridge(api, f.store, () => 1_800_000_000_010, new FakeNativeSurface());
    const fresh = structuredClone(f.request);
    fresh.generation += 1;
    fresh.exact_window.context_binding = contextBinding("ctx_22222222222222222222222222222222");
    assert.deepEqual(recovered.handle(fresh), { ok: true, decision: "deny" });
    assert.deepEqual(recovered.handle({ action: "recover_rebind", prior_context_id: priorContextId }), { ok: false });
    assert.deepEqual(recovered.handle({ action: "recover_rebind", prior_context_id: priorContextId, prior_generation: fresh.generation + 1 }), { ok: false });
    const reissued = recovered.handle({ action: "recover_rebind", prior_context_id: priorContextId, prior_generation: f.request.generation });
    assert.equal(reissued.ok, true);
    assert.equal(reissued.status, "awaiting_human");
  } finally { f.cleanup(); }
});

test("expired signed checkpoint stays fail-closed until explicit prior-context rebind proof", async () => {
  const f = fixture();
  const priorContextId = "ctx_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  try {
    f.request.exact_window.context_binding = contextBinding(priorContextId);
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");
    f.tick(16 * 60_000);

    const recovered = new HandoffBridge(
      api, f.store, () => 1_800_000_960_001, new FakeNativeSurface(), f.audit,
    );
    const status = recovered.handle({ action: "status" });
    assert.equal(status.recovery_required, true);
    assert.equal(status.recovery_expired, true);
    assert.deepEqual(recovered.handle({ action: "recover_reissue" }), { ok: false });

    const fresh = structuredClone(f.request);
    fresh.exact_window.context_binding = contextBinding("ctx_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    assert.deepEqual(recovered.handle(fresh), { ok: true, decision: "deny" });
    const reissued = recovered.handle({ action: "recover_rebind", prior_context_id: priorContextId });
    assert.equal(reissued.ok, true);
    assert.equal(reissued.status, "awaiting_human");
    assert.equal(recovered.handle({ action: "status" }).recovery_expired, false);
  } finally { f.cleanup(); }
});

test("expired recovery can be explicitly abandoned only with the exact epoch and never replays the prior action", async () => {
  const f = fixture();
  try {
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "allow" });
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");
    f.tick(16 * 60_000);

    const recovered = new HandoffBridge(
      api, f.store, () => 1_800_000_960_001, new FakeNativeSurface(), f.audit,
    );
    const status = recovered.handle({ action: "status" });
    assert.equal(status.recovery_required, true);
    assert.equal(status.recovery_expired, true);
    assert.equal(status.recovery_epoch, begun.epoch);
    assert.equal(status.active, null);

    assert.deepEqual(recovered.handle({
      action: "abandon_expired_recovery",
      expected_epoch: begun.epoch + 1,
    }), { ok: false });
    assert.equal(fs.existsSync(f.checkpoint), true);
    assert.equal(recovered.handle({ action: "status" }).recovery_required, true);

    assert.deepEqual(recovered.handle({
      action: "abandon_expired_recovery",
      expected_epoch: begun.epoch,
    }), { ok: true });
    const cleared = recovered.handle({ action: "status" });
    assert.equal(cleared.recovery_required, false);
    assert.equal(cleared.active, null);
    assert.equal(cleared.resume_requested, false);
    assert.equal(fs.existsSync(f.checkpoint), false);
    const auditRecords = fs.readFileSync(path.join(f.root, "audit", "abandonment.jsonl"), "utf8")
      .trim().split("\n").map((line) => JSON.parse(line));
    assert.deepEqual(auditRecords, [{
      timestamp_ms: 1_800_000_960_001,
      recovery_epoch: begun.epoch,
      prior_closed_recovery_status: "human_active",
      result: "abandonment_authorized",
    }]);
    const auditText = JSON.stringify(auditRecords);
    assert.doesNotMatch(auditText, /principal|device|context|process|window|intervention|locator|turn|credential|input/i);

    // Abandonment does not resume/replay the old intervention. A future fresh admission is
    // evaluated as a new Agent action under the normal authority gate.
    assert.deepEqual(recovered.handle(f.request), { ok: true, decision: "allow" });
  } finally { f.cleanup(); }
});

test("expired recovery remains authoritative when append-only audit cannot be written", async () => {
  const f = fixture();
  try {
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");
    f.tick(16 * 60_000);
    const blocker = path.join(f.root, "audit-blocker");
    fs.writeFileSync(blocker, "x");
    const recovered = new HandoffBridge(
      api,
      f.store,
      () => 1_800_000_960_001,
      new FakeNativeSurface(),
      new AppendOnlyAbandonmentAudit(path.join(blocker, "audit.jsonl")),
    );
    assert.equal(recovered.handle({ action: "status" }).recovery_expired, true);
    assert.deepEqual(recovered.handle({
      action: "abandon_expired_recovery",
      expected_epoch: begun.epoch,
    }), { ok: false });
    const status = recovered.handle({ action: "status" });
    assert.equal(status.recovery_required, true);
    assert.equal(status.faulted, true);
    assert.equal(fs.existsSync(f.checkpoint), true);
  } finally { f.cleanup(); }
});

test("non-expired recovery cannot be abandoned", async () => {
  const f = fixture();
  try {
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");
    const recovered = new HandoffBridge(api, f.store, () => 1_800_000_000_010, new FakeNativeSurface());
    assert.equal(recovered.handle({ action: "status" }).recovery_expired, false);
    assert.deepEqual(recovered.handle({
      action: "abandon_expired_recovery",
      expected_epoch: begun.epoch,
    }), { ok: false });
    assert.equal(recovered.handle({ action: "status" }).recovery_required, true);
    assert.equal(fs.existsSync(f.checkpoint), true);
  } finally { f.cleanup(); }
});

test("wrong principal, target, intervention and stale epoch fail closed", async () => {
  const f = fixture();
  try {
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");

    const wrongPrincipal = structuredClone(f.request);
    wrongPrincipal.principal_binding = "9".repeat(64);
    assert.deepEqual(f.bridge.handle(wrongPrincipal), { ok: true, decision: "deny" });

    await nativeControl(f.bridge, begun.native_locator, "done");
    const verification = structuredClone(f.request);
    verification.verification_candidate = true;
    const admitted = f.bridge.handle(verification);

    const wrongTarget = structuredClone(verification);
    wrongTarget.exact_window.process_id += 1;
    assert.deepEqual(f.bridge.handle(wrongTarget), { ok: true, decision: "deny" });

    assert.deepEqual(f.bridge.handle({
      ...verification,
      action: "report_verification",
      intervention_id: "stale-intervention",
      epoch: admitted.epoch,
      satisfied: true,
    }), { ok: false });
    assert.deepEqual(f.bridge.handle({
      ...verification,
      action: "report_verification",
      intervention_id: admitted.intervention_id,
      epoch: admitted.epoch + 1,
      satisfied: true,
    }), { ok: false });
  } finally {
    f.cleanup();
  }
});

test("native Cancel after possible Human side effects enters verifying and never restores Agent", async () => {
  const f = fixture();
  try {
    f.bridge.handle(f.request);
    const begun = ctl(f.bridge, "begin");
    await nativeControl(f.bridge, begun.native_locator, "claim");
    const cancelled = await nativeControl(f.bridge, begun.native_locator, "cancel");
    assert.equal(cancelled.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "verifying");
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });
  } finally {
    f.cleanup();
  }
});


test("CUMG WebRTC surface composes the first-class WindowHandoffAdapter with exact legacy-equivalent input policy", async () => {
  const calls = { config: undefined, start: undefined, handle: undefined, revoked: [], unclaimed: [] };
  class FakeWindowHandoffAdapter {
    constructor(config) { calls.config = structuredClone(config); }
    start(request) {
      calls.start = structuredClone(request);
      return "https://handoff.example/takeover/window-session-12345678";
    }
    handle(request, principalBinding) {
      calls.handle = { pathname: new URL(request.url).pathname, principalBinding };
      return Promise.resolve(new Response("ok", { status: 200 }));
    }
    async revoke(interventionId) { calls.revoked.push(interventionId); }
    revokeUnclaimed(interventionId) { calls.unclaimed.push(interventionId); }
  }
  const surface = new WebRtcHandoffSurface(
    { WindowHandoffAdapter: FakeWindowHandoffAdapter },
    { publicBaseUrl: "https://handoff.example/", hostExecutable: process.execPath },
  );
  const locator = surface.create(
    { id: "window-int-1", epoch: 7 },
    {
      principalBinding: "1".repeat(64),
      exactWindow: { processId: 1234, windowId: 5678 },
    },
  );
  assert.equal(locator, "https://handoff.example/takeover/window-session-12345678");
  assert.deepEqual(calls.config, {
    takeover: { enabled: true, publicBaseUrl: "https://handoff.example/", ttlMs: 300_000, reconnectIdleMs: 2_000 },
    runtime: { hostExecutable: process.execPath },
  });
  assert.deepEqual(calls.start, {
    intervention: { id: "window-int-1", epoch: 7 },
    principalBinding: "1".repeat(64),
    target: { processId: 1234, windowId: 5678 },
    inputPolicy: { tap: true, scroll: true, text: true, key: true },
  });
  const handled = await surface.handle(new Request("https://handoff.example/takeover/window-session-12345678"), "1".repeat(64));
  assert.equal(handled.status, 200);
  assert.deepEqual(calls.handle, { pathname: "/takeover/window-session-12345678", principalBinding: "1".repeat(64) });
  await surface.revoke("window-int-1");
  surface.revokeUnclaimed("window-int-2");
  assert.deepEqual(calls.revoked, ["window-int-1"]);
  assert.deepEqual(calls.unclaimed, ["window-int-2"]);
});


test("iPhone WebRTC prepare does not claim Human authority; connect does, suspend stays fail-closed, Done verifies", async () => {
  const surface = new FakeWebRtcSurface();
  const f = fixture(surface);
  try {
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "allow" });
    const begun = ctl(f.bridge, "begin");
    assert.equal(begun.ok, true);
    assert.equal(begun.surface, "webrtc");
    assert.equal(begun.webrtc_locator, begun.locator);
    assert.equal(begun.native_locator, null);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "awaiting_human");
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    const page = await f.bridge.handleSurface(new Request(begun.locator));
    assert.equal(page.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "awaiting_human");

    const prepared = await webRtcControl(f.bridge, begun.locator, "webrtc-prepare-claim");
    assert.equal(prepared.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "awaiting_human");

    const connected = await webRtcControl(f.bridge, begun.locator, "webrtc-connect");
    assert.equal(connected.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "human_active");
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    const suspended = await webRtcControl(f.bridge, begun.locator, "webrtc-suspend");
    assert.equal(suspended.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "human_active");
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });

    const reconnected = await webRtcControl(f.bridge, begun.locator, "webrtc-connect");
    assert.equal(reconnected.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "human_active");

    const done = await webRtcControl(f.bridge, begun.locator, "done");
    assert.equal(done.status, 200);
    assert.equal(f.bridge.handle({ action: "status" }).active.status, "verifying");
    assert.deepEqual(f.bridge.handle(f.request), { ok: true, decision: "deny" });
  } finally {
    f.cleanup();
  }
});


class FakeTerminalHandoffAdapter {
  static instances = [];

  constructor(config) {
    this.config = structuredClone(config);
    this.binding = structuredClone(config.binding);
    this.statusValue = {
      authority: "agent",
      interventionStatus: null,
      interventionEpoch: null,
      sessionGeneration: config.binding.sessionGeneration,
      sessionAlive: true,
      humanDisconnected: false,
      agentStateSynchronizationRequired: false,
      transport: null,
    };
    this.events = [{ kind: "resize", rows: 30, cols: 100 }, { kind: "done" }];
    this.outputs = [];
    this.revoked = false;
    FakeTerminalHandoffAdapter.instances.push(this);
  }

  status() { return structuredClone(this.statusValue); }
  begin() {
    this.awaiting = { interventionId: "terminal-intervention", epoch: 1, status: "awaiting_human" };
    this.statusValue.authority = "none";
    this.statusValue.interventionStatus = "awaiting_human";
    this.statusValue.interventionEpoch = 1;
    this.statusValue.transport = {
      transportReady: true, humanActive: false, disconnected: false,
      completed: false, faulted: false, queuedEvents: this.events.length,
    };
    return { intervention: structuredClone(this.awaiting), locator: "https://handoff.example/takeover/terminal/fake-session" };
  }
  transportStatus(ref) {
    assert.equal(ref.interventionId, "terminal-intervention");
    return structuredClone(this.statusValue.transport);
  }
  claimHumanAfterAgentDrain(ref) {
    assert.equal(ref.interventionId, "terminal-intervention");
    this.human = { interventionId: ref.interventionId, epoch: ref.epoch, status: "human_active" };
    this.statusValue.authority = "human";
    this.statusValue.interventionStatus = "human_active";
    this.statusValue.transport.humanActive = true;
    return structuredClone(this.human);
  }
  assertAgentInput() { if (this.statusValue.authority !== "agent" || this.statusValue.agentStateSynchronizationRequired) throw new Error("fenced"); }
  assertAgentObservation() { this.assertAgentInput(); }
  assertAgentResize() { this.assertAgentInput(); }
  assertHumanInput() { if (this.statusValue.authority !== "human") throw new Error("fenced"); }
  assertHumanObservation() { this.assertHumanInput(); }
  assertHumanResize() { this.assertHumanInput(); }
  nextHumanEvent() {
    const event = this.events.shift();
    if (event?.kind === "done") {
      this.verifying = { interventionId: "terminal-intervention", epoch: 2, status: "verifying" };
      this.statusValue.authority = "none";
      this.statusValue.interventionStatus = "verifying";
      this.statusValue.interventionEpoch = 2;
      this.statusValue.agentStateSynchronizationRequired = true;
      this.statusValue.transport = null;
      return { kind: "done", verifying: structuredClone(this.verifying) };
    }
    if (this.statusValue.transport) this.statusValue.transport.queuedEvents = this.events.length;
    return event ? structuredClone(event) : undefined;
  }
  pushHumanOutput(_human, bytes) { this.assertHumanObservation(); this.outputs.push(Buffer.from(bytes).toString("base64")); }
  noteHumanDisconnect() { this.statusValue.humanDisconnected = true; return this.status(); }
  confirmHumanDrain(ref) { assert.equal(ref.epoch, 2); return structuredClone(this.verifying); }
  reportVerification(ref, satisfied) {
    assert.equal(ref.epoch, 2);
    if (!satisfied) return structuredClone(this.verifying);
    this.ready = { interventionId: ref.interventionId, epoch: ref.epoch, status: "ready_to_resume" };
    this.statusValue.interventionStatus = "ready_to_resume";
    return structuredClone(this.ready);
  }
  resume(ref) {
    assert.equal(ref.status, "ready_to_resume");
    this.statusValue.authority = "agent";
    this.statusValue.interventionStatus = null;
    this.statusValue.interventionEpoch = null;
    this.statusValue.agentStateSynchronizationRequired = true;
    return { resumePolicy: "never_replay", epoch: ref.epoch, sessionAlive: true, agentStateSynchronizationRequired: true };
  }
  acknowledgeAgentStateSynchronization() { this.statusValue.agentStateSynchronizationRequired = false; }
  async noteSessionExit() {
    this.statusValue.sessionAlive = false;
    this.statusValue.authority = "none";
    this.statusValue.interventionStatus = null;
    this.statusValue.interventionEpoch = null;
    this.statusValue.transport = null;
    return this.status();
  }
  async revokeTransport() { this.revoked = true; this.statusValue.transport = null; }
  async handle(_request, principal) {
    assert.equal(principal, this.binding.principalBinding);
    return new Response("terminal", { status: 200 });
  }
}

test("CUMG Terminal bridge consumes one first-class TerminalHandoffAdapter while preserving the existing wire contract", async () => {
  FakeTerminalHandoffAdapter.instances.length = 0;
  const terminalApi = { TerminalHandoffAdapter: FakeTerminalHandoffAdapter };
  const takeover = { enabled: true, publicBaseUrl: "https://handoff.example/", ttlMs: 300_000, env: {} };
  const bridge = new TerminalPtyHandoffBridge(terminalApi, takeover);
  const terminal_pty = { session_id: "a".repeat(32), session_generation: 7, principal_binding: "c".repeat(64) };

  assert.equal((await bridge.handle({ action: "terminal_bind", terminal_pty })).ok, true);
  assert.equal(FakeTerminalHandoffAdapter.instances.length, 1);
  const adapter = FakeTerminalHandoffAdapter.instances[0];
  assert.deepEqual(adapter.config, {
    binding: { sessionId: "a".repeat(32), sessionGeneration: 7, principalBinding: "c".repeat(64) },
    takeover,
  });

  const fenced = await bridge.handle({ action: "terminal_begin_fence", terminal_pty });
  assert.equal(fenced.intervention_status, "awaiting_human");
  assert.equal((await bridge.handle({ action: "terminal_agent_input", terminal_pty })).ok, false);

  const started = await bridge.handle({ action: "terminal_transport_start", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch });
  assert.equal(started.locator, "https://handoff.example/takeover/terminal/fake-session");
  const transportStatus = await bridge.handle({ action: "terminal_transport_status", terminal_pty });
  assert.equal(transportStatus.transport_status.transportReady, true);

  const claimed = await bridge.handle({ action: "terminal_claim_human", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch });
  assert.equal(claimed.intervention_status, "human_active");
  assert.equal((await bridge.handle({ action: "terminal_transport_activate", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch })).ok, true);
  assert.equal((await bridge.handle({ action: "terminal_human_input", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch })).ok, true);
  assert.equal((await bridge.handle({ action: "terminal_human_observe", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch })).ok, true);
  assert.equal((await bridge.handle({ action: "terminal_human_resize", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch })).ok, true);

  const event1 = await bridge.handle({ action: "terminal_transport_next_event", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch });
  assert.deepEqual(event1.event, { kind: "resize", rows: 30, cols: 100 });
  assert.equal((await bridge.handle({ action: "terminal_transport_output", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch, data_base64: "c2FmZQ==" })).ok, true);
  assert.deepEqual(adapter.outputs, ["c2FmZQ=="]);

  const event2 = await bridge.handle({ action: "terminal_transport_next_event", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch });
  assert.deepEqual(event2.event, { kind: "done" });
  const verifying = await bridge.handle({ action: "terminal_done_fence", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch });
  assert.equal(verifying.intervention_status, "verifying");
  const drained = await bridge.handle({ action: "terminal_confirm_human_drain", terminal_pty, intervention_id: verifying.intervention_id, epoch: verifying.epoch });
  assert.equal(drained.intervention_status, "verifying");
  const ready = await bridge.handle({ action: "terminal_verify", terminal_pty, intervention_id: drained.intervention_id, epoch: drained.epoch, satisfied: true });
  assert.equal(ready.intervention_status, "ready_to_resume");
  const resumed = await bridge.handle({ action: "terminal_resume", terminal_pty, intervention_id: ready.intervention_id, epoch: ready.epoch });
  assert.equal(resumed.agent_state_sync_required, true);
  assert.equal((await bridge.handle({ action: "terminal_agent_input", terminal_pty })).ok, false);
  assert.equal((await bridge.handle({ action: "terminal_ack_state_sync", terminal_pty })).ok, true);
  assert.equal((await bridge.handle({ action: "terminal_agent_input", terminal_pty })).ok, true);

  const surface = await bridge.handleSurface(new Request("https://handoff.example/takeover/terminal/fake-session"));
  assert.equal(surface.status, 200);
  assert.equal((await bridge.handle({ action: "terminal_transport_revoke", terminal_pty, intervention_id: fenced.intervention_id, epoch: fenced.epoch })).ok, true);
  assert.equal(adapter.revoked, true);
});

test("actual Handoff root exposes the first-class Terminal adapter and pre-claim setup failure rolls Agent authority back", { skip: !HANDOFF_ROOT }, async () => {
  assert.equal(typeof api.TerminalHandoffAdapter, "function");
  const bridge = new TerminalPtyHandoffBridge(api, { enabled: false, ttlMs: 60_000, env: {} });
  const terminal_pty = { session_id: "d".repeat(32), session_generation: 3, principal_binding: "e".repeat(64) };
  assert.equal((await bridge.handle({ action: "terminal_bind", terminal_pty })).ok, true);
  assert.equal((await bridge.handle({ action: "terminal_agent_input", terminal_pty })).ok, true);
  const begun = await bridge.handle({ action: "terminal_begin_fence", terminal_pty });
  assert.equal(begun.ok, false);
  const status = await bridge.handle({ action: "terminal_status", terminal_pty });
  assert.equal(status.status.authority, "agent");
  assert.equal(status.status.interventionStatus, null);
  assert.equal((await bridge.handle({ action: "terminal_agent_input", terminal_pty })).ok, true);
});

test("begin failure diagnostics expose only bounded codes and never exception messages", () => {
  const sensitive = new Error("secret-token=do-not-log");
  sensitive.code = "WINDOW_HANDOFF_UNAVAILABLE";
  assert.equal(handoffBeginFailureCode(sensitive), "WINDOW_HANDOFF_UNAVAILABLE");
  sensitive.code = "UNBOUNDED_INTERNAL_DETAIL";
  assert.equal(handoffBeginFailureCode(sensitive), "HANDOFF_BEGIN_INTERNAL");
});
