/**
 * @forked-run/langgraph, a LangGraph checkpointer backed by forked.
 *
 * Mapping:
 * - thread_id       → forked session
 * - checkpoint_ns   → branch ("main" for the root namespace)
 * - each checkpoint → one commit (serialized checkpoint in commit metadata,
 *                     JSON-safe channel values mirrored into readable state
 *                     keys, so the forked console tree shows real agent state)
 * - time travel     → a real branch rewind: resuming from an older
 *                     checkpoint checks the branch out to that commit before
 *                     committing, so the console shows the fork point.
 */

import {
  BaseCheckpointSaver,
  type ChannelVersions,
  type Checkpoint,
  type CheckpointListOptions,
  type CheckpointMetadata,
  type CheckpointPendingWrite,
  type CheckpointTuple,
  type PendingWrite,
  type SerializerProtocol,
  WRITES_IDX_MAP,
} from "@langchain/langgraph-checkpoint";
import type { RunnableConfig } from "@langchain/core/runnables";
import { ForkedClient, ForkedError, type Commit, type Json, type Op } from "@forked-run/client";

interface SerializedBlob {
  t: string;
  d: string;
}

interface CheckpointCommitMeta {
  lg: "checkpoint";
  checkpoint_id: string;
  checkpoint_ns: string;
  parent_checkpoint_id: string | null;
  checkpoint: SerializedBlob;
  metadata: SerializedBlob;
}

interface WritesCommitMeta {
  lg: "writes";
  checkpoint_id: string;
  task_id: string;
  writes: { idx: number; channel: string; t: string; d: string }[];
}

function toBase64(data: Uint8Array): string {
  const B = (globalThis as Record<string, any>).Buffer;
  if (B) return B.from(data).toString("base64");
  let bin = "";
  for (const byte of data) bin += String.fromCharCode(byte);
  return btoa(bin);
}

function fromBase64(encoded: string): Uint8Array {
  const B = (globalThis as Record<string, any>).Buffer;
  if (B) return new Uint8Array(B.from(encoded, "base64"));
  const bin = atob(encoded);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

const NAME_RE = /^[A-Za-z0-9._-]{1,128}$/;

async function safeName(prefix: string, raw: string): Promise<string> {
  if (NAME_RE.test(raw)) return raw;
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(raw));
  const hex = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
  return `${prefix}${hex.slice(0, 40)}`;
}

function isJsonSafe(value: unknown): boolean {
  if (value === null) return true;
  const t = typeof value;
  if (t === "string" || t === "boolean") return true;
  if (t === "number") return Number.isFinite(value as number);
  if (Array.isArray(value)) return (value as unknown[]).every(isJsonSafe);
  if (t === "object") {
    const proto = Object.getPrototypeOf(value);
    if (proto !== Object.prototype && proto !== null) return false;
    return Object.values(value as Record<string, unknown>).every(isJsonSafe);
  }
  return false;
}

/** Options for {@link ForkedSaver}. */
export interface ForkedSaverOptions {
  /** A configured forked client. */
  client: ForkedClient;
  /**
   * Also mirror JSON-safe channel values into readable state keys
   * (`channels/<name>`) so the forked console shows live agent state.
   * Default: true.
   */
  mirrorChannels?: boolean;
  /** Custom serializer (defaults to LangGraph's JSON+ serializer). */
  serde?: SerializerProtocol;
}

/**
 * A LangGraph checkpointer where every checkpoint is a forked commit.
 *
 * ```ts
 * import { ForkedClient } from "@forked-run/client";
 * import { ForkedSaver } from "@forked-run/langgraph";
 *
 * const checkpointer = new ForkedSaver({
 *   client: new ForkedClient({ baseUrl: "http://localhost:8080" }),
 * });
 * const graph = workflow.compile({ checkpointer });
 * ```
 */
export class ForkedSaver extends BaseCheckpointSaver {
  private readonly client: ForkedClient;
  private readonly mirrorChannels: boolean;
  /** Sessions already known to exist (`thread_id` → session id). */
  private readonly sessions = new Map<string, string>();
  /** Branches already known to exist (`session/branch`). */
  private readonly branches = new Set<string>();

  constructor(options: ForkedSaverOptions) {
    super(options.serde);
    this.client = options.client;
    this.mirrorChannels = options.mirrorChannels ?? true;
  }

  private async sessionFor(threadId: string): Promise<string> {
    const cached = this.sessions.get(threadId);
    if (cached) return cached;
    const sessionId = await safeName("lg-", threadId);
    try {
      await this.client.createSession(sessionId);
    } catch (e) {
      if (!(e instanceof ForkedError && e.code === "session_exists")) throw e;
    }
    this.sessions.set(threadId, sessionId);
    return sessionId;
  }

  private async branchFor(sessionId: string, checkpointNs: string): Promise<string> {
    const branch = checkpointNs === "" ? "main" : await safeName("ns-", `ns-${checkpointNs}`);
    if (branch === "main" || this.branches.has(`${sessionId}/${branch}`)) return branch;
    try {
      await this.client.session(sessionId).branch("main").fork(branch);
    } catch (e) {
      if (!(e instanceof ForkedError && e.code === "branch_exists")) throw e;
    }
    this.branches.add(`${sessionId}/${branch}`);
    return branch;
  }

  private threadConfig(threadId: string, checkpointNs: string, checkpointId: string): RunnableConfig {
    return {
      configurable: {
        thread_id: threadId,
        checkpoint_ns: checkpointNs,
        checkpoint_id: checkpointId,
      },
    };
  }

  private async tupleFromCommit(
    threadId: string,
    checkpointNs: string,
    commit: Commit,
    log: Commit[],
  ): Promise<CheckpointTuple> {
    const meta = commit.meta as unknown as CheckpointCommitMeta;
    const checkpoint = (await this.serde.loadsTyped(
      meta.checkpoint.t,
      fromBase64(meta.checkpoint.d),
    )) as Checkpoint;
    const metadata = (await this.serde.loadsTyped(
      meta.metadata.t,
      fromBase64(meta.metadata.d),
    )) as CheckpointMetadata;

    const pendingWrites: CheckpointPendingWrite[] = [];
    for (const entry of log) {
      const writesMeta = entry.meta as unknown as Partial<WritesCommitMeta>;
      if (writesMeta.lg === "writes" && writesMeta.checkpoint_id === meta.checkpoint_id) {
        const sorted = [...(writesMeta.writes ?? [])].sort((a, b) => a.idx - b.idx);
        for (const write of sorted) {
          pendingWrites.push([
            writesMeta.task_id ?? "",
            write.channel,
            await this.serde.loadsTyped(write.t, fromBase64(write.d)),
          ]);
        }
      }
    }

    return {
      config: this.threadConfig(threadId, checkpointNs, meta.checkpoint_id),
      checkpoint,
      metadata,
      parentConfig: meta.parent_checkpoint_id
        ? this.threadConfig(threadId, checkpointNs, meta.parent_checkpoint_id)
        : undefined,
      pendingWrites,
    };
  }

  private isCheckpointCommit(commit: Commit): boolean {
    return (commit.meta as { lg?: string }).lg === "checkpoint";
  }

  async getTuple(config: RunnableConfig): Promise<CheckpointTuple | undefined> {
    const threadId = config.configurable?.thread_id as string | undefined;
    if (!threadId) return undefined;
    const checkpointNs = (config.configurable?.checkpoint_ns as string | undefined) ?? "";
    const checkpointId = config.configurable?.checkpoint_id as string | undefined;

    const sessionId = await safeName("lg-", threadId);
    const branch = checkpointNs === "" ? "main" : await safeName("ns-", `ns-${checkpointNs}`);

    let log: Commit[];
    try {
      log = await this.client.session(sessionId).branch(branch).log();
    } catch (e) {
      if (e instanceof ForkedError && e.status === 404) return undefined;
      throw e;
    }

    const commit = checkpointId
      ? log.find(
          (c) =>
            this.isCheckpointCommit(c) &&
            (c.meta as { checkpoint_id?: string }).checkpoint_id === checkpointId,
        )
      : log.find((c) => this.isCheckpointCommit(c));
    if (!commit) return undefined;
    return this.tupleFromCommit(threadId, checkpointNs, commit, log);
  }

  async *list(
    config: RunnableConfig,
    options?: CheckpointListOptions,
  ): AsyncGenerator<CheckpointTuple> {
    const threadId = config.configurable?.thread_id as string | undefined;
    if (!threadId) return;
    const checkpointNs = (config.configurable?.checkpoint_ns as string | undefined) ?? "";
    const sessionId = await safeName("lg-", threadId);
    const branch = checkpointNs === "" ? "main" : await safeName("ns-", `ns-${checkpointNs}`);

    let log: Commit[];
    try {
      log = await this.client.session(sessionId).branch(branch).log();
    } catch (e) {
      if (e instanceof ForkedError && e.status === 404) return;
      throw e;
    }

    const beforeId = options?.before?.configurable?.checkpoint_id as string | undefined;
    let past = beforeId === undefined;
    let yielded = 0;
    for (const commit of log) {
      if (!this.isCheckpointCommit(commit)) continue;
      const meta = commit.meta as unknown as CheckpointCommitMeta;
      if (!past) {
        if (meta.checkpoint_id === beforeId) past = true;
        continue;
      }
      if (options?.filter) {
        const tupleMeta = (await this.serde.loadsTyped(
          meta.metadata.t,
          fromBase64(meta.metadata.d),
        )) as Record<string, unknown>;
        const matches = Object.entries(options.filter).every(
          ([key, wanted]) => JSON.stringify(tupleMeta[key]) === JSON.stringify(wanted),
        );
        if (!matches) continue;
      }
      yield await this.tupleFromCommit(threadId, checkpointNs, commit, log);
      yielded += 1;
      if (options?.limit !== undefined && yielded >= options.limit) return;
    }
  }

  async put(
    config: RunnableConfig,
    checkpoint: Checkpoint,
    metadata: CheckpointMetadata,
    _newVersions: ChannelVersions,
  ): Promise<RunnableConfig> {
    const threadId = config.configurable?.thread_id as string | undefined;
    if (!threadId) {
      throw new Error("ForkedSaver.put: config.configurable.thread_id is required");
    }
    const checkpointNs = (config.configurable?.checkpoint_ns as string | undefined) ?? "";
    const parentId = (config.configurable?.checkpoint_id as string | undefined) ?? null;

    const sessionId = await this.sessionFor(threadId);
    const branch = await this.branchFor(sessionId, checkpointNs);
    const handle = this.client.session(sessionId).branch(branch);

    // Time travel is a real rewind: when resuming from an older checkpoint,
    // move the branch head back to that commit first. forked anchors the
    // abandoned history, and the console shows the fork.
    if (parentId) {
      const log = await handle.log();
      const parentCommit = log.find(
        (c) =>
          this.isCheckpointCommit(c) &&
          (c.meta as { checkpoint_id?: string }).checkpoint_id === parentId,
      );
      if (parentCommit && log[0] && log[0].hash !== parentCommit.hash) {
        await handle.checkout(parentCommit.hash);
      }
    }

    const [cpType, cpBytes] = await this.serde.dumpsTyped(checkpoint);
    const [mdType, mdBytes] = await this.serde.dumpsTyped(metadata);
    const meta: CheckpointCommitMeta = {
      lg: "checkpoint",
      checkpoint_id: checkpoint.id,
      checkpoint_ns: checkpointNs,
      parent_checkpoint_id: parentId,
      checkpoint: { t: cpType, d: toBase64(cpBytes) },
      metadata: { t: mdType, d: toBase64(mdBytes) },
    };

    const ops: Op[] = [{ op: "put", key: "checkpoint_id", value: checkpoint.id }];
    if (this.mirrorChannels) {
      for (const [channel, value] of Object.entries(checkpoint.channel_values ?? {})) {
        if (isJsonSafe(value)) {
          ops.push({ op: "put", key: `channels/${channel}`, value: value as Json });
        }
      }
    }

    await handle.commit(ops, { meta: meta as unknown as Record<string, Json> });
    return this.threadConfig(threadId, checkpointNs, checkpoint.id);
  }

  async putWrites(config: RunnableConfig, writes: PendingWrite[], taskId: string): Promise<void> {
    const threadId = config.configurable?.thread_id as string | undefined;
    const checkpointId = config.configurable?.checkpoint_id as string | undefined;
    if (!threadId || !checkpointId) {
      throw new Error(
        "ForkedSaver.putWrites: thread_id and checkpoint_id are required in config.configurable",
      );
    }
    const checkpointNs = (config.configurable?.checkpoint_ns as string | undefined) ?? "";
    const sessionId = await this.sessionFor(threadId);
    const branch = await this.branchFor(sessionId, checkpointNs);

    const serialized = await Promise.all(
      writes.map(async ([channel, value], i) => {
        const [t, bytes] = await this.serde.dumpsTyped(value);
        return {
          idx: WRITES_IDX_MAP[channel] ?? i,
          channel,
          t,
          d: toBase64(bytes),
        };
      }),
    );
    const meta: WritesCommitMeta = {
      lg: "writes",
      checkpoint_id: checkpointId,
      task_id: taskId,
      writes: serialized,
    };
    await this.client
      .session(sessionId)
      .branch(branch)
      .commit([], { meta: meta as unknown as Record<string, Json> });
  }

  async deleteThread(_threadId: string): Promise<void> {
    throw new Error(
      "ForkedSaver.deleteThread: forked does not support deleting sessions yet; " +
        "sessions expire per your retention settings instead",
    );
  }
}

export { ForkedClient, ForkedError } from "@forked-run/client";
