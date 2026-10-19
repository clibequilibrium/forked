/**
 * @forked-run/mcp, an MCP server that gives any agent undo.
 *
 * Exposes forked (versioned agent state: fork, rewind, replay, plus an
 * effectively-once side-effect log) as MCP tools, so Claude Code, Claude
 * Desktop, Cursor or any MCP client can checkpoint and rewind its own
 * working state with zero SDK integration.
 */

import { ForkedClient, ForkedError, type Json, type Op } from "@forked-run/client";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";

/** Options for {@link buildServer}. */
export interface ForkedMcpOptions {
  /** forked API base URL (self-hosted or https://api.forked.run). */
  baseUrl: string;
  /** API key (forked cloud) or bearer token (self-hosted, if configured). */
  apiKey?: string;
}

const INSTRUCTIONS = `forked gives you versioned state with an undo log. Recommended habits:
- Create one session per task (forked_create_session) and save a checkpoint
  with forked_save before risky or irreversible steps.
- If a path fails or looks wrong, forked_rewind to an earlier checkpoint and
  try differently; rewinds are exact and history is never lost.
- Before performing a side effect on the outside world (sending, paying,
  posting), call forked_find_effect with a stable key; if it exists, reuse
  the stored result instead of doing it again. After performing it, record
  it with forked_record_effect.
- Use forked_fork to explore alternatives in parallel branches, and
  forked_diff to compare two checkpoints.`;

function textResult(value: unknown) {
  return { content: [{ type: "text" as const, text: JSON.stringify(value, null, 2) }] };
}

function errorResult(e: unknown) {
  const message =
    e instanceof ForkedError
      ? `forked error (${e.status} ${e.code}): ${e.message}`
      : `error: ${e instanceof Error ? e.message : String(e)}`;
  return { content: [{ type: "text" as const, text: message }], isError: true as const };
}

/** Build the MCP server over a forked endpoint. */
export function buildServer(options: ForkedMcpOptions): McpServer {
  const forked = new ForkedClient({ baseUrl: options.baseUrl, apiKey: options.apiKey });
  const server = new McpServer(
    { name: "forked", version: "0.1.0" },
    { instructions: INSTRUCTIONS },
  );

  server.registerTool(
    "forked_create_session",
    {
      title: "Create a session",
      description:
        "Create a new forked session (one per task/run). Returns the session id to pass to every other forked tool.",
      inputSchema: {
        session_id: z
          .string()
          .regex(/^[A-Za-z0-9._-]{1,128}$/)
          .optional()
          .describe("Optional custom id; omit to generate one"),
      },
    },
    async ({ session_id }) => {
      try {
        const session = await forked.createSession(session_id);
        return textResult({ session_id: session.id, branch: "main" });
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_save",
    {
      title: "Save a checkpoint",
      description:
        "Commit key/value state onto a branch (creates a checkpoint you can rewind to). Keys you omit are left unchanged. Returns the new commit hash.",
      inputSchema: {
        session_id: z.string(),
        data: z.record(z.unknown()).describe("State to save: a JSON object of key → value"),
        branch: z.string().optional().describe("Branch name (default: main)"),
        message: z.string().optional().describe("Human-readable checkpoint message"),
      },
    },
    async ({ session_id, data, branch, message }) => {
      try {
        const ops: Op[] = Object.entries(data).map(([key, value]) => ({
          op: "put",
          key,
          value: value as Json,
        }));
        const head = await forked
          .session(session_id)
          .branch(branch ?? "main")
          .commit(ops, { meta: message ? { message } : {} });
        return textResult({ commit: head, saved_keys: Object.keys(data) });
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_load",
    {
      title: "Load current state",
      description: "Read a branch's full current state and its head commit hash.",
      inputSchema: {
        session_id: z.string(),
        branch: z.string().optional().describe("Branch name (default: main)"),
      },
    },
    async ({ session_id, branch }) => {
      try {
        return textResult(await forked.session(session_id).branch(branch ?? "main").state());
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_history",
    {
      title: "List checkpoints",
      description:
        "Walk a branch's history, newest first: commit hashes, messages and recorded effects. Use a hash with forked_rewind or forked_diff.",
      inputSchema: {
        session_id: z.string(),
        branch: z.string().optional(),
        limit: z.number().int().positive().max(100).optional().describe("Default 20"),
      },
    },
    async ({ session_id, branch, limit }) => {
      try {
        const entries = await forked
          .session(session_id)
          .branch(branch ?? "main")
          .log(limit ?? 20);
        return textResult(
          entries.map((c) => ({
            commit: c.hash,
            message: (c.meta as { message?: string }).message ?? null,
            effects: c.effects.map((e) => e.key),
          })),
        );
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_fork",
    {
      title: "Fork a branch",
      description:
        "Create a new branch from an existing one to explore an alternative. O(1): nothing is copied.",
      inputSchema: {
        session_id: z.string(),
        new_branch: z.string().regex(/^[A-Za-z0-9._-]{1,128}$/),
        from: z.string().optional().describe("Source branch (default: main)"),
      },
    },
    async ({ session_id, new_branch, from }) => {
      try {
        const handle = await forked.session(session_id).branch(from ?? "main").fork(new_branch);
        return textResult({ branch: handle.name });
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_rewind",
    {
      title: "Rewind (undo)",
      description:
        "Move a branch back to an earlier checkpoint. State is restored exactly; recorded side effects are never undone. Pass a commit hash from forked_history, or steps (how many checkpoints to go back, default 1).",
      inputSchema: {
        session_id: z.string(),
        branch: z.string().optional(),
        commit: z.string().optional().describe("Target commit hash"),
        steps: z.number().int().positive().max(100).optional().describe("Go back N checkpoints"),
      },
    },
    async ({ session_id, branch, commit, steps }) => {
      try {
        const handle = forked.session(session_id).branch(branch ?? "main");
        let target = commit;
        if (!target) {
          const log = await handle.log((steps ?? 1) + 1);
          const entry = log[steps ?? 1];
          if (!entry) return errorResult(new Error("not enough history to rewind that far"));
          target = entry.hash;
        }
        await handle.checkout(target);
        const { state } = await handle.state();
        return textResult({ rewound_to: target, state });
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_diff",
    {
      title: "Diff two checkpoints",
      description: "What changed between two commits (added/removed/modified keys).",
      inputSchema: {
        session_id: z.string(),
        a: z.string().describe("Older commit hash"),
        b: z.string().describe("Newer commit hash"),
      },
    },
    async ({ session_id, a, b }) => {
      try {
        return textResult(await forked.session(session_id).diff(a, b));
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_find_effect",
    {
      title: "Check if a side effect already happened",
      description:
        "Look up a side effect by its idempotency key BEFORE performing it. If a record exists, reuse its stored result, do NOT perform the effect again. Searches all branches, including rewound-away history.",
      inputSchema: {
        session_id: z.string(),
        key: z.string().describe("Stable idempotency key, e.g. 'send-email:report-42'"),
      },
    },
    async ({ session_id, key }) => {
      try {
        const record = await forked.session(session_id).findEffect(key);
        return textResult(
          record
            ? { already_done: true, record }
            : { already_done: false, note: "safe to perform; record it afterwards" },
        );
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  server.registerTool(
    "forked_record_effect",
    {
      title: "Record a performed side effect",
      description:
        "Record a side effect you just performed (email sent, payment made, API call issued) under a stable idempotency key. If the key already exists, nothing is written and the original stored result is returned, use that instead.",
      inputSchema: {
        session_id: z.string(),
        key: z.string(),
        kind: z.string().describe("Effect kind, e.g. 'email.send'"),
        payload: z.unknown().optional().describe("The input that produced the effect"),
        result: z.unknown().optional().describe("The observed result"),
        branch: z.string().optional(),
      },
    },
    async ({ session_id, key, kind, payload, result, branch }) => {
      try {
        const outcome = await forked.session(session_id).recordEffect(
          { key, kind, payload: (payload ?? null) as Json, result: (result ?? null) as Json },
          { branch },
        );
        return textResult(outcome);
      } catch (e) {
        return errorResult(e);
      }
    },
  );

  return server;
}
