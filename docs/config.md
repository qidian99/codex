# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Lifecycle hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.

## Image file upload cache (local extension)

ChatGPT image file uploads are **enabled by default**. Each distinct image is
uploaded concurrently once, and subsequent model requests use a stable `file_id`.
The cache is scoped to the backend, account, and image content. All images, their
order, and detail settings remain available together to the model; local session
history retains the original images. This does not itself guarantee model prompt
cache hits or reduce image input tokens.

To disable uploads and send inline images as before, add to your Codex
`config.toml` (normally `$CODEX_HOME/config.toml` or `~/.codex/config.toml`):

```toml
[features]
image_file_uploads = false
```

Set it to `true`, or remove the entry, to enable it. Start a new session or resume
the session after changing the setting. A one-launch override is also supported:

```sh
codex -c features.image_file_uploads=false
```

File IDs are stored under `$CODEX_HOME/image-file-cache/`. Disabling the feature
does not delete this cache or alter conversation history. Removing the cache
directory forces fresh uploads in subsequently started sessions. API-key and
independent provider requests continue using inline images.
