//! Checkpoint controls use the ordinary picker and report recoverable RPC errors in chat.
use super::*;
use codex_app_server_protocol::ThreadCheckpointResponse;

fn command_item(
    label: &str,
    description: &str,
    action: &str,
    name: Option<String>,
) -> SelectionItem {
    let action = action.to_string();
    SelectionItem {
        name: label.to_string(),
        description: Some(description.to_string()),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::CodexOp(AppCommand::Checkpoint {
                action: action.clone(),
                name: name.clone(),
                prompt: None,
            }));
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

impl ChatWidget {
    pub(crate) fn handle_checkpoint_result(
        &mut self,
        action: &str,
        prompt: Option<&str>,
        result: color_eyre::Result<ThreadCheckpointResponse>,
    ) {
        match result {
            Ok(response) => {
                self.set_frozen_checkpoint(response.frozen);
                if action == "list" {
                    self.show_checkpoint_picker(&response.data);
                } else if action == "read_branch" {
                    self.show_checkpoint_branch(&response.data);
                } else if action != "status" {
                    self.add_info_message(response.message, None);
                }
                if let Some(prompt) = prompt {
                    self.submit_user_message_with_shell_escape_policy(
                        prompt.into(),
                        ShellEscapePolicy::Disallow,
                    );
                }
            }
            Err(err) => {
                self.add_error_message(format!("Checkpoint {action} failed: {err:#}"));
                if let Some(prompt) = prompt {
                    self.restore_user_message_to_composer(prompt.into());
                }
            }
        }
    }

    pub(crate) fn show_checkpoint_picker(&mut self, data: &serde_json::Value) {
        let mut items = Vec::new();
        if self.frozen_checkpoint.is_some() {
            items.push(command_item(
                "Return to main conversation",
                "Leave frozen checkpoint mode",
                "unfreeze",
                None,
            ));
        }
        if let Some(checkpoints) = data["checkpoints"].as_object() {
            for name in checkpoints.keys() {
                items.push(command_item(
                    name,
                    "Continue, freeze, replace, or delete",
                    "menu",
                    Some(name.clone()),
                ));
            }
        }
        items.push(command_item(
            "Save current context as 'default'",
            "For another name, type /cp NAME",
            "save",
            Some("default".to_string()),
        ));
        if let Some(branches) = data["branches"].as_object() {
            for (id, branch) in branches {
                let feedback = branch["feedback"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    })
                    .unwrap_or_default();
                items.push(command_item(
                    &format!("Feedback: {feedback}"),
                    &format!(
                        "From {} · {} · view saved reply",
                        branch["checkpoint_name"].as_str().unwrap_or_default(),
                        branch["status"].as_str().unwrap_or_default()
                    ),
                    "read_branch",
                    Some(id.clone()),
                ));
            }
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Conversation checkpoints".to_string()),
            subtitle: Some(
                "Select a checkpoint to manage it. /cp NAME saves the current context.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn show_checkpoint_actions(&mut self, name: &str) {
        let items = [
            (
                "Continue from this checkpoint",
                "Keep displayed messages",
                "use",
            ),
            (
                "Freeze here for independent feedback",
                "Each new question starts here; Ctrl+C when idle exits",
                "freeze",
            ),
            (
                "Replace with current context",
                "Overwrite this named checkpoint",
                "save",
            ),
            (
                "Delete this checkpoint",
                "Saved feedback branches remain available",
                "remove",
            ),
        ]
        .into_iter()
        .map(|(label, description, action)| {
            command_item(label, description, action, Some(name.to_string()))
        })
        .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("Checkpoint: {name}")),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.request_redraw();
    }

    fn show_checkpoint_branch(&mut self, data: &serde_json::Value) {
        let feedback = data["feedback"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let mut messages = Vec::new();
        if let Some(items) = data["items"].as_array() {
            for item in items {
                if item["type"] == "compacted" {
                    if let Some(history) = item["payload"]["replacement_history"].as_array() {
                        messages.extend(history.iter());
                    }
                } else if item["type"] == "response_item" {
                    messages.push(&item["payload"]);
                }
            }
        }
        // Do not mistake a baseline answer for a reply to an unfinished feedback turn.
        let feedback_start = messages.iter().rposition(|message| {
            message["role"] == "user"
                && message["content"].as_array().is_some_and(|content| {
                    content.iter().any(|part| {
                        part["text"].as_str().is_some_and(|text| {
                            data["feedback"].as_array().is_some_and(|feedback| {
                                feedback.iter().any(|f| f.as_str() == Some(text))
                            })
                        })
                    })
                })
        });
        let reply = feedback_start
            .map(|start| {
                messages[start + 1..]
                    .iter()
                    .filter(|message| message["role"] == "assistant")
                    .filter_map(|message| message["content"].as_array())
                    .flatten()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .filter(|reply| !reply.is_empty())
            .unwrap_or_else(|| "No saved assistant reply yet.".to_string());
        self.add_info_message(
            format!(
                "Feedback from checkpoint '{}':\n{feedback}\n\nSaved reply:\n{reply}",
                data["checkpoint_name"].as_str().unwrap_or_default()
            ),
            None,
        );
    }
}
