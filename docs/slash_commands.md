# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

## Conversation checkpoints

`/cp [name]` saves the model's current conversation context. `/usecp [name]`
continues from that snapshot while keeping earlier messages visible in the TUI.
Omitting a name uses `default`; saving the same name replaces that named snapshot.
`/rmcp [name]` deletes a named snapshot; `/rmallcp` deletes all named snapshots in
this session. These commands require an idle, persistent session and do not save
or restore workspace files.

`/usecp name!` pins a checkpoint for independent feedback. Each new user turn
starts from that snapshot; tool calls within the turn retain their normal history.
Feedback entered while a turn is running is queued for its own turn. Press Ctrl+C
when idle to leave this mode (during a running turn, Ctrl+C interrupts as usual).
The original main context is restored with a list of feedback and durable branch
IDs. Earlier displayed messages remain visible. Branches remain available after
named checkpoints are removed.

Snapshots and feedback branches survive resume in a versioned
`*.checkpoints.json` sibling of the session rollout. Inspect them using
`codexctl checkpoint SESSION list`, `show NAME`, `branch ID`, or `export`.
The `thread/checkpoint` RPC accepts `save`, `use`, `remove`, `clear`, `freeze`,
`unfreeze`, `status`, `list`, `read_branch`, and `use_branch` actions with a
`threadId` and optional `name` (a branch ID for branch actions).
