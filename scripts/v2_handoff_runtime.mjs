#!/usr/bin/env node
import { createHash } from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import { createInterface } from "node:readline";
import path from "node:path";
import { pathToFileURL } from "node:url";

const PROTOCOL = 1;
const MAX_LINE = 4096;
const OBSERVATION_TTL_MS = 60_000;
const CHECKPOINT_TTL_MS = 15 * 60_000;
const MAX_RECOVERED_EPOCH = 1_000_000;
const MAX_AUDIT_RECORD_BYTES = 1024;

export class AppendOnlyAbandonmentAudit {
  constructor(filePath) {
    if (!path.isAbsolute(filePath)) throw new Error("abandonment audit path must be absolute");
    this.filePath = filePath;
  }

  record({ timestampMs, recoveryEpoch, priorClosedRecoveryStatus, result }) {
    if (!Number.isSafeInteger(timestampMs) || timestampMs < 0
      || !positiveInt(recoveryEpoch)
      || typeof priorClosedRecoveryStatus !== "string" || !priorClosedRecoveryStatus
      || typeof result !== "string" || !/^[a-z0-9_]{1,64}$/.test(result)) {
      throw new Error("abandonment audit record invalid");
    }
    const directory = path.dirname(this.filePath);
    try { fs.mkdirSync(directory, { mode: 0o700 }); }
    catch (error) { if (error?.code !== "EEXIST") throw error; }
    const directoryMetadata = fs.lstatSync(directory);
    if (directoryMetadata.isSymbolicLink() || !directoryMetadata.isDirectory()
      || (process.platform !== "win32" && (directoryMetadata.mode & 0o077) !== 0)) {
      throw new Error("abandonment audit directory unsafe");
    }
    const record = {
      timestamp_ms: timestampMs,
      recovery_epoch: recoveryEpoch,
      prior_closed_recovery_status: priorClosedRecoveryStatus,
      result,
    };
    const encoded = `${JSON.stringify(record)}\n`;
    if (Buffer.byteLength(encoded, "utf8") > MAX_AUDIT_RECORD_BYTES) {
      throw new Error("abandonment audit record too large");
    }
    const noFollow = process.platform === "win32" ? 0 : fs.constants.O_NOFOLLOW;
    const fd = fs.openSync(
      this.filePath,
      fs.constants.O_WRONLY | fs.constants.O_APPEND | fs.constants.O_CREAT | noFollow,
      0o600,
    );
    try {
      const metadata = fs.fstatSync(fd);
      if (!metadata.isFile() || (process.platform !== "win32" && (metadata.mode & 0o077) !== 0)) {
        throw new Error("abandonment audit file unsafe");
      }
      fs.writeSync(fd, encoded, undefined, "utf8");
      fs.fsyncSync(fd);
    } finally { fs.closeSync(fd); }
  }
}

function parseArgs(argv) {
  const command = argv[2] ?? "";
  const options = new Map();
  for (let i = 3; i < argv.length; i += 2) {
    const key = argv[i];
    const value = argv[i + 1];
    if (!key?.startsWith("--") || value === undefined) throw new Error("invalid arguments");
    options.set(key.slice(2), value);
  }
  return { command, options };
}

function optional(options, name, envName) {
  return options.get(name) ?? (envName ? process.env[envName] : undefined);
}

function required(options, name, envName) {
  const value = options.get(name) ?? (envName ? process.env[envName] : undefined);
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function exactWindow(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  if (!isBinding(value.context_binding) || !positiveInt(value.process_id) || !positiveInt(value.window_id)) return undefined;
  return {
    contextBinding: value.context_binding,
    processId: value.process_id,
    windowId: value.window_id,
  };
}

function terminalPtyBinding(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  if (typeof value.session_id !== "string" || !/^[0-9a-f]{32}$/.test(value.session_id)
    || !positiveInt(value.session_generation)
    || !isBinding(value.principal_binding)) return undefined;
  return {
    sessionId: value.session_id,
    sessionGeneration: value.session_generation,
    principalBinding: value.principal_binding,
  };
}

function positiveInt(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function isBinding(value) {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}

function interactionContextBinding(contextId) {
  if (typeof contextId !== "string" || !/^ctx_[0-9a-f]{32}$/.test(contextId)) return undefined;
  return createHash("sha256")
    .update("cumg/operator-handoff/context/v1\0", "utf8")
    .update(contextId, "utf8")
    .digest("hex");
}

function sameExact(left, right) {
  return !!left && !!right
    && left.contextBinding === right.contextBinding
    && left.processId === right.processId
    && left.windowId === right.windowId;
}

function sameAuthority(left, right) {
  return left.principalBinding === right.principalBinding
    && left.deviceBinding === right.deviceBinding
    && left.generation === right.generation
    && left.capabilityRevision === right.capabilityRevision;
}

function safeStatus(active, recovery, recoveryExpired, resumeRequested, latest, surface, locator, faulted) {
  return {
    ok: true,
    active: active ? {
      intervention_id: active.id,
      status: active.status,
      epoch: active.epoch,
      authority: active.authority,
    } : null,
    recovery_required: !!recovery,
    recovery_status: recovery?.status ?? null,
    recovery_epoch: recovery?.epoch ?? null,
    recovery_expired: !!recoveryExpired,
    resume_requested: resumeRequested,
    faulted: !!faulted,
    human_surface: surface?.kind ?? null,
    locator: locator ?? null,
    native_locator: surface?.kind === "native" ? locator ?? null : null,
    webrtc_locator: surface?.kind === "webrtc" ? locator ?? null : null,
    latest_exact_window: latest?.exactWindow ? {
      process_id: latest.exactWindow.processId,
      window_id: latest.exactWindow.windowId,
    } : null,
  };
}


const NATIVE_TTL_MS = 5 * 60_000;

function disabledLegacyBrowserAdapter() {
  const unavailable = async () => { throw new Error("legacy browser frame/input surface disabled for CUMG handoff dogfood"); };
  return {
    captureHumanTakeoverFrame: unavailable,
    tapHumanTakeover: unavailable,
    scrollHumanTakeover: unavailable,
    insertHumanTakeoverText: unavailable,
    pressHumanTakeoverKey: unavailable,
  };
}

export class NativeHandoffSurface {
  constructor(api, { baseUrl, hostExecutable, revokeExecutable }) {
    const url = new URL(baseUrl);
    if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" || !url.port || url.pathname !== "/") {
      throw new Error("native dogfood broker must use an explicit 127.0.0.1 HTTP origin");
    }
    for (const executable of [hostExecutable, revokeExecutable]) {
      if (!path.isAbsolute(executable) || !fs.statSync(executable).isFile()) throw new Error("native runtime executable invalid");
      fs.accessSync(executable, fs.constants.X_OK);
    }
    const runtime = new api.InheritedFdNativeRuntimeProvider({
      hostExecutable,
      revokeExecutable,
      advertisedHost: "127.0.0.1",
      inputBindHost: "127.0.0.1",
      feedbackBindHost: "127.0.0.1",
      controlBindHost: "127.0.0.1",
      inputPort: 48_556,
      controlPort: 48_557,
      videoFeedbackPort: 48_558,
    });
    this.kind = "native";
    this.broker = new api.TakeoverBroker(disabledLegacyBrowserAdapter(), {
      enabled: true,
      publicBaseUrl: baseUrl,
      ttlMs: NATIVE_TTL_MS,
      reconnectIdleMs: 500,
    }, runtime);
  }

  create(intervention, binding) {
    return this.broker.createNativeLink(
      { id: intervention.id, epoch: intervention.epoch },
      binding.principalBinding,
      { processId: binding.exactWindow.processId, windowId: binding.exactWindow.windowId },
    );
  }

  handle(request, principalBinding) {
    return this.broker.handle(request, principalBinding);
  }

  revoke(interventionId) {
    return this.broker.revokeNativeForIntervention(interventionId);
  }

  revokeUnclaimed(interventionId) {
    this.broker.revokeForIntervention(interventionId);
  }

  lifecycle(pathname) {
    const match = /^\/takeover\/api\/(claim|reconnect|done|cancel)\//.exec(pathname);
    if (!match) return undefined;
    if (match[1] === "claim") return "claim";
    if (match[1] === "done" || match[1] === "cancel") return "complete";
    return undefined;
  }
}

export class WebRtcHandoffSurface {
  constructor(api, { publicBaseUrl, hostExecutable }) {
    const url = new URL(publicBaseUrl);
    if (url.protocol !== "https:" || !url.hostname || url.username || url.password || url.search || url.hash || url.pathname !== "/") {
      throw new Error("WebRTC dogfood broker requires an explicit HTTPS public origin");
    }
    if (!path.isAbsolute(hostExecutable) || !fs.statSync(hostExecutable).isFile()) {
      throw new Error("WebRTC runtime executable invalid");
    }
    fs.accessSync(hostExecutable, fs.constants.X_OK);
    this.kind = "webrtc";
    this.broker = new api.TakeoverBroker(disabledLegacyBrowserAdapter(), {
      enabled: true,
      publicBaseUrl,
      ttlMs: NATIVE_TTL_MS,
      reconnectIdleMs: 2_000,
    }, undefined, new api.SpawnedWebRtcRuntimeProvider({ hostExecutable }));
  }

  create(intervention, binding) {
    return this.broker.createWebRtcLink(
      { id: intervention.id, epoch: intervention.epoch },
      binding.principalBinding,
      { processId: binding.exactWindow.processId, windowId: binding.exactWindow.windowId },
    );
  }

  handle(request, principalBinding) {
    return this.broker.handle(request, principalBinding);
  }

  revoke(interventionId) {
    return this.broker.revokeWebRtcForIntervention(interventionId);
  }

  revokeUnclaimed(interventionId) {
    this.broker.revokeForIntervention(interventionId);
  }

  lifecycle(pathname) {
    const match = /^\/takeover\/api\/(webrtc-connect|done|cancel)\//.exec(pathname);
    if (!match) return undefined;
    if (match[1] === "webrtc-connect") return "connect";
    return "complete";
  }
}

export class TerminalPtyHandoffBridge {
  constructor(api) {
    if (typeof api.ExperimentalTerminalPtyAuthority !== "function") {
      throw new Error("experimental Terminal PTY Handoff runtime unavailable");
    }
    this.api = api;
    this.binding = undefined;
    this.gate = undefined;
  }

  bind(request) {
    const binding = terminalPtyBinding(request.terminal_pty);
    if (!binding) return { ok: false, code: "terminal_binding_invalid" };
    if (this.binding) {
      if (this.binding.sessionId !== binding.sessionId
        || this.binding.sessionGeneration !== binding.sessionGeneration
        || this.binding.principalBinding !== binding.principalBinding) {
        return { ok: false, code: "terminal_binding_mismatch" };
      }
      return { ok: true, status: this.gate.getStatus() };
    }
    const unavailableDrain = async () => { throw new Error("external CUMG drain handshake required"); };
    this.binding = binding;
    this.gate = new this.api.ExperimentalTerminalPtyAuthority(
      binding,
      { drainAgentWrites: unavailableDrain, drainHumanWrites: unavailableDrain },
    );
    return { ok: true, status: this.gate.getStatus() };
  }

  require(request) {
    const binding = terminalPtyBinding(request.terminal_pty);
    if (!binding || !this.binding || !this.gate
      || binding.sessionId !== this.binding.sessionId
      || binding.sessionGeneration !== this.binding.sessionGeneration
      || binding.principalBinding !== this.binding.principalBinding) {
      throw new Error("terminal binding unavailable");
    }
    return binding;
  }

  handle(request) {
    try {
      if (request.action === "terminal_bind") return this.bind(request);
      if (request.action === "terminal_status") {
        return { ok: true, status: this.gate?.getStatus() ?? null };
      }
      if (request.action === "terminal_release_closed") {
        this.require(request);
        const status = this.gate.getStatus();
        if (status.sessionAlive || status.interventionStatus !== null || status.authority !== "none") {
          return { ok: false, code: "terminal_release_rejected" };
        }
        this.binding = undefined;
        this.gate = undefined;
        return { ok: true, status: null };
      }
      const binding = this.require(request);
      switch (request.action) {
      case "terminal_agent_input":
        this.gate.assertAgentInput(binding); return { ok: true };
      case "terminal_agent_observe":
        this.gate.assertAgentObservation(binding); return { ok: true };
      case "terminal_agent_resize":
        this.gate.assertAgentResize(binding); return { ok: true };
      case "terminal_begin_fence": {
        const active = this.gate.beginFence(binding);
        return { ok: true, intervention_id: active.id, epoch: active.epoch, intervention_status: active.status };
      }
      case "terminal_claim_human": {
        const active = this.gate.claimHumanAfterAgentDrain(binding, request.intervention_id, request.epoch);
        return { ok: true, intervention_id: active.id, epoch: active.epoch, intervention_status: active.status };
      }
      case "terminal_human_input":
        this.gate.assertHumanInput(binding, request.intervention_id, request.epoch); return { ok: true };
      case "terminal_human_observe":
        this.gate.assertHumanObservation(binding, request.intervention_id, request.epoch); return { ok: true };
      case "terminal_human_resize":
        this.gate.assertHumanResize(binding, request.intervention_id, request.epoch); return { ok: true };
      case "terminal_human_disconnect":
        return { ok: true, status: this.gate.noteHumanDisconnect(binding, request.intervention_id, request.epoch) };
      case "terminal_done_fence": {
        const active = this.gate.markHumanDoneFence(binding, request.intervention_id, request.epoch);
        return { ok: true, intervention_id: active.id, epoch: active.epoch, intervention_status: active.status };
      }
      case "terminal_confirm_human_drain": {
        const active = this.gate.confirmHumanWritesDrained(binding, request.intervention_id, request.epoch);
        return { ok: true, intervention_id: active.id, epoch: active.epoch, intervention_status: active.status };
      }
      case "terminal_verify": {
        if (typeof request.satisfied !== "boolean") return { ok: false, code: "terminal_verification_invalid" };
        const active = this.gate.reportVerification(binding, request.intervention_id, request.epoch, request.satisfied);
        return { ok: true, intervention_id: active.id, epoch: active.epoch, intervention_status: active.status };
      }
      case "terminal_resume": {
        const decision = this.gate.resumeAgent(binding, request.intervention_id, request.epoch);
        return {
          ok: true,
          resume_policy: decision.resumePolicy,
          epoch: decision.epoch,
          session_alive: decision.sessionAlive,
          agent_state_sync_required: decision.agentStateSynchronizationRequired,
        };
      }
      case "terminal_ack_state_sync":
        this.gate.acknowledgeAgentStateSynchronization(binding); return { ok: true };
      case "terminal_session_exit":
        return { ok: true, status: this.gate.noteSessionExit(binding) };
      default:
        return { ok: false, code: "terminal_action_unsupported" };
      }
    } catch (error) {
      const code = typeof error?.code === "string" && /^[A-Z0-9_]{1,80}$/.test(error.code)
        ? error.code.toLowerCase()
        : "terminal_action_rejected";
      return { ok: false, code };
    }
  }
}

export class HandoffBridge {
  constructor(api, checkpointStore, now = Date.now, humanSurface = undefined, abandonmentAudit = undefined) {
    this.api = api;
    this.checkpointStore = checkpointStore;
    this.now = now;
    this.humanSurface = humanSurface;
    this.abandonmentAudit = abandonmentAudit;
    this.state = new api.ExecutionHandoffState(now);
    this.owners = new Map();
    this.activeBinding = undefined;
    this.latestObservation = undefined;
    this.resumeRequested = false;
    this.surfaceLocator = undefined;
    this.faulted = false;
    this.recoveryExpired = false;
    try {
      this.recovery = checkpointStore.recover();
    } catch (error) {
      if (error?.code !== "CHECKPOINT_EXPIRED"
        || typeof checkpointStore.recoverForOperatorRevalidation !== "function") throw error;
      this.recovery = checkpointStore.recoverForOperatorRevalidation();
      this.recoveryExpired = true;
    }
    if (this.recovery && this.recovery.epoch > MAX_RECOVERED_EPOCH) {
      throw new Error("recovered handoff epoch exceeds bridge bound");
    }
  }

  ownerFor(binding) {
    return this.api.createHandoffOwner(
      binding.principalBinding,
      "cumg.os_window_handoff",
      {
        deviceBinding: binding.deviceBinding,
        generation: binding.generation,
        capabilityRevision: binding.capabilityRevision,
        contextBinding: binding.exactWindow.contextBinding,
        processId: binding.exactWindow.processId,
        windowId: binding.exactWindow.windowId,
      },
      "require_fresh_semantic_action",
    );
  }

  createSurfaceLocator(intervention) {
    if (!this.humanSurface || !this.activeBinding?.exactWindow) return undefined;
    const locator = this.humanSurface.create(intervention, this.activeBinding);
    if (!locator) throw new Error("Human handoff locator unavailable");
    this.surfaceLocator = locator;
    return locator;
  }

  async handleSurface(request) {
    const active = this.state.getActive();
    if (!this.humanSurface || !active || !this.activeBinding || !this.surfaceLocator || this.faulted) {
      return new Response(JSON.stringify({ error: "takeover_unavailable" }), { status: 404, headers: { "content-type": "application/json" } });
    }
    const lifecycle = this.humanSurface.lifecycle(new URL(request.url).pathname);
    const response = await this.humanSurface.handle(request, this.activeBinding.principalBinding);
    if (!lifecycle || response.status !== 200) return response;

    if (lifecycle === "claim" || lifecycle === "connect") {
      const current = this.state.getActive();
      const initialClaim = current && current.id === active.id && current.epoch === active.epoch && current.status === "awaiting_human";
      const reconnect = lifecycle === "connect" && current && current.id === active.id && current.epoch === active.epoch && current.status === "human_active";
      if (!initialClaim && !reconnect) {
        await this.humanSurface.revoke(active.id).catch(() => undefined);
        this.faulted = true;
        return new Response(JSON.stringify({ error: "takeover_state_changed" }), { status: 409, headers: { "content-type": "application/json" } });
      }
      if (initialClaim) {
        this.state.claimHuman(active.id);
        try { this.checkpoint(); }
        catch {
          await this.humanSurface.revoke(active.id).catch(() => undefined);
          this.faulted = true;
          return new Response(JSON.stringify({ error: "takeover_checkpoint_failed" }), { status: 503, headers: { "content-type": "application/json" } });
        }
      }
    } else if (lifecycle === "complete") {
      const current = this.state.getActive();
      if (!current || current.id !== active.id || current.status !== "human_active") {
        this.faulted = true;
        return new Response(JSON.stringify({ error: "takeover_state_changed" }), { status: 409, headers: { "content-type": "application/json" } });
      }
      this.state.markHumanComplete(active.id);
      this.surfaceLocator = undefined;
      this.resumeRequested = false;
      try { this.checkpoint(); }
      catch {
        this.faulted = true;
        return new Response(JSON.stringify({ error: "takeover_checkpoint_failed" }), { status: 503, headers: { "content-type": "application/json" } });
      }
    }
    return response;
  }

  checkpoint() {
    const active = this.state.getActive();
    if (!active || !this.activeBinding) {
      this.checkpointStore.clear();
      return;
    }
    const owner = this.owners.get(active.id);
    if (!owner) throw new Error("active handoff owner missing");
    this.checkpointStore.write({
      version: 1,
      adapterKind: "cumg_os_window_dogfood",
      interventionId: active.id,
      status: active.status,
      epoch: active.epoch,
      resumePolicy: active.resumePolicy,
      principalBinding: owner.principalBinding,
      actionDigest: owner.argsDigest,
      updatedAt: active.updatedAt,
      expiresAt: this.now() + CHECKPOINT_TTL_MS,
    });
  }

  parseAdmission(request) {
    if (request.protocol !== PROTOCOL
      || !isBinding(request.principal_binding)
      || !isBinding(request.device_binding)
      || !positiveInt(request.generation)
      || !Number.isSafeInteger(request.capability_revision)
      || request.capability_revision < 0
      || typeof request.verification_candidate !== "boolean") {
      return undefined;
    }
    const suppliedExact = request.exact_window === undefined ? undefined : exactWindow(request.exact_window);
    if (request.exact_window !== undefined && !suppliedExact) return undefined;
    return {
      principalBinding: request.principal_binding,
      deviceBinding: request.device_binding,
      generation: request.generation,
      capabilityRevision: request.capability_revision,
      exactWindow: suppliedExact,
      verificationCandidate: request.verification_candidate,
      observedAt: this.now(),
    };
  }

  admit(request) {
    const candidate = this.parseAdmission(request);
    if (!candidate) return { ok: false };
    if (candidate.exactWindow) this.latestObservation = candidate;
    if (this.recovery || this.faulted) return { ok: true, decision: "deny" };

    const active = this.state.getActive();
    if (!active) return { ok: true, decision: "allow" };
    if (!this.activeBinding || !sameAuthority(candidate, this.activeBinding)) {
      return { ok: true, decision: "deny" };
    }

    if (active.status === "verifying"
      && candidate.verificationCandidate
      && sameExact(candidate.exactWindow, this.activeBinding.exactWindow)) {
      return {
        ok: true,
        decision: "verification",
        intervention_id: active.id,
        epoch: active.epoch,
      };
    }

    if (active.status === "ready_to_resume" && this.resumeRequested) {
      if (!sameExact(candidate.exactWindow, this.activeBinding.exactWindow)) return { ok: true, decision: "deny" };
      this.state.resumeAgent(active.id);
      this.activeBinding = undefined;
      this.resumeRequested = false;
      this.surfaceLocator = undefined;
      try { this.checkpointStore.clear(); }
      catch { this.faulted = true; return { ok: true, decision: "deny" }; }
      return { ok: true, decision: "allow" };
    }
    return { ok: true, decision: "deny" };
  }

  reportVerification(request) {
    const candidate = this.parseAdmission({ ...request, verification_candidate: true });
    const active = this.state.getActive();
    if (!candidate || !active || !this.activeBinding
      || active.status !== "verifying"
      || request.intervention_id !== active.id
      || request.epoch !== active.epoch
      || !sameAuthority(candidate, this.activeBinding)
      || !sameExact(candidate.exactWindow, this.activeBinding.exactWindow)
      || typeof request.satisfied !== "boolean") {
      return { ok: false };
    }
    if (request.satisfied) this.state.markVerified(active.id);
    this.checkpoint();
    return { ok: true };
  }

  controlBinding(request) {
    if (request.authority !== undefined) {
      const binding = this.parseAdmission(request.authority);
      if (!binding?.exactWindow || binding.verificationCandidate) return undefined;
      return binding;
    }
    // Legacy Unix bridge compatibility only. Managed stdio control requires an explicit
    // CUMG-selected authority binding and never delegates target selection to this cache.
    return this.latestObservation;
  }

  begin(request = {}) {
    if (this.recovery || this.state.getActive()) return { ok: false };
    const binding = this.controlBinding(request);
    if (!binding?.exactWindow || this.now() - binding.observedAt > OBSERVATION_TTL_MS) return { ok: false };
    const intervention = this.state.begin({ reason: "operator_handoff", resumePolicy: "never_replay" });
    const owner = this.ownerFor(binding);
    if (!this.api.claimHandoffOwner(this.owners, intervention.id, intervention.status, owner)) {
      this.state.cancel(intervention.id);
      return { ok: false };
    }
    this.activeBinding = binding;
    this.resumeRequested = false;
    try {
      const locator = this.createSurfaceLocator(intervention);
      this.checkpoint();
      return { ok: true, intervention_id: intervention.id, epoch: intervention.epoch, status: intervention.status, surface: this.humanSurface?.kind ?? null, locator: locator ?? null, native_locator: this.humanSurface?.kind === "native" ? locator ?? null : null, webrtc_locator: this.humanSurface?.kind === "webrtc" ? locator ?? null : null };
    } catch {
      this.surfaceLocator = undefined;
      this.activeBinding = undefined;
      this.state.cancel(intervention.id);
      try { this.checkpointStore.clear(); }
      catch { this.faulted = true; }
      return { ok: false };
    }
  }

  reissueRecovery(binding, owner) {
    for (let epoch = 0; epoch <= this.recovery.epoch; epoch += 1) {
      if (epoch < this.recovery.epoch) this.state.advanceResourceEpoch();
    }
    const intervention = this.state.begin({ reason: "operator_handoff_recovery", resumePolicy: "never_replay" });
    if (!this.api.claimHandoffOwner(this.owners, intervention.id, intervention.status, owner)) {
      return { ok: false };
    }
    this.recovery = undefined;
    this.activeBinding = binding;
    this.resumeRequested = false;
    try {
      const locator = this.createSurfaceLocator(intervention);
      this.checkpoint();
      return { ok: true, intervention_id: intervention.id, epoch: intervention.epoch, status: intervention.status, surface: this.humanSurface?.kind ?? null, locator: locator ?? null, native_locator: this.humanSurface?.kind === "native" ? locator ?? null : null, webrtc_locator: this.humanSurface?.kind === "webrtc" ? locator ?? null : null };
    } catch {
      this.faulted = true;
      return { ok: false };
    }
  }

  recoverReissue(request = {}) {
    if (!this.recovery || this.recoveryExpired || this.state.getActive()) return { ok: false };
    const binding = this.controlBinding(request);
    if (!binding?.exactWindow || this.now() - binding.observedAt > OBSERVATION_TTL_MS) return { ok: false };
    const owner = this.ownerFor(binding);
    if (this.recovery.adapterKind !== "cumg_os_window_dogfood"
      || this.recovery.principalBinding !== owner.principalBinding
      || this.recovery.actionDigest !== owner.argsDigest) return { ok: false };
    return this.reissueRecovery(binding, owner);
  }

  recoverRebind(request) {
    if (!this.recovery || this.state.getActive()) return { ok: false };
    const binding = this.controlBinding(request);
    if (!binding?.exactWindow || this.now() - binding.observedAt > OBSERVATION_TTL_MS) return { ok: false };
    const priorContextBinding = interactionContextBinding(request.prior_context_id);
    if (!priorContextBinding) return { ok: false };
    const priorGeneration = request.prior_generation === undefined ? binding.generation : request.prior_generation;
    const priorCapabilityRevision = request.prior_capability_revision === undefined
      ? binding.capabilityRevision
      : request.prior_capability_revision;
    if (!positiveInt(priorGeneration) || binding.generation < priorGeneration
      || !Number.isSafeInteger(priorCapabilityRevision) || priorCapabilityRevision < 0
      || binding.capabilityRevision < priorCapabilityRevision) return { ok: false };
    const priorBinding = {
      ...binding,
      generation: priorGeneration,
      capabilityRevision: priorCapabilityRevision,
      exactWindow: { ...binding.exactWindow, contextBinding: priorContextBinding },
    };
    const priorOwner = this.ownerFor(priorBinding);
    if (this.recovery.adapterKind !== "cumg_os_window_dogfood"
      || this.recovery.principalBinding !== priorOwner.principalBinding
      || this.recovery.actionDigest !== priorOwner.argsDigest) return { ok: false };
    const result = this.reissueRecovery(binding, this.ownerFor(binding));
    if (result.ok) this.recoveryExpired = false;
    return result;
  }


  abandonExpiredRecovery(request) {
    if (!this.recovery || !this.recoveryExpired || this.state.getActive() || this.faulted
      || this.activeBinding || this.surfaceLocator
      || !positiveInt(request.expected_epoch)
      || request.expected_epoch !== this.recovery.epoch) return { ok: false };
    // Persist a privacy-bounded append-only operator decision before clearing the checkpoint.
    // If the audit cannot be durably appended, the recovery remains authoritative and faults closed.
    if (!this.abandonmentAudit) { this.faulted = true; return { ok: false }; }
    try {
      this.abandonmentAudit.record({
        timestampMs: this.now(),
        recoveryEpoch: this.recovery.epoch,
        priorClosedRecoveryStatus: this.recovery.status,
        result: "abandonment_authorized",
      });
    } catch { this.faulted = true; return { ok: false }; }
    // The audit proves an explicit operator abandonment decision. Checkpoint deletion still happens
    // before the in-memory recovery lock is released; failure leaves recovery authoritative.
    try { this.checkpointStore.clear(); }
    catch {
      try {
        this.abandonmentAudit.record({
          timestampMs: this.now(),
          recoveryEpoch: this.recovery.epoch,
          priorClosedRecoveryStatus: this.recovery.status,
          result: "checkpoint_clear_failed",
        });
      } catch {}
      this.faulted = true; return { ok: false };
    }
    this.recovery = undefined;
    this.recoveryExpired = false;
    this.latestObservation = undefined;
    this.resumeRequested = false;
    return { ok: true };
  }

  rebindLive(request) {
    if (this.recovery || this.faulted) return { ok: false };
    const active = this.state.getActive();
    if (!active || !this.activeBinding) return { ok: false };
    const binding = this.controlBinding(request);
    if (!binding?.exactWindow) return { ok: false };
    if (binding.generation <= this.activeBinding.generation
      || binding.capabilityRevision < this.activeBinding.capabilityRevision
      || binding.principalBinding !== this.activeBinding.principalBinding
      || binding.deviceBinding !== this.activeBinding.deviceBinding
      || binding.exactWindow.processId !== this.activeBinding.exactWindow.processId
      || binding.exactWindow.windowId !== this.activeBinding.exactWindow.windowId) {
      return { ok: false };
    }
    // Context identity is intentionally fresh after generation rollover. The exact OS Window
    // remains unchanged; no Human/Agent authority transition or intervention epoch change occurs.
    this.activeBinding = binding;
    this.latestObservation = binding;
    this.owners.set(active.id, this.ownerFor(binding));
    try { this.checkpoint(); }
    catch { this.faulted = true; return { ok: false }; }
    return { ok: true, intervention_id: active.id, epoch: active.epoch, status: active.status };
  }

  exactActive(request, expectedStatus) {
    const active = this.state.getActive();
    return active
      && active.status === expectedStatus
      && request.intervention_id === active.id
      && request.epoch === active.epoch
      ? active
      : undefined;
  }

  async shutdown() {
    const active = this.state.getActive();
    this.surfaceLocator = undefined;
    if (!active || !this.humanSurface) return;
    // Runtime shutdown fences the live Human transport but deliberately preserves the signed
    // checkpoint. A subsequent runtime must recover explicitly; it never revives authority.
    await this.humanSurface.revoke(active.id).catch(() => undefined);
  }

  control(request) {
    switch (request.action) {
    case "status":
      return safeStatus(this.state.getActive(), this.recovery, this.recoveryExpired, this.resumeRequested, this.latestObservation, this.humanSurface, this.surfaceLocator, this.faulted);
    case "begin":
      return this.begin(request);
    case "recover_reissue":
      return this.recoverReissue(request);
    case "recover_rebind":
      return this.recoverRebind(request);
    case "rebind_live":
      return this.rebindLive(request);
    case "abandon_expired_recovery":
      return this.abandonExpiredRecovery(request);
    case "request_resume": {
      const active = this.exactActive(request, "ready_to_resume");
      if (!active) return { ok: false };
      this.resumeRequested = true;
      this.checkpoint();
      return { ok: true, intervention_id: active.id, epoch: active.epoch, status: active.status };
    }
    case "cancel_before_human": {
      const active = this.exactActive(request, "awaiting_human");
      if (!active) return { ok: false };
      this.state.cancel(active.id);
      this.humanSurface?.revokeUnclaimed(active.id);
      this.surfaceLocator = undefined;
      this.activeBinding = undefined;
      this.resumeRequested = false;
      try { this.checkpointStore.clear(); }
      catch { this.faulted = true; return { ok: false }; }
      return { ok: true };
    }
    default:
      return { ok: false };
    }
  }

  handle(request) {
    if (!request || typeof request !== "object" || Array.isArray(request)) return { ok: false };
    if (request.action === "admit_agent") return this.admit(request);
    if (request.action === "report_verification") return this.reportVerification(request);
    return this.control(request);
  }

  handleManaged(request) {
    if (!request || typeof request !== "object" || Array.isArray(request)) return { ok: false };
    if (["begin", "recover_reissue", "recover_rebind", "rebind_live"].includes(request.action)
      && request.authority === undefined) return { ok: false };
    return this.handle(request);
  }
}

async function loadHandoff(root) {
  const modulePath = path.join(root, "dist", "index.js");
  const terminalPath = path.join(root, "dist", "experimental", "terminal-pty.js");
  const core = await import(pathToFileURL(modulePath).href);
  let terminal = {};
  try { terminal = await import(pathToFileURL(terminalPath).href); }
  catch (error) {
    if (error?.code !== "ERR_MODULE_NOT_FOUND") throw error;
  }
  return { ...core, ...terminal };
}

function privateKey(pathname) {
  const noFollow = process.platform === "win32" ? 0 : fs.constants.O_NOFOLLOW;
  const fd = fs.openSync(pathname, fs.constants.O_RDONLY | noFollow);
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || (process.platform !== "win32" && (stat.mode & 0o077) !== 0)) {
      throw new Error("checkpoint key must be a private regular file");
    }
    const key = fs.readFileSync(fd);
    if (key.length < 32 || key.length > 4096) throw new Error("checkpoint key size invalid");
    return key;
  } finally {
    fs.closeSync(fd);
  }
}

function serve(socketPath, bridge) {
  if (!path.isAbsolute(socketPath)) throw new Error("socket path must be absolute");
  if (fs.existsSync(socketPath)) throw new Error("socket path already exists; refuse implicit replacement");
  fs.mkdirSync(path.dirname(socketPath), { recursive: true, mode: 0o700 });
  const server = net.createServer((socket) => {
    let bytes = 0;
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      bytes += Buffer.byteLength(chunk);
      if (bytes > MAX_LINE) return socket.destroy();
      buffer += chunk;
      const newline = buffer.indexOf("\n");
      if (newline < 0) return;
      const line = buffer.slice(0, newline);
      buffer = "";
      let response;
      try { response = bridge.handle(JSON.parse(line)); }
      catch { response = { ok: false }; }
      socket.end(`${JSON.stringify(response)}\n`);
    });
  });
  server.listen(socketPath, () => fs.chmodSync(socketPath, 0o600));
  const cleanup = () => {
    server.close(() => {
      try { fs.unlinkSync(socketPath); } catch {}
      process.exit(0);
    });
  };
  process.on("SIGINT", cleanup);
  process.on("SIGTERM", cleanup);
  return server;
}


export async function serveStdio(bridge, input = process.stdin, output = process.stdout, terminalBridge = undefined) {
  const lines = createInterface({ input, crlfDelay: Infinity });
  for await (const line of lines) {
    if (Buffer.byteLength(line, "utf8") + 1 > MAX_LINE) throw new Error("handoff runtime request too large");
    let response;
    try {
      const request = JSON.parse(line);
      response = typeof request?.action === "string" && request.action.startsWith("terminal_")
        ? (terminalBridge?.handle(request) ?? { ok: false, code: "terminal_runtime_unavailable" })
        : (typeof bridge.handleManaged === "function" ? bridge.handleManaged(request) : bridge.handle(request));
    } catch { response = { ok: false }; }
    const encoded = JSON.stringify(response);
    if (Buffer.byteLength(encoded, "utf8") + 1 > MAX_LINE) throw new Error("handoff runtime response too large");
    if (!output.write(`${encoded}\n`)) {
      await new Promise((resolve) => output.once("drain", resolve));
    }
  }
}


function parseLoopbackBind(value, label = "http-bind") {
  const match = /^(127\.0\.0\.1):(\d{1,5})$/.exec(value ?? "");
  if (!match) throw new Error(`${label} must be 127.0.0.1:<port>`);
  const port = Number(match[2]);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) throw new Error(`${label} port invalid`);
  return { host: "127.0.0.1", port, baseUrl: `http://127.0.0.1:${port}/` };
}

async function readNodeRequest(req, baseUrl, maxBodyBytes = MAX_LINE) {
  const method = req.method ?? "GET";
  const chunks = [];
  let bytes = 0;
  if (method !== "GET" && method !== "HEAD") {
    for await (const chunk of req) {
      bytes += chunk.length;
      if (bytes > maxBodyBytes) throw new Error("handoff request too large");
      chunks.push(chunk);
    }
  }
  const headers = new Headers();
  for (const [name, value] of Object.entries(req.headers)) {
    if (Array.isArray(value)) for (const item of value) headers.append(name, item);
    else if (typeof value === "string") headers.set(name, value);
  }
  return new Request(new URL(req.url ?? "/", baseUrl), {
    method,
    headers,
    ...(method === "GET" || method === "HEAD" ? {} : { body: Buffer.concat(chunks) }),
  });
}

function serveSurface(bind, requestBaseUrl, bridge) {
  const server = http.createServer(async (req, res) => {
    try {
      const pathname = new URL(req.url ?? "/", requestBaseUrl).pathname;
      if (!pathname.startsWith("/takeover/")) {
        res.writeHead(404, { "content-type": "application/json", "cache-control": "no-store" });
        res.end('{"error":"not_found"}');
        return;
      }
      const request = await readNodeRequest(req, requestBaseUrl, 256 * 1024);
      const response = await bridge.handleSurface(request);
      const body = Buffer.from(await response.arrayBuffer());
      const headers = {};
      response.headers.forEach((value, key) => { headers[key] = value; });
      res.writeHead(response.status, headers);
      res.end(body);
    } catch {
      res.writeHead(503, { "content-type": "application/json", "cache-control": "no-store" });
      res.end('{"error":"takeover_unavailable"}');
    }
  });
  server.listen(bind.port, bind.host);
  return server;
}

function control(socketPath, action, options) {
  const payload = { action };
  if (options.has("intervention-id")) payload.intervention_id = options.get("intervention-id");
  if (options.has("epoch")) payload.epoch = Number(options.get("epoch"));
  if (options.has("prior-context-id")) payload.prior_context_id = options.get("prior-context-id");
  if (options.has("prior-generation")) payload.prior_generation = Number(options.get("prior-generation"));
  if (options.has("prior-capability-revision")) payload.prior_capability_revision = Number(options.get("prior-capability-revision"));
  if (options.has("expected-epoch")) payload.expected_epoch = Number(options.get("expected-epoch"));
  const socket = net.createConnection(socketPath);
  let data = "";
  socket.setEncoding("utf8");
  socket.on("connect", () => socket.write(`${JSON.stringify(payload)}\n`));
  socket.on("data", (chunk) => { data += chunk; });
  socket.on("end", () => process.stdout.write(`${data.trim()}\n`));
  socket.on("error", () => { process.stderr.write("operator handoff bridge unavailable\n"); process.exitCode = 2; });
}

export async function runCli(argv = process.argv) {
  const { command, options } = parseArgs(argv);
  if (!["serve", "serve-stdio"].includes(command)) {
    const socketPath = required(options, "socket", "CUMG_V2_OPERATOR_HANDOFF_SOCKET");
    const action = command.replaceAll("-", "_");
    if (!["status", "begin", "recover_reissue", "recover_rebind", "rebind_live", "abandon_expired_recovery", "request_resume", "cancel_before_human"].includes(action)) {
      throw new Error("unknown command");
    }
    control(socketPath, action, options);
    return;
  }

  const handoffRoot = required(options, "handoff-root", "CUMG_V2_HANDOFF_ROOT");
  const checkpointPath = required(options, "checkpoint", "CUMG_V2_HANDOFF_CHECKPOINT_FILE");
  const keyPath = required(options, "checkpoint-key", "CUMG_V2_HANDOFF_CHECKPOINT_KEY_FILE");
  if (!path.isAbsolute(handoffRoot) || !path.isAbsolute(checkpointPath) || !path.isAbsolute(keyPath)) {
    throw new Error("handoff/checkpoint paths must be absolute");
  }
  const api = await loadHandoff(handoffRoot);
  const nativeBindValue = optional(options, "native-http-bind", "CUMG_V2_HANDOFF_NATIVE_HTTP_BIND");
  const webRtcBindValue = optional(options, "webrtc-http-bind", "CUMG_V2_HANDOFF_WEBRTC_HTTP_BIND");
  if (nativeBindValue && webRtcBindValue) throw new Error("configure exactly one Human handoff surface");
  let humanSurface;
  let surfaceBind;
  let requestBaseUrl;
  if (nativeBindValue) {
    surfaceBind = parseLoopbackBind(nativeBindValue, "native-http-bind");
    const hostExecutable = required(options, "native-host-executable", "CUMG_V2_HANDOFF_NATIVE_HOST_EXECUTABLE");
    const revokeExecutable = required(options, "native-revoke-executable", "CUMG_V2_HANDOFF_NATIVE_REVOKE_EXECUTABLE");
    humanSurface = new NativeHandoffSurface(api, { baseUrl: surfaceBind.baseUrl, hostExecutable, revokeExecutable });
    requestBaseUrl = surfaceBind.baseUrl;
  } else if (webRtcBindValue) {
    surfaceBind = parseLoopbackBind(webRtcBindValue, "webrtc-http-bind");
    const publicBaseUrl = required(options, "webrtc-public-origin", "CUMG_V2_HANDOFF_WEBRTC_PUBLIC_ORIGIN");
    const hostExecutable = required(options, "webrtc-host-executable", "CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE");
    humanSurface = new WebRtcHandoffSurface(api, { publicBaseUrl, hostExecutable });
    requestBaseUrl = new URL(publicBaseUrl).toString();
  }
  const key = privateKey(keyPath);
  // SignedFileHandoffCheckpointStore retains the supplied Buffer for future writes. Keep a
  // dedicated in-memory copy alive for the runtime lifetime, then erase only the temporary read.
  const storeKey = Buffer.from(key);
  key.fill(0);
  const store = new api.SignedFileHandoffCheckpointStore(checkpointPath, storeKey);
  const abandonmentAuditPath = optional(
    options,
    "abandonment-audit",
    "CUMG_V2_HANDOFF_ABANDONMENT_AUDIT_FILE",
  ) ?? path.join(path.dirname(checkpointPath), "audit", "expired-recovery-abandonment.jsonl");
  const abandonmentAudit = new AppendOnlyAbandonmentAudit(abandonmentAuditPath);
  const bridge = new HandoffBridge(api, store, Date.now, humanSurface, abandonmentAudit);
  const terminalBridge = typeof api.ExperimentalTerminalPtyAuthority === "function"
    ? new TerminalPtyHandoffBridge(api)
    : undefined;
  if (command === "serve") {
    const socketPath = required(options, "socket", "CUMG_V2_OPERATOR_HANDOFF_SOCKET");
    serve(socketPath, bridge);
    if (surfaceBind && requestBaseUrl) serveSurface(surfaceBind, requestBaseUrl, bridge);
    return;
  }

  let surfaceServer;
  if (surfaceBind && requestBaseUrl) surfaceServer = serveSurface(surfaceBind, requestBaseUrl, bridge);
  try {
    await serveStdio(bridge, process.stdin, process.stdout, terminalBridge);
  } finally {
    await bridge.shutdown();
    if (surfaceServer) await new Promise((resolve) => surfaceServer.close(resolve));
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  runCli().catch((error) => {
    process.stderr.write(`handoff runtime refused: ${error.message}\n`);
    process.exitCode = 2;
  });
}
