---
name: looklook-pages-ask
description: Check whether a local HTTP dev server is already published through Looklook. Use whenever you start, find or are told about a local web/HTTP server port during development (e.g. Vite on localhost:5173, Next.js on :3000, a backend API on :8080, "Local: http://localhost:4321/" in command output) — if the port already has a Looklook address, give it to the user; if not, ask the user whether to create one.
---

# Looklook local pages — ask first

Looklook (installed on this computer) can publish a local port at the user's private address so they can
open it from a phone or another computer. Only the signed-in owner can open these addresses; they are not public.

This skill is the **ask-first** mode: look up existing mappings freely, but only create a new mapping after the
user says yes.

## When to act

Act when, during the session, a local **HTTP** server becomes relevant:

- you start a dev server / preview / API server yourself, or its output prints a URL such as
  `Local: http://localhost:5173/`, `ready on http://127.0.0.1:3000`, `Listening on :8080`;
- the user mentions a local web address or port they are working on;
- a project config makes the port obvious and the server is running (e.g. `vite.config` `server.port`).

Skip ports that are clearly not HTTP (databases such as 5432/3306/6379/27017, SSH, raw TCP, debugger/inspector
ports, HMR-only websocket ports) and ports that belong to Looklook itself. Act once per port per session; if the
user declines, do not ask again for that port.

## What to do

1. Call the `looklook` MCP tool `find_mapping` with the `port`. It never changes anything.
2. If `mapping` is not null:
   - enabled: tell the user the address in one short line, in the user's language, e.g.
     `Remote access: https://… (localhost:5173)` — the URL is `mapping.url`.
   - disabled: give the address and say it is currently turned off; ask whether to turn it back on. If yes,
     call `ensure_mapping` with the same port (it reuses and re-enables the existing mapping).
3. If `mapping` is null: ask the user once, briefly, whether they want a Looklook address for this port
   (e.g. "Publish localhost:5173 through Looklook so you can open it from your phone?"). Mention if the account
   is full (`count` >= `max`). Only if the user agrees, call `ensure_mapping` with `port` and a short `name`
   (project or app name, at most 32 characters), then report `mapping.url`.
   Do not block the main task while waiting — ask at a natural point and keep working.
4. Check `local.state` in the result:
   - `http`: nothing more to say.
   - `closed`: the server is not up yet; the address will work once it starts.
   - `ipv6_only`: Looklook forwards to 127.0.0.1 but the server listens on `[::1]` only. Tell the user, and if
     you started the server, restart it bound to 127.0.0.1 (for Vite: `--host 127.0.0.1`).
   - `not_http`: probably not a web server; say so before offering a mapping.

## Errors

The tools return `isError` with an `error` code and a `message`; relay the message briefly and continue with
the main task. In particular:

- `TUNNEL_LIMIT_REACHED`: the account is full. Call `list_mappings`, show the existing mappings and ask the user
  which one to delete in the Looklook console (Local pages). Do not delete anything yourself.
- `NOT_LOGGED_IN` / `LOCKED` / `NETWORK`: tell the user and stop trying for this session.
- If the `looklook` MCP server is unavailable or returns 401, tell the user to turn on / reinstall the AI
  assistant integration in the Looklook console (Local pages → AI assistants), then carry on without it.

Never print or ask for the Looklook MCP token.
