# Error retry semantics

Flea error envelopes answer two independent questions:

- `upstream_transient` reports whether the observed upstream failure is likely temporary.
- `safe_to_retry` reports whether repeating the complete command is safe from duplicate or conflicting remote mutations.

A temporary upstream failure does not make a mutation safe to repeat. For example, a `502` response from `draft show` produces `upstream_transient: true` and `safe_to_retry: true`. The same response after a draft update request produces `upstream_transient: true` and `safe_to_retry: false` because the remote mutation outcome is uncertain.

Errors include `retry_guidance` when one or both classifications are true or a mutation outcome is uncertain. The guidance explains the distinction in human-readable output. When mutation state is uncertain, `next_actions` identifies an authoritative `draft show`, `listing show`, or listing command before any further mutation.

## Classification rules

Flea classifies request failures from the operation and the evidence available at the failure boundary:

- GET and HEAD operations are safe to repeat. Transport failures, HTTP 408, 425, 429, 500, 502, 503, and 504 are also classified as upstream-transient.
- Mutation transport failures and transient HTTP responses are upstream-transient, but they are unsafe to repeat without a source-backed idempotency contract, including a documented idempotency key.
- A malformed 2xx read response is safe to request again but is not assumed to be transient.
- A malformed 2xx mutation response has an uncertain outcome and is unsafe to repeat.
- A mutation protected by an ETag is safe to repeat after a precondition failure only when Flea has authoritatively observed the fresh remote state.
- A workflow with completed mutation steps is unsafe to replay from the beginning. Returned draft or listing identifiers are recovery handles, not permission to repeat creation or publication.
- Conflict and validation responses are not upstream-transient. Their safe retry value depends on authoritative evidence that the attempted mutation was not applied.

The HTTP retry loop consumes the same classification. It retries transient reads within its configured bound. A mutation enters that loop only when its request declares a source-backed idempotency contract or carries a source-backed idempotency key. Flea's Tori mutation adapters declare neither, so they execute once.

## Failure envelope

JSON errors use this shape:

```json
{
  "ok": false,
  "error": {
    "code": "mutation.uncertain",
    "message": "The upstream failure may be temporary, but the mutation outcome is unknown",
    "upstream_transient": true,
    "safe_to_retry": false,
    "retry_guidance": "The upstream failure appears temporary, but repeating this operation could duplicate a remote mutation. Inspect authoritative state first.",
    "details": {
      "status": 502
    }
  },
  "partial": {
    "draft_id": "36443414",
    "completed_steps": ["fetch_draft"],
    "upstream_transient": true,
    "safe_to_retry": false,
    "next_safe_actions": ["flea draft show 36443414"]
  },
  "next_actions": [
    { "command": "flea draft show 36443414" }
  ]
}
```

Partial draft recovery records and search explanation failures use the same `upstream_transient` and `safe_to_retry` field names.

## JSON consumer compatibility

`error.retryable` is not part of the error schema. JSON consumers read both `error.upstream_transient` and `error.safe_to_retry` and decide separately whether to wait for upstream recovery and whether to replay an operation. Consumers that require `retryable` are incompatible with this schema and must update their decoders.

The same compatibility rule applies to `partial.retryable` and `data.explain.failures[].retryable`. Their schema uses `upstream_transient` and `safe_to_retry`.
