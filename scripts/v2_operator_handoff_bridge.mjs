#!/usr/bin/env node
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import path from "node:path";
import { pathToFileURL } from "node:url";

const PROTOCOL = 1;
const MAX_LINE = 4096;
const OBSERVATION_TTL_MS = 60_000;
const CHECKPOINT_TTL_MS = 15 * 60_000;
const MAX_RECOVERED_EPOCH = 1_000_000;

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

function positiveInt(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function isBinding(value) {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
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

function safeStatus(active, recovery, resumeRequested, latest, nativeLocator, faulted) {
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
    resume_requested: resumeRequested,
    faulted: !!faulted,
    native_locator: nativeLocator ?? null,
    latest_exact_window: latest?.exactWindow ? {
      process_id: latest.exactWindow.processId,
      window_id: latest.exactWindow.windowId,
    } : null,
  };
}


const NATIVE_TTL_MS = 5 * 60_000;

function localNativeBrowserAdapter() {
  const unavailable = async () => { throw new Error("legacy browser surface disabled for CUMG native dogfood"); };
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
    this.broker = new api.TakeoverBroker(localNativeBrowserAdapter(), {
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
}

export class HandoffBridge {
  constructor(api, checkpointStore, now = Date.now, nativeSurface = undefined) {
    this.api = api;
    this.checkpointStore = checkpointStore;
    this.now = now;
    this.nativeSurface = nativeSurface;
    this.state = new api.ExecutionHandoffState(now);
    this.owners = new Map();
    this.activeBinding = undefined;
    this.latestObservation = undefined;
    this.resumeRequested = false;
    this.nativeLocator = undefined;
    this.faulted = false;
    this.recovery = checkpointStore.recover();
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

  createNativeLocator(intervention) {
    if (!this.nativeSurface || !this.activeBinding?.exactWindow) return undefined;
    const locator = this.nativeSurface.create(intervention, this.activeBinding);
    if (!locator) throw new Error("native handoff locator unavailable");
    this.nativeLocator = locator;
    return locator;
  }

  async handleNative(request) {
    const active = this.state.getActive();
    if (!this.nativeSurface || !active || !this.activeBinding || !this.nativeLocator || this.faulted) {
      return new Response(JSON.stringify({ error: "takeover_unavailable" }), { status: 404, headers: { "content-type": "application/json" } });
    }
    const match = /^\/takeover\/api\/(claim|reconnect|done|cancel)\//.exec(new URL(request.url).pathname);
    const response = await this.nativeSurface.handle(request, this.activeBinding.principalBinding);
    if (!match || response.status !== 200) return response;
    const operation = match[1];
    if (operation === "claim") {
      const current = this.state.getActive();
      if (!current || current.id !== active.id || current.epoch !== active.epoch || current.status !== "awaiting_human") {
        await this.nativeSurface.revoke(active.id).catch(() => undefined);
        this.faulted = true;
        return new Response(JSON.stringify({ error: "takeover_state_changed" }), { status: 409, headers: { "content-type": "application/json" } });
      }
      this.state.claimHuman(active.id);
      try { this.checkpoint(); }
      catch {
        await this.nativeSurface.revoke(active.id).catch(() => undefined);
        this.faulted = true;
        return new Response(JSON.stringify({ error: "takeover_checkpoint_failed" }), { status: 503, headers: { "content-type": "application/json" } });
      }
    } else if (operation === "done" || operation === "cancel") {
      const current = this.state.getActive();
      if (!current || current.id !== active.id || current.status !== "human_active") {
        this.faulted = true;
        return new Response(JSON.stringify({ error: "takeover_state_changed" }), { status: 409, headers: { "content-type": "application/json" } });
      }
      this.state.markHumanComplete(active.id);
      this.nativeLocator = undefined;
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
      this.nativeLocator = undefined;
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

  begin() {
    if (this.recovery || this.state.getActive()) return { ok: false };
    const binding = this.latestObservation;
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
      const locator = this.createNativeLocator(intervention);
      this.checkpoint();
      return { ok: true, intervention_id: intervention.id, epoch: intervention.epoch, status: intervention.status, native_locator: locator ?? null };
    } catch {
      this.nativeLocator = undefined;
      this.activeBinding = undefined;
      this.state.cancel(intervention.id);
      try { this.checkpointStore.clear(); }
      catch { this.faulted = true; }
      return { ok: false };
    }
  }

  recoverReissue() {
    if (!this.recovery || this.state.getActive()) return { ok: false };
    const binding = this.latestObservation;
    if (!binding?.exactWindow || this.now() - binding.observedAt > OBSERVATION_TTL_MS) return { ok: false };
    const owner = this.ownerFor(binding);
    if (this.recovery.adapterKind !== "cumg_os_window_dogfood"
      || this.recovery.principalBinding !== owner.principalBinding
      || this.recovery.actionDigest !== owner.argsDigest) return { ok: false };
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
      const locator = this.createNativeLocator(intervention);
      this.checkpoint();
      return { ok: true, intervention_id: intervention.id, epoch: intervention.epoch, status: intervention.status, native_locator: locator ?? null };
    } catch {
      this.faulted = true;
      return { ok: false };
    }
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

  control(request) {
    switch (request.action) {
    case "status":
      return safeStatus(this.state.getActive(), this.recovery, this.resumeRequested, this.latestObservation, this.nativeLocator, this.faulted);
    case "begin":
      return this.begin();
    case "recover_reissue":
      return this.recoverReissue();
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
      this.nativeSurface?.revokeUnclaimed(active.id);
      this.nativeLocator = undefined;
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
}

async function loadHandoff(root) {
  const modulePath = path.join(root, "dist", "index.js");
  return import(pathToFileURL(modulePath).href);
}

function privateKey(pathname) {
  const stat = fs.statSync(pathname);
  if (!stat.isFile() || (stat.mode & 0o077) !== 0) throw new Error("checkpoint key must be a private regular file");
  const key = fs.readFileSync(pathname);
  if (key.length < 32 || key.length > 4096) throw new Error("checkpoint key size invalid");
  return key;
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


function parseLoopbackBind(value) {
  const match = /^(127\.0\.0\.1):(\d{1,5})$/.exec(value ?? "");
  if (!match) throw new Error("native-http-bind must be 127.0.0.1:<port>");
  const port = Number(match[2]);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) throw new Error("native-http-bind port invalid");
  return { host: "127.0.0.1", port, baseUrl: `http://127.0.0.1:${port}/` };
}

async function readNodeRequest(req, baseUrl) {
  const method = req.method ?? "GET";
  const chunks = [];
  let bytes = 0;
  if (method !== "GET" && method !== "HEAD") {
    for await (const chunk of req) {
      bytes += chunk.length;
      if (bytes > MAX_LINE) throw new Error("native request too large");
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

function serveNative(bind, bridge) {
  const server = http.createServer(async (req, res) => {
    try {
      const pathname = new URL(req.url ?? "/", bind.baseUrl).pathname;
      if (!/^\/takeover\/api\/(claim|reconnect|done|cancel)\/[A-Za-z0-9-]{8,100}$/.test(pathname)) {
        res.writeHead(404, { "content-type": "application/json", "cache-control": "no-store" });
        res.end('{"error":"not_found"}');
        return;
      }
      const request = await readNodeRequest(req, bind.baseUrl);
      const response = await bridge.handleNative(request);
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
  const socket = net.createConnection(socketPath);
  let data = "";
  socket.setEncoding("utf8");
  socket.on("connect", () => socket.write(`${JSON.stringify(payload)}\n`));
  socket.on("data", (chunk) => { data += chunk; });
  socket.on("end", () => process.stdout.write(`${data.trim()}\n`));
  socket.on("error", () => { process.stderr.write("operator handoff bridge unavailable\n"); process.exitCode = 2; });
}

async function main() {
  const { command, options } = parseArgs(process.argv);
  const socketPath = required(options, "socket", "CUMG_V2_OPERATOR_HANDOFF_SOCKET");
  if (command !== "serve") {
    const action = command.replaceAll("-", "_");
    if (!["status", "begin", "recover_reissue", "request_resume", "cancel_before_human"].includes(action)) {
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
  let nativeSurface;
  let nativeBind;
  if (nativeBindValue) {
    nativeBind = parseLoopbackBind(nativeBindValue);
    const hostExecutable = required(options, "native-host-executable", "CUMG_V2_HANDOFF_NATIVE_HOST_EXECUTABLE");
    const revokeExecutable = required(options, "native-revoke-executable", "CUMG_V2_HANDOFF_NATIVE_REVOKE_EXECUTABLE");
    nativeSurface = new NativeHandoffSurface(api, { baseUrl: nativeBind.baseUrl, hostExecutable, revokeExecutable });
  }
  const key = privateKey(keyPath);
  // SignedFileHandoffCheckpointStore retains the supplied Buffer for future writes. Keep a
  // dedicated in-memory copy alive for the bridge lifetime, then erase only the temporary read.
  const storeKey = Buffer.from(key);
  key.fill(0);
  const store = new api.SignedFileHandoffCheckpointStore(checkpointPath, storeKey);
  const bridge = new HandoffBridge(api, store, Date.now, nativeSurface);
  serve(socketPath, bridge);
  if (nativeBind) serveNative(nativeBind, bridge);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch((error) => {
    process.stderr.write(`operator handoff bridge refused: ${error.message}\n`);
    process.exitCode = 2;
  });
}
