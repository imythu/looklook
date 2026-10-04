---
name: looklook-pages-auto
description: Automatically publish local HTTP dev servers through Looklook. Use whenever you start, find or are told about a local web/HTTP server port during development (e.g. Vite on localhost:5173, Next.js on :3000, a backend API on :8080, "Local: http://localhost:4321/" in command output) — create or reuse a Looklook mapping for that port and give the user the access URL without asking.
---

# Looklook local pages — automatic

Looklook (installed on this computer) can publish a local port at the user's private address so they can
open it from a phone or another computer. Only the signed-in owner can open these addresses; they are not public.

This skill is the **automatic** mode: when an HTTP port appears, publish it right away and report the URL.
Do not ask for confirmation first.

## When to act

Act when, during the session, a local **HTTP** server becomes relevant:

- you start a dev server / preview / API server yourself, or its output prints a URL such as
  `Local: http://localhost:5173/`, `ready on http://127.0.0.1:3000`, `Listening on :8080`;
- the user mentions a local web address or port they are working on;
- a project config makes the port obvious and the server is running (e.g. `vite.config` `server.port`).

Skip ports that are clearly not HTTP (databases such as 5432/3306/6379/27017, SSH, raw TCP, debugger/inspector
ports, HMR-only websocket ports) and ports that belong to Looklook itself. Act once per port per session; do not
call the tools repeatedly for a port you already handled.

## What to do

1. Call the `looklook` MCP tool `ensure_mapping` with `port` and a short `name` (the project or app name,
   at most 32 characters). It reuses the existing mapping for that port if there is one (re-enabling it if it
   was disabled) and only creates a new one otherwise — never create duplicates yourself.
2. Tell the user the result in one short line, in the user's language, e.g.
   `Remote access: https://… (localhost:5173, reused)` — the URL comes from `mapping.url`.
   Say whether it was created, reused or re-enabled.
3. Check `local.state` in the result:
   - `http`: nothing more to say.
   - `closed`: the server is not up yet; the address will work once it starts. Mention it only if the server
     should already be running.
   - `ipv6_only`: Looklook forwards to 127.0.0.1 but the server listens on `[::1]` only. Tell the user, and if
     you started the server, restart it bound to 127.0.0.1 (for Vite: `--host 127.0.0.1`).
   - `not_http`: probably not a web server; tell the user the mapping may not be useful.

## Errors

The tool returns `isError` with an `error` code and a `message`; relay the message briefly and continue with
the main task. In particular:

- `TUNNEL_LIMIT_REACHED`: the account is full. Call `list_mappings`, show the existing mappings and ask the user
  which one to delete in the Looklook console (Local pages). Do not delete anything yourself.
- `NOT_LOGGED_IN` / `LOCKED` / `NETWORK`: tell the user and stop trying for this session.
- If the `looklook` MCP server is unavailable or returns 401, tell the user to turn on / reinstall the AI
  assistant integration in the Looklook console (Local pages → AI assistants), then carry on without it.

Never print or ask for the Looklook MCP token.
