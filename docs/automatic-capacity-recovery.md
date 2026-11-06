# Automatic capacity recovery

Enable `[features] auto_continue_on_rate_limit = true` in each Codex home to keep
work running through temporary provider capacity failures. This is an opt-in
runtime policy shared by the CLI and app server.

Sampling and remote compaction retry temporary overloads and HTTP 429 responses,
including failures without `Retry-After`. Retries start around five seconds,
double with jitter, and cap at one minute. A longer server deadline is respected.
Capacity retries have their own counter and can outlive the transport retry
budget. Notifications expose the wait and attempt count. The original request,
model selection, and conversation history are retained; interrupting the turn
cancels its pending retry.

If another failure path has already stopped a goal because of capacity, the goal
runtime waits five seconds and applies the same active-state transition as a goal
resume. It only resumes the exact stopped goal snapshot. Editing, pausing,
completing, replacing, or unloading that goal prevents the delayed recovery.
Billing and usage limits, policy denials, budget exhaustion, invalid requests,
and explicit interruptions do not trigger capacity recovery.

Set the feature to `false` to retain the upstream bounded retry policy.

File creation and upload confirmation also recover automatically from temporary
connection failures, interrupted response bodies, HTTP 408/429, and transient
server errors. These requests retry their current stage with a five-second
initial backoff, jitter, and a one-minute cap, respecting longer server advice.
Confirmation retries retain the existing file ID and do not upload the bytes
again. Creation retries retain their JSON body and idempotency key. Cancelling
the calling operation cancels its request and retry wait. Authentication,
quota, network-policy and invalid-request errors remain terminal. File control
request recovery is independent of the Responses stream retry budget.
