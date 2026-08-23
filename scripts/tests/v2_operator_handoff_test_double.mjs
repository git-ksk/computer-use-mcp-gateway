import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export class ExecutionHandoffState {
  constructor(now = Date.now) {
    this.now = now;
    this.epoch = 0;
    this.active = undefined;
  }
  getActive() { return this.active ? { ...this.active } : undefined; }
  advanceResourceEpoch() { this.epoch += 1; return this.epoch; }
  begin({ reason, resumePolicy }) {
    if (this.active) return { ...this.active };
    const now = this.now();
    this.active = {
      id: crypto.randomUUID(), reason, status: "awaiting_human", authority: "none",
      epoch: this.advanceResourceEpoch(), resumePolicy, createdAt: now, updatedAt: now,
    };
    return { ...this.active };
  }
  claimHuman(id) {
    if (!this.active || this.active.id !== id || this.active.status !== "awaiting_human") throw new Error("state");
    this.active.status = "human_active"; this.active.authority = "human"; this.active.updatedAt = this.now();
    return { ...this.active };
  }
  markHumanComplete(id) {
    if (!this.active || this.active.id !== id || this.active.status !== "human_active") throw new Error("state");
    this.active.status = "verifying"; this.active.authority = "none"; this.active.epoch = this.advanceResourceEpoch(); this.active.updatedAt = this.now();
    return { ...this.active };
  }
  markVerified(id) {
    if (!this.active || this.active.id !== id || this.active.status !== "verifying") throw new Error("state");
    this.active.status = "ready_to_resume"; this.active.updatedAt = this.now(); return { ...this.active };
  }
  resumeAgent(id) {
    if (!this.active || this.active.id !== id || this.active.status !== "ready_to_resume") throw new Error("state");
    const decision = { epoch: this.active.epoch, resumePolicy: this.active.resumePolicy };
    this.active = undefined;
    return decision;
  }
  cancel(id) {
    if (!this.active || this.active.id !== id) throw new Error("state");
    this.active = undefined; this.advanceResourceEpoch();
  }
}

function stable(value) {
  if (Array.isArray(value)) return `[${value.map(stable).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map((k) => `${JSON.stringify(k)}:${stable(value[k])}`).join(",")}}`;
  return JSON.stringify(value);
}

export function createHandoffOwner(principalBinding, toolName, args, resumeStrategy) {
  return {
    principalBinding, toolName, resumeStrategy,
    argsDigest: crypto.createHash("sha256").update(`${toolName}\0${stable(args)}`).digest("hex"),
  };
}

export function claimHandoffOwner(owners, interventionId, interventionStatus, candidate) {
  const existing = owners.get(interventionId);
  if (existing) return stable(existing) === stable(candidate) ? existing : undefined;
  if (interventionStatus !== "awaiting_human") return undefined;
  owners.set(interventionId, candidate);
  return candidate;
}

export class SignedFileHandoffCheckpointStore {
  constructor(filePath, signingKey, now = Date.now) {
    this.filePath = filePath;
    this.signingKey = Buffer.from(signingKey);
    this.now = now;
  }
  write(checkpoint) {
    fs.mkdirSync(path.dirname(this.filePath), { recursive: true, mode: 0o700 });
    const mac = crypto.createHmac("sha256", this.signingKey).update(JSON.stringify(checkpoint)).digest("hex");
    fs.writeFileSync(this.filePath, `${JSON.stringify({ checkpoint, mac })}\n`, { mode: 0o600 });
  }
  readVerified() {
    if (!fs.existsSync(this.filePath)) return undefined;
    const envelope = JSON.parse(fs.readFileSync(this.filePath, "utf8"));
    const expected = crypto.createHmac("sha256", this.signingKey).update(JSON.stringify(envelope.checkpoint)).digest("hex");
    if (envelope.mac !== expected) { const error = new Error("checkpoint"); error.code = "CHECKPOINT_INVALID"; throw error; }
    return envelope.checkpoint;
  }
  recover() {
    const checkpoint = this.readVerified();
    if (!checkpoint) return undefined;
    if (checkpoint.expiresAt <= this.now()) { const error = new Error("checkpoint expired"); error.code = "CHECKPOINT_EXPIRED"; throw error; }
    return { ...checkpoint, recovery: "reissue_and_revalidate" };
  }
  recoverForOperatorRevalidation() {
    const checkpoint = this.readVerified();
    return checkpoint ? { ...checkpoint, recovery: "reissue_and_revalidate" } : undefined;
  }
  clear() {
    try { fs.unlinkSync(this.filePath); } catch (error) { if (error.code !== "ENOENT") throw error; }
  }
}
