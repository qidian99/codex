# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

## Conversation checkpoints

`/cp` (also `/listcp`) opens the checkpoint manager: select a checkpoint to continue,
freeze, replace, or delete it, or inspect saved feedback replies. `/cp NAME` saves
the model's current conversation context. `/usecp NAME`
continues from that snapshot while keeping earlier messages visible in the TUI.
`/usecp` without a name opens the manager. Saving the same name replaces that snapshot.
`/usecp NAME question` restores the checkpoint and then sends the question;
`/usecp NAME! question` freezes first and then sends it. If restoration fails, the
question stays in the composer and no model request is sent. Checkpoint errors,
including resume-time status errors, are displayed in chat without exiting the TUI.
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

Revert conversation does not remove named checkpoints, even when reverting to before
the checkpoint was created. Delete them explicitly with `/rmcp` or `/rmallcp`.
The checkpoint manager works entirely inside Codex; codexctl is optional for
offline inspection, export, or copying/linking sessions.

Snapshots and feedback branches survive resume in a versioned
`*.checkpoints.json` sibling of the session rollout. Inspect them using
`codexctl checkpoint SESSION list`, `show NAME`, `branch ID`, or `export`.
The `thread/checkpoint` RPC accepts `save`, `use`, `remove`, `clear`, `freeze`,
`unfreeze`, `status`, `list`, `read_branch`, and `use_branch` actions with a
`threadId` and optional `name` (a branch ID for branch actions).
