//! Named model-context snapshots and frozen feedback branches. UI history never rewinds.
use super::*;
use anyhow::Context;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
struct CheckpointStore {
    version: u32,
    #[serde(default)]
    checkpoints: BTreeMap<String, Checkpoint>,
    #[serde(default)]
    branches: BTreeMap<String, Branch>,
    #[serde(default)]
    frozen: Option<Frozen>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
impl Default for CheckpointStore {
    fn default() -> Self {
        Self {
            version: 1,
            checkpoints: BTreeMap::new(),
            branches: BTreeMap::new(),
            frozen: None,
            extra: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Checkpoint {
    id: String,
    name: String,
    created_at: u64,
    items: Vec<RolloutItem>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Serialize, Deserialize)]
struct Branch {
    id: String,
    checkpoint_id: String,
    checkpoint_name: String,
    feedback: Vec<String>,
    created_at: u64,
    status: String,
    #[serde(default)]
    turn_id: String,
    items: Vec<RolloutItem>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Serialize, Deserialize)]
struct Frozen {
    checkpoint: Checkpoint,
    main_items: Vec<RolloutItem>,
    branch_ids: Vec<String>,
    active_branch_id: Option<String>,
}

struct CheckpointAbortGuard<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for CheckpointAbortGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Session {
    /// Covers the gap between removing active_turn and joining its cancelled task.
    pub(crate) fn checkpoint_abort_guard(&self) -> impl Drop + '_ {
        self.checkpoint_abort_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        CheckpointAbortGuard(&self.checkpoint_abort_count)
    }
    pub(crate) async fn checkpoint(
        self: &Arc<Self>,
        action: &str,
        name: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let name = checkpoint_name(name)?;
        if action == "status" {
            let store = match self.checkpoint_path().await? {
                Some(path) => read_store(&path).await?,
                None => CheckpointStore::default(),
            };
            return Ok(
                serde_json::json!({"message":"Checkpoint status.", "data":null,
                "frozen": store.frozen.as_ref().map(|f| &f.checkpoint.name)}),
            );
        }
        // Commands are idle-only; this also serializes sidecar writers against turn admission.
        let _transaction = self.checkpoint_transaction.lock().await;
        let active_turn = self.active_turn.lock().await;
        anyhow::ensure!(
            active_turn.is_none()
                && self
                    .checkpoint_abort_count
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == 0,
            "Wait for the current turn to finish before managing checkpoints."
        );
        let path = self
            .checkpoint_path()
            .await?
            .context("Checkpoints require a persistent session.")?;
        self.checkpoint_at_path(action, &name, &path).await
    }

    async fn checkpoint_at_path(
        &self,
        action: &str,
        name: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<serde_json::Value> {
        let name = name.to_string();
        let mut store = read_store(&path).await?;
        self.finish_checkpoint_branch(&mut store, "interrupted")
            .await;
        let mut data = serde_json::Value::Null;
        let message = match action {
            "save" => {
                store.checkpoints.insert(
                    name.clone(),
                    Checkpoint {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: name.clone(),
                        created_at: now(),
                        items: self.conversation_checkpoint_items().await,
                        extra: BTreeMap::new(),
                    },
                );
                format!("Saved checkpoint '{name}'.")
            }
            "use" | "use_branch" => {
                anyhow::ensure!(
                    store.frozen.is_none(),
                    "Exit frozen checkpoint mode before switching context."
                );
                let items = if action == "use" {
                    &store
                        .checkpoints
                        .get(&name)
                        .with_context(|| format!("Checkpoint '{name}' does not exist."))?
                        .items
                } else {
                    &store
                        .branches
                        .get(&name)
                        .with_context(|| format!("Branch '{name}' does not exist."))?
                        .items
                };
                self.install_checkpoint(items).await?;
                format!("Using '{name}'. Displayed messages are preserved.")
            }
            "freeze" => {
                anyhow::ensure!(
                    store.frozen.is_none(),
                    "Already in frozen checkpoint mode; exit it first."
                );
                let checkpoint = store
                    .checkpoints
                    .get(&name)
                    .with_context(|| format!("Checkpoint '{name}' does not exist."))?
                    .clone();
                store.frozen = Some(Frozen {
                    checkpoint,
                    main_items: self.conversation_checkpoint_items().await,
                    branch_ids: Vec::new(),
                    active_branch_id: None,
                });
                format!(
                    "Frozen checkpoint '{name}': each new feedback starts here. Ctrl+C when idle returns to the main conversation."
                )
            }
            "unfreeze" => {
                let frozen = store
                    .frozen
                    .as_ref()
                    .context("No frozen checkpoint is active.")?;
                let mut items = frozen.main_items.clone();
                let mut summary = format!(
                    "Returned from frozen checkpoint '{}'. Feedback branches are stored in {}. Inspect/export with codexctl checkpoint SESSION; use thread/checkpoint read_branch or use_branch with the branch ID.\n",
                    frozen.checkpoint.name,
                    path.display()
                );
                for id in &frozen.branch_ids {
                    if let Some(branch) = store.branches.get(id) {
                        summary.push_str(&format!(
                            "- Branch {} ({}): {}\n",
                            id,
                            branch.status,
                            branch.feedback.join(" / ")
                        ));
                    }
                }
                let supplement = ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![codex_protocol::models::ContentItem::InputText {
                        text: summary.clone(),
                    }],
                    encrypted_content: None,
                    status: None,
                    phase: None,
                    internal_chat_message_metadata_passthrough: None,
                };
                items.push(RolloutItem::ResponseItem(ResponseItemEnvelope::new(
                    supplement,
                )));
                self.install_checkpoint(&items).await?;
                store.frozen = None;
                summary
            }
            "remove" => {
                anyhow::ensure!(
                    store.checkpoints.remove(&name).is_some(),
                    "Checkpoint '{name}' does not exist."
                );
                format!("Deleted checkpoint '{name}'.")
            }
            "clear" => {
                store.checkpoints.clear();
                "Deleted all named checkpoints for this session. Feedback branches are retained."
                    .to_string()
            }
            "status" => "Checkpoint status.".to_string(),
            "list" => {
                data = serde_json::to_value(&store)?;
                "Conversation checkpoints and branches.".to_string()
            }
            "read_branch" => {
                data = serde_json::to_value(
                    store
                        .branches
                        .get(&name)
                        .with_context(|| format!("Branch '{name}' does not exist."))?,
                )?;
                format!("Feedback branch '{name}'.")
            }
            _ => anyhow::bail!("Unknown checkpoint action: {action}"),
        };
        if !matches!(action, "status" | "list" | "read_branch") {
            write_store(&path, &store).await?;
        }
        Ok(
            serde_json::json!({"message": message, "data": data, "frozen": store.frozen.as_ref().map(|f| &f.checkpoint.name)}),
        )
    }

    async fn checkpoint_path(&self) -> anyhow::Result<Option<PathBuf>> {
        Ok(self.current_rollout_path().await?.map(|path| {
            codex_rollout::plain_rollout_path(&path).with_extension("checkpoints.json")
        }))
    }

    async fn install_checkpoint(&self, items: &[RolloutItem]) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.persist_rollout_items(items).await,
            "Could not persist checkpoint restoration"
        );
        self.flush_rollout().await?;
        for item in items {
            if let RolloutItem::Compacted(compacted) = item {
                let mut state = self.state.lock().await;
                state.latest_token_usage_record = compacted.latest_token_usage_record.clone();
                if let Some(tools) = compacted
                    .resume_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.dynamic_tools.as_ref())
                {
                    state.persisted_dynamic_tools = tools.tools.clone().into();
                    state.session_configuration.dynamic_tools =
                        Arc::clone(&state.persisted_dynamic_tools);
                }
            }
        }
        let turn_context = self.new_default_turn().await;
        self.apply_rollout_reconstruction(&turn_context, items)
            .await;
        for item in items {
            if let RolloutItem::Compacted(compacted) = item
                && let Some(checkpoint) = &compacted.mcp_resource_origins
            {
                self.services
                    .mcp_runtime
                    .restore_resource_origin_checkpoint(checkpoint);
            }
        }
        let mut state = self.state.lock().await;
        state.async_compaction.discard_pending_result();
        state.set_token_info(None);
        Ok(())
    }

    /// Runs once per regular user turn, before recording input. Tool loops and steers
    /// inside that turn retain their normal accumulated history.
    pub(crate) async fn begin_checkpoint_feedback(
        &self,
        turn_id: &str,
        input: &[TurnInput],
    ) -> anyhow::Result<()> {
        let _transaction = self.checkpoint_transaction.lock().await;
        let feedback: Vec<String> = input
            .iter()
            .filter_map(|input| match input {
                TurnInput::UserInput { content, .. } => Some(
                    content
                        .iter()
                        .filter_map(|item| match item {
                            codex_protocol::user_input::UserInput::Text { text, .. } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect();
        if feedback.is_empty() {
            return Ok(());
        }
        let Some(path) = self.checkpoint_path().await? else {
            return Ok(());
        };
        self.begin_checkpoint_feedback_at_path(turn_id, feedback, &path)
            .await
    }

    async fn begin_checkpoint_feedback_at_path(
        &self,
        turn_id: &str,
        feedback: Vec<String>,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let mut store = read_store(path).await?;
        if store.frozen.is_none() {
            return Ok(());
        }
        self.finish_checkpoint_branch(&mut store, "interrupted")
            .await;
        let frozen = store.frozen.as_mut().expect("checked above");
        let (started_turn, attribution) = {
            let state = self.state.lock().await;
            (
                state.last_started_turn_id.clone(),
                state.turn_attribution.clone(),
            )
        };
        self.install_checkpoint(&frozen.checkpoint.items).await?;
        {
            let mut state = self.state.lock().await;
            state.last_started_turn_id = started_turn;
            state.turn_attribution = attribution;
        }
        let id = uuid::Uuid::new_v4().to_string();
        store.branches.insert(
            id.clone(),
            Branch {
                id: id.clone(),
                checkpoint_id: frozen.checkpoint.id.clone(),
                checkpoint_name: frozen.checkpoint.name.clone(),
                feedback,
                created_at: now(),
                status: "running".to_string(),
                turn_id: turn_id.to_string(),
                items: frozen.checkpoint.items.clone(),
                extra: BTreeMap::new(),
            },
        );
        frozen.branch_ids.push(id.clone());
        frozen.active_branch_id = Some(id);
        write_store(&path, &store).await
    }

    async fn finish_checkpoint_branch(&self, store: &mut CheckpointStore, status: &str) {
        if let Some(id) = store
            .frozen
            .as_mut()
            .and_then(|f| f.active_branch_id.take())
            && let Some(branch) = store.branches.get_mut(&id)
        {
            branch.items = self.conversation_checkpoint_items().await;
            branch.status = status.to_string();
        }
    }

    pub(crate) async fn finish_checkpoint_feedback(
        &self,
        turn_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let _transaction = self.checkpoint_transaction.lock().await;
        self.finish_checkpoint_feedback_locked(turn_id, status)
            .await
    }

    /// Caller holds checkpoint_transaction across turn finalization and pending-input recording.
    pub(crate) async fn finish_checkpoint_feedback_locked(
        &self,
        turn_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let Some(path) = self.checkpoint_path().await? else {
            return Ok(());
        };
        let mut store = read_store(&path).await?;
        self.finish_checkpoint_feedback_at_path(&mut store, turn_id, status)
            .await;
        if store.frozen.is_some() {
            write_store(&path, &store).await?;
        }
        Ok(())
    }

    async fn finish_checkpoint_feedback_at_path(
        &self,
        store: &mut CheckpointStore,
        turn_id: &str,
        status: &str,
    ) {
        let matches_turn = store
            .frozen
            .as_ref()
            .and_then(|f| f.active_branch_id.as_ref())
            .and_then(|id| store.branches.get(id))
            .is_some_and(|branch| branch.turn_id == turn_id);
        if matches_turn {
            self.finish_checkpoint_branch(store, status).await;
        }
    }

    async fn conversation_checkpoint_items(&self) -> Vec<RolloutItem> {
        let state = self.state.lock().await;
        let ids = state.auto_compact_window_ids();
        let compacted = CompactedItem {
            message: String::new(),
            replacement_history: Some(state.history.annotated_items().to_vec()),
            guardian_history: state.history.guardian_history_checkpoint(),
            retained_context: Some(state.history.retained_context().clone()),
            mcp_resource_origins: self.services.mcp_runtime.resource_origin_checkpoint(),
            window_number: Some(state.auto_compact_window_number()),
            first_window_id: Some(ids.first_window_id.to_string()),
            previous_window_id: ids.previous_window_id.as_ref().map(|id| id.to_string()),
            window_id: Some(ids.window_id.to_string()),
            compaction_response_id: None,
            latest_token_usage_record: state.latest_token_usage_record.clone(),
            resume_metadata: Some(CompactionResumeMetadata {
                rejected_audio_file_ids: state.history.rejected_audio_file_ids(),
                dynamic_tools: Some(Box::new(DynamicToolsSnapshot {
                    thread_id: self.thread_id(),
                    tools: state.persisted_dynamic_tools.to_vec(),
                })),
                multi_agent_version: self.multi_agent_version(),
                last_started_turn_id: state.last_started_turn_id.clone(),
                turn_attribution: state.turn_attribution.clone(),
                previous_turn_settings: state.previous_turn_settings(),
            }),
        };
        let mut items = vec![RolloutItem::Compacted(Box::new(compacted))];
        if let Some(world_state) = state.history.world_state_checkpoint() {
            items.push(RolloutItem::WorldState(world_state));
        }
        if let Some(context) = state.reference_context_item() {
            items.push(RolloutItem::TurnContext(context));
        }
        items
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
async fn read_store(path: &std::path::Path) -> anyhow::Result<CheckpointStore> {
    let store: CheckpointStore = match tokio::fs::read(path).await {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).context("Cannot read conversation checkpoints")?
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => CheckpointStore::default(),
        Err(err) => return Err(err.into()),
    };
    anyhow::ensure!(
        store.version == 1,
        "Unsupported checkpoint format version {}",
        store.version
    );
    Ok(store)
}
async fn write_store(path: &std::path::Path, store: &CheckpointStore) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).await?;
    tokio::io::AsyncWriteExt::write_all(&mut file, &serde_json::to_vec(store)?).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&temp, path).await?;
    Ok(())
}

fn checkpoint_name(name: Option<&str>) -> anyhow::Result<String> {
    let name = name.unwrap_or("default").trim();
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control),
        "Checkpoint names must contain 1–128 bytes and no control characters."
    );
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn checkpoint_round_trip_preserves_context_and_discards_later_messages() {
        use crate::context::world_state::WorldStateSnapshot;
        use crate::session::tests::make_session_and_context;
        use codex_protocol::models::ResponseItem;
        use pretty_assertions::assert_eq;
        use serde_json::json;

        let (session, turn) = make_session_and_context().await;
        let turn = Arc::new(turn);
        let instruction: ResponseItem = serde_json::from_value(json!({
            "type": "message", "id": "saved-user", "role": "user", "content": [
                {"type": "input_text", "text": "Remember the private constraint."}
            ],
            "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["unknown"]}
        }))
        .unwrap();
        session
            .replace_history(vec![instruction], Some(turn.to_turn_context_item()))
            .await;
        let baseline = json!({"test": "baseline"});
        {
            let mut state = session.state.lock().await;
            state
                .history
                .set_world_state_baseline(WorldStateSnapshot::from(baseline.as_object().unwrap()));
            state
                .history
                .extend_rejected_audio_file_ids(["file-rejected".to_string()]);
        }
        let expected = session.clone_history().await;
        let snapshot = session.conversation_checkpoint_items().await;
        // Exercise the on-disk format, not only a clone of the in-memory snapshot.
        let snapshot: Vec<RolloutItem> =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let later: ResponseItem = serde_json::from_value(json!({
            "type": "message", "id": "later-user", "role": "user", "content": [
                {"type": "input_text", "text": "This later branch must disappear from model context."}
            ]
        })).unwrap();
        session.replace_history(vec![later], None).await;
        session
            .state
            .lock()
            .await
            .history
            .restore_rejected_audio_file_ids(Default::default());
        session.apply_rollout_reconstruction(&turn, &snapshot).await;
        let restored = session.clone_history().await;
        assert_eq!(restored.annotated_items(), expected.annotated_items());
        assert_eq!(restored.retained_context(), expected.retained_context());
        assert_eq!(
            restored.world_state_checkpoint(),
            expected.world_state_checkpoint()
        );
        assert_eq!(
            restored.rejected_audio_file_ids(),
            expected.rejected_audio_file_ids()
        );
        assert_eq!(
            serde_json::to_value(restored.reference_context_item()).unwrap(),
            serde_json::to_value(expected.reference_context_item()).unwrap()
        );
        assert!(
            snapshot
                .iter()
                .all(|item| !matches!(item, RolloutItem::EventMsg(_)))
        );
    }

    #[tokio::test]
    async fn frozen_feedback_resets_each_turn_and_exit_restores_main_with_branch_links() {
        use crate::session::tests::make_session_and_context;
        use serde_json::json;
        let (session, _) = make_session_and_context().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.checkpoints.json");
        let message = |text: &str| {
            serde_json::from_value::<ResponseItem>(json!({
                "type": "message", "role": "user", "content": [{"type": "input_text", "text": text}],
                "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["unknown"]}
            }))
            .unwrap()
        };
        let base = message("checkpoint baseline");
        session.replace_history(vec![base.clone()], None).await;
        session
            .checkpoint_at_path("save", "base", &path)
            .await
            .unwrap();
        let main = message("main conversation after checkpoint");
        session.replace_history(vec![main.clone()], None).await;
        session
            .checkpoint_at_path("freeze", "base", &path)
            .await
            .unwrap();
        for feedback in ["first question", "second question"] {
            session
                .begin_checkpoint_feedback_at_path(feedback, vec![feedback.to_string()], &path)
                .await
                .unwrap();
            assert_eq!(
                session.clone_history().await.annotated_items()[0].item,
                base
            );
            session
                .replace_history(vec![base.clone(), message(feedback)], None)
                .await;
            let mut store = read_store(&path).await.unwrap();
            session
                .finish_checkpoint_feedback_at_path(&mut store, "stale-turn", "completed")
                .await;
            let active_id = store
                .frozen
                .as_ref()
                .unwrap()
                .active_branch_id
                .as_ref()
                .unwrap();
            assert_eq!(store.branches[active_id].status, "running");
            session
                .finish_checkpoint_feedback_at_path(&mut store, feedback, "completed")
                .await;
            write_store(&path, &store).await.unwrap();
        }
        let result = session
            .checkpoint_at_path("unfreeze", "default", &path)
            .await
            .unwrap();
        assert!(result["frozen"].is_null());
        let restored = session.clone_history().await;
        assert_eq!(restored.annotated_items()[0].item, main);
        let text = serde_json::to_string(&restored.raw_items().collect::<Vec<_>>()).unwrap();
        assert!(text.contains("first question"));
        assert!(text.contains("second question"));
        let store = read_store(&path).await.unwrap();
        assert_eq!(store.branches.len(), 2);
        assert!(store.frozen.is_none());
        for branch in store.branches.values() {
            assert!(text.contains(&branch.id));
            assert_eq!(branch.status, "completed");
        }
    }

    #[test]
    fn checkpoint_names_are_keys_not_paths() {
        assert_eq!(checkpoint_name(None).unwrap(), "default");
        assert_eq!(
            checkpoint_name(Some("  before refactor  ")).unwrap(),
            "before refactor"
        );
        assert_eq!(checkpoint_name(Some("../outside")).unwrap(), "../outside");
        assert!(checkpoint_name(Some("\n")).is_err());
        assert!(checkpoint_name(Some(&"x".repeat(129))).is_err());
    }
}
