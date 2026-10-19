#!/usr/bin/env node
/**
 * Stdio entrypoint:
 *   FORKED_URL=http://localhost:8080 npx @forked/mcp
 * or for forked cloud:
 *   FORKED_URL=https://api.forked.run FORKED_API_KEY=fk_live_… npx @forked/mcp
 */

import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { buildServer } from "./index.js";

const baseUrl = process.env.FORKED_URL ?? "http://localhost:8080";
const apiKey = process.env.FORKED_API_KEY;

const server = buildServer({ baseUrl, apiKey });
const transport = new StdioServerTransport();
await server.connect(transport);
console.error(`forked-mcp: connected (forked at ${baseUrl})`);
