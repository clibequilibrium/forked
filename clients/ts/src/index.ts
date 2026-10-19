/**
 * @forked-run/client, TypeScript client for the forked v1 API.
 *
 * Works against a self-hosted `forked-server` or forked cloud; only
 * `baseUrl` (and the API key) changes. Zero dependencies; uses global
 * `fetch` (Node 18+, Bun, Deno, browsers, edge runtimes).
 */

/** Any JSON value. */
export type Json =
  | null
  | boolean
  | number
  | string
  | Json[]
  | { [key: string]: Json };

/** A key→value state snapshot. */
export type StateMap = Record<string, Json>;

/** One state operation inside a commit. */
export type Op =
  | { op: "put"; key: string; value: Json }
  | { op: "delete"; key: string };

/** A side-effect record. */
export interface EffectRecord {
  /** Idempotency key, unique per session. */
  key: string;
  /** Effect kind, free-form (e.g. "email.send"). */
  kind: string;
  /** The input that produced the effect. */
  payload: Json;
  /** The observed result; returned verbatim on idempotent replays. */
  result: Json;
}

/** A commit on the wire. */
export interface Commit {
  hash: string;
  parent: string | null;
  root: string;
  meta: Record<string, Json>;
  effects: EffectRecord[];
}

/** One changed key in a diff. */
export interface DiffEntry {
  key: string;
  change: "added" | "removed" | "modified";
  old?: Json;
  new?: Json;
}

/** A branch name + head pair. */
export interface Branch {
  name: string;
  head: string;
}

/** The whole session graph, for visualization or custom walks. */
export interface SessionGraph {
  branches: Branch[];
  anchors: string[];
  commits: Commit[];
}

/** Options for {@link ForkedClient}. */
export interface ForkedClientOptions {
  /**
   * API base URL, e.g. `http://localhost:8080` (self-hosted) or
   * `https://api.forked.run` (forked cloud).
   */
  baseUrl: string;
  /** Bearer credential: an `fk_live_…` API key, or `FORKED_TOKEN` value. */
  apiKey?: string;
  /** Custom fetch implementation (defaults to global fetch). */
  fetch?: typeof fetch;
}

/** Commit options. */
export interface CommitOptions {
  /** Commit metadata (message, actor, timestamps, …). */
  meta?: Record<string, Json>;
  /** Effects recorded atomically with the commit (no idempotency check). */
  effects?: EffectRecord[];
  /**
   * Optimistic concurrency: only commit if the branch head still equals
   * this hash; otherwise the server returns 409 and this client throws
   * a {@link ForkedError} with `code === "conflict"`.
   */
  expectedHead?: string;
}

/** Result of recording an effect. */
export interface EffectOutcome {
  /** `"recorded"` for a new effect, `"already_recorded"` on replay. */
  status: "recorded" | "already_recorded";
  /** The stored record (original result on replay). */
  record: EffectRecord;
  /** The commit carrying the record (absent on replay). */
  commit?: string;
}

/** An error returned by the forked API. */
export class ForkedError extends Error {
  /** HTTP status code. */
  readonly status: number;
  /** Stable machine-readable code, e.g. `"conflict"`. */
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ForkedError";
    this.status = status;
    this.code = code;
  }

  /** True if this is an optimistic-concurrency conflict (retryable). */
  get isConflict(): boolean {
    return this.code === "conflict";
  }
}

/** Client for the forked v1 API. */
export class ForkedClient {
  readonly baseUrl: string;
  private readonly apiKey?: string;
  private readonly fetchImpl: typeof fetch;

  constructor(options: ForkedClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.apiKey = options.apiKey;
    this.fetchImpl = options.fetch ?? fetch;
  }

  /** Check connectivity. */
  async health(): Promise<void> {
    await this.request("GET", "/v1/health");
  }

  /**
   * Create a session (optionally with a chosen id) and return a handle to
   * it.
   */
  async createSession(sessionId?: string): Promise<Session> {
    const body = await this.request<{
      session_id: string;
      branch: string;
      head: string;
    }>("POST", "/v1/sessions", sessionId ? { session_id: sessionId } : {});
    return new Session(this, body.session_id);
  }

  /** A handle to an existing session (no network call). */
  session(sessionId: string): Session {
    return new Session(this, sessionId);
  }

  /** @internal */
  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = {};
    if (this.apiKey) headers["authorization"] = `Bearer ${this.apiKey}`;
    let payload: string | undefined;
    if (body !== undefined) {
      headers["content-type"] = "application/json";
      payload = JSON.stringify(body);
    }
    const response = await this.fetchImpl(`${this.baseUrl}${path}`, {
      method,
      headers,
      body: payload,
    });
    const text = await response.text();
    let parsed: unknown = undefined;
    if (text.length > 0) {
      try {
        parsed = JSON.parse(text);
      } catch {
        throw new ForkedError(
          response.status,
          "invalid_response",
          `non-JSON response from server: ${text.slice(0, 200)}`,
        );
      }
    }
    if (!response.ok) {
      const err = (parsed as { error?: { code?: string; message?: string } })
        ?.error;
      throw new ForkedError(
        response.status,
        err?.code ?? "unknown",
        err?.message ?? `HTTP ${response.status}`,
      );
    }
    return parsed as T;
  }
}

/** A handle to one session. */
export class Session {
  constructor(
    private readonly client: ForkedClient,
    /** The session id. */
    readonly id: string,
  ) {}

  /** A handle to a branch of this session (no network call). */
  branch(name = "main"): BranchHandle {
    return new BranchHandle(this.client, this.id, name);
  }

  /** List branches with their heads. */
  async branches(): Promise<Branch[]> {
    const body = await this.client.request<{ branches: Branch[] }>(
      "GET",
      `/v1/sessions/${this.id}/branches`,
    );
    return body.branches;
  }

  /** Diff two commits (old → new). */
  async diff(a: string, b: string): Promise<DiffEntry[]> {
    const body = await this.client.request<{ entries: DiffEntry[] }>(
      "GET",
      `/v1/sessions/${this.id}/diff?a=${encodeURIComponent(a)}&b=${encodeURIComponent(b)}`,
    );
    return body.entries;
  }

  /** The whole session graph (branches, anchors, every commit). */
  async graph(): Promise<SessionGraph> {
    return this.client.request<SessionGraph>(
      "GET",
      `/v1/sessions/${this.id}/graph`,
    );
  }

  /**
   * Look up an effect by idempotency key without recording anything -
   * searches every branch, including history rewound away. Returns the
   * stored record or `null`.
   *
   * The effectively-once recipe: check with `findEffect`, do the work only if
   * `null`, then {@link recordEffect} the result (which also resolves a
   * lost race by returning the already-stored record).
   */
  async findEffect(key: string): Promise<EffectRecord | null> {
    const body = await this.client.request<{ record: EffectRecord | null }>(
      "GET",
      `/v1/sessions/${this.id}/effects?key=${encodeURIComponent(key)}`,
    );
    return body.record;
  }

  /**
   * Record a side effect idempotently. If the key was already recorded
   * anywhere in this session, any branch, even history rewound away -
   * the stored record comes back with `status: "already_recorded"`; use
   * `record.result` instead of re-running the effect.
   */
  async recordEffect(
    effect: Omit<EffectRecord, "payload" | "result"> &
      Partial<Pick<EffectRecord, "payload" | "result">>,
    options?: { branch?: string },
  ): Promise<EffectOutcome> {
    return this.client.request<EffectOutcome>(
      "POST",
      `/v1/sessions/${this.id}/effects`,
      {
        branch: options?.branch,
        key: effect.key,
        kind: effect.kind,
        payload: effect.payload ?? null,
        result: effect.result ?? null,
      },
    );
  }
}

/** A handle to one branch of a session. */
export class BranchHandle {
  constructor(
    private readonly client: ForkedClient,
    /** The session id. */
    readonly sessionId: string,
    /** The branch name. */
    readonly name: string,
  ) {}

  private path(suffix: string): string {
    return `/v1/sessions/${this.sessionId}/branches/${this.name}${suffix}`;
  }

  /** The branch's full state and the head it was read at. */
  async state(): Promise<{ head: string; state: StateMap }> {
    return this.client.request("GET", this.path("/state"));
  }

  /** Read a single key (convenience over {@link state}). */
  async get(key: string): Promise<Json | undefined> {
    const { state } = await this.state();
    return state[key];
  }

  /** Commit operations; returns the new head hash. */
  async commit(ops: Op[], options?: CommitOptions): Promise<string> {
    const body = await this.client.request<{ head: string }>(
      "POST",
      this.path("/commit"),
      {
        ops,
        meta: options?.meta ?? {},
        effects: options?.effects ?? [],
        expected_head: options?.expectedHead,
      },
    );
    return body.head;
  }

  /** Convenience: commit a set of puts from an object. */
  async put(
    values: Record<string, Json>,
    options?: CommitOptions,
  ): Promise<string> {
    const ops: Op[] = Object.entries(values).map(([key, value]) => ({
      op: "put",
      key,
      value,
    }));
    return this.commit(ops, options);
  }

  /** Fork this branch, O(1) on the server, nothing is copied. */
  async fork(newBranch: string): Promise<BranchHandle> {
    await this.client.request("POST", this.path("/fork"), {
      branch: newBranch,
    });
    return new BranchHandle(this.client, this.sessionId, newBranch);
  }

  /**
   * Rewind this branch to an earlier commit. State is restored exactly;
   * recorded effects are never undone.
   */
  async checkout(commit: string): Promise<void> {
    await this.client.request("POST", this.path("/checkout"), { commit });
  }

  /** The branch's history, newest first. */
  async log(limit?: number): Promise<Commit[]> {
    const params = new URLSearchParams({ branch: this.name });
    if (limit !== undefined) params.set("limit", String(limit));
    const body = await this.client.request<{ entries: Commit[] }>(
      "GET",
      `/v1/sessions/${this.sessionId}/log?${params.toString()}`,
    );
    return body.entries;
  }

  /** The branch's current head hash. */
  async head(): Promise<string> {
    const { head } = await this.state();
    return head;
  }
}
