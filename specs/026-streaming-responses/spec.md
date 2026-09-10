---
id: "026-streaming-responses"
title: "Streaming responses: server-sent events as the one mechanism, declared per route, bounded and cancellable"
status: approved
kind: "kernel"
domain: "edge"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: medium
wave: 2
depends_on:
  - "020-edge-server"
  - "023-observability"
  - "025-api-tokens-and-resource-server"
establishes:
  - "crates/rahi-edge/src/stream.rs"
  - "crates/rahi-edge/tests/stream.rs"
extends:
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/router.rs", nature: additive }
  - { spec: "020-edge-server", unit: "crates/rahi-edge/src/lib.rs", nature: additive }
summary: >
  A response that is produced over seconds rather than milliseconds breaks
  every default the edge holds: compression buffers it, the request timeout
  kills it, the rate limiter counts it once and forgets it, and graceful
  shutdown drops it mid-sentence. This spec adds one streaming mechanism,
  server-sent events, and makes a streaming route declare itself so those
  four defaults are turned off deliberately rather than discovered in
  production. Streams are bounded by a per-stream channel and a per-client
  concurrency limit, are cancellation-aware, close cleanly on shutdown, and
  are one span each in the tracer.
---

# 026: Streaming responses

## 1. Purpose

Spec 020 built the edge for request and response. Two consumers of this
chassis need a response that stays open: a memory service speaking MCP over
streamable HTTP, whose transport is server-sent events by definition, and a
governed delivery plane that reports a long-running verification to a
watching operator. Neither can be served by polling without giving up the
latency that makes them feel live.

The reason this is a chassis spec rather than an application concern is
that a stream is not a route feature, it is an exemption from four
middleware behaviors that the chassis owns. An application cannot opt out
of the edge's compression or its timeout, and it should not learn that it
needed to by watching a proxy buffer sixty seconds of events into one
delivery.

## 2. Territory

One module and one test in `crates/rahi-edge`. Extends 020's `router.rs`
with the route marker and 020's `lib.rs` with the exports. Adds no
dependency beyond the `Sse` types axum already provides.

## 3. Behavior

- **B-1 (one mechanism).** Server-sent events over HTTP is the only
  streaming mechanism the chassis offers. WebSockets are refused: a second
  mechanism doubles the middleware exemption surface, needs its own
  authentication story because browsers send no headers on the upgrade, and
  no consumer requires bidirectional frames.
- **B-2 (the response).** `stream(rx) -> Response` produces
  `Content-Type: text/event-stream`, `Cache-Control: no-store`,
  `Connection: keep-alive`, and `X-Accel-Buffering: no` (which stops an
  intermediary proxy from buffering), and emits a comment keep-alive every
  fifteen seconds when the source is idle. Events carry `event`, `data`,
  and an optional monotonic `id`.
- **B-3 (declared, not inferred).** A route is registered as streaming
  through `Router::stream_route(path, handler)`, which records the fact in
  the route table. The exemptions of B-4 apply to declared routes only, and
  a handler that returns a streaming response from a non-declared route is
  a 500 with a Decision, because the exemptions would not have been
  applied and the stream would misbehave silently.
- **B-4 (the four exemptions).** On a declared route: response compression
  is disabled; the response body size limit does not apply; the request
  timeout applies to the time before the first byte only, after which the
  connection is governed by B-5 and B-7; and the rate limiter of 020 admits
  the request once and then counts the stream against a concurrency budget
  rather than a request budget.
- **B-5 (concurrency budget).** A client identity (024) may hold
  `max_concurrent_streams` open streams, default four. The next attempt
  answers 429 with `Retry-After` and a ledgered Decision. The budget is per
  identity, and for a bearer-authenticated request the identity is the
  `(client_id, sub)` pair of 025 B-12.
- **B-6 (backpressure).** Each stream owns a bounded channel, default 64
  events. When the consumer is too slow to drain it, the stream is closed
  with `event: overflow` and the producer task is cancelled. Memory per
  stream is therefore a constant the operator can multiply, and one slow
  reader can neither stall a producer nor grow the heap.
- **B-7 (cancellation and shutdown).** A stream task holds a cancellation
  token. A dropped client cancels it within one keep-alive interval.
  Graceful shutdown emits `event: shutdown` to every open stream, waits
  `stream_drain_timeout` (default ten seconds) for them to close, then
  aborts the remainder; the listener does not stop before this completes.
- **B-8 (resumption is not offered).** The chassis emits `id` when the
  producer supplies one and ignores `Last-Event-ID` on reconnect. Replay
  requires knowing what a client already consumed, which is application
  state; a product that needs resumable streams implements it over its own
  cursor and says so.
- **B-9 (observability).** One span per stream, closed with an outcome of
  `complete`, `client_gone`, `overflow`, or `shutdown`. Metrics:
  `rahi_streams_open` (gauge), `rahi_stream_duration_seconds` (histogram),
  `rahi_stream_events_total`, and `rahi_streams_closed_total` labelled by
  outcome.
- **B-10 (the proxy is untouched).** The raw `/auth/*` proxy of 021
  already forwards bodies without buffering and is not a declared
  streaming route; this spec changes nothing about it.

## 4. Functional requirements

- **FR-001.** A declared route's response carries the four headers of B-2,
  and no `Content-Encoding`, even when the request sent
  `Accept-Encoding: gzip`.
- **FR-002.** An idle stream delivers a keep-alive within twenty seconds
  and the connection stays open past the configured request timeout.
- **FR-003.** A consumer that reads nothing while the producer emits 200
  events receives `event: overflow` and a closed stream, and the test
  asserts the producer task ended.
- **FR-004.** A fifth concurrent stream from one identity answers 429 and
  emits one Decision; closing an earlier stream frees the slot.
- **FR-005.** Graceful shutdown with two open streams emits `event:
  shutdown` on both and returns from the shutdown future within the drain
  timeout.
- **FR-006.** A handler returning a streaming response from an undeclared
  route answers 500 and emits a Decision naming the route.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p rahi-edge --locked --test stream` passes.
- **AC-2.** `/metrics` exposes `rahi_streams_open` and the closed counter
  labelled by all four outcomes after the test suite has run.

## 6. Out of scope

WebSockets and any bidirectional transport. Resumability and event replay
(B-8). MCP session semantics, which are a product concern and sit on top of
this mechanism. Server push to a client that is not currently connected,
which is a notification concern and does not exist in this chassis.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Streaming is declared per route rather
  than detected from the handler's return type. Detection would apply the
  exemptions after the middleware chain had already been assembled for a
  buffered response, which is the bug this spec exists to prevent.

## Verification

```verify:cli
cargo test -p rahi-edge --locked --test stream
```
