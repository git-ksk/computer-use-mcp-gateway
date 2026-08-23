import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { HandoffBridge } from "../v2_operator_handoff_bridge.mjs";

const HANDOFF_ROOT = process.env.CUMG_V2_HANDOFF_ROOT;
const api = HANDOFF_ROOT
  ? await import(pathToFileURL(path.join(HANDOFF_ROOT, "dist/core/index.js")).href)
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
  const bridge = new HandoffBridge(api, store, () => now, surface);
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
    root, checkpoint, store, bridge, surface, native: surface, request,
    tick(ms = 1) { now += ms; },
    cleanup() { fs.rmSync(root, { recursive: true, force: true }); },
  };
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

    const recovered = new HandoffBridge(api, f.store, () => 1_800_000_960_001, new FakeNativeSurface());
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
