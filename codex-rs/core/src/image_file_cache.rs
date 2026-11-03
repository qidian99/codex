//! Outbound-only image references; canonical history keeps the original bytes.
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use base64::Engine;
use codex_api::AuthProvider;
use codex_http_client::RouteAwareClientPool;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseItem;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::Mutex;

type CachedId = Arc<Mutex<Option<String>>>;

#[derive(Debug)]
pub(crate) struct ImageFileCache {
    root: PathBuf,
    ids: Mutex<HashMap<String, CachedId>>,
}

impl ImageFileCache {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            ids: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn materialize(
        &self,
        base_url: &str,
        account: &str,
        auth: &dyn AuthProvider,
        pool: &RouteAwareClientPool,
        items: &mut [ResponseItem],
    ) -> io::Result<()> {
        let base_url = base_url.trim_end_matches('/');
        let scope = serde_json::to_string(&(base_url, account))?;
        self.materialize_with(&scope, items, |bytes, name| async move {
            let bytes = bytes::Bytes::from(bytes);
            codex_api::upload_openai_file(
                base_url,
                auth,
                pool,
                name,
                bytes.len() as u64,
                || {
                    let bytes = bytes.clone();
                    async move { Ok(futures::stream::once(async move { Ok(bytes) })) }
                },
                None,
            )
            .await
            .map(|uploaded| uploaded.file_id)
            .map_err(io::Error::other)
        })
        .await
    }

    async fn materialize_with<F, Fut>(
        &self,
        scope: &str,
        items: &mut [ResponseItem],
        upload: F,
    ) -> io::Result<()>
    where
        F: Fn(Vec<u8>, String) -> Fut + Sync,
        Fut: Future<Output = io::Result<String>> + Send,
    {
        let mut references = image_references(items);
        let mut replacements = Vec::new();
        let mut unique = HashMap::new();
        for (index, image) in references.iter().enumerate() {
            let ImageReference::Inline { image_url } = image else {
                continue;
            };
            let Some((header, data)) = image_url.split_once(',') else {
                continue;
            };
            if !header.starts_with("data:image/") || !header.ends_with(";base64") {
                continue;
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let mut digest = Sha256::new();
            digest.update(scope.as_bytes());
            digest.update([0]);
            digest.update(&bytes);
            let key = format!("{:x}", digest.finalize());
            let extension = match header.split(';').next() {
                Some("data:image/jpeg") => "jpg",
                Some("data:image/webp") => "webp",
                Some("data:image/gif") => "gif",
                _ => "png",
            };
            unique.entry(key.clone()).or_insert((bytes, extension));
            replacements.push((index, key));
        }
        if unique.is_empty() {
            return Ok(());
        }
        tokio::fs::create_dir_all(&self.root).await?;
        let started = std::time::Instant::now();
        let uploaded = AtomicUsize::new(0);
        let count = unique.len();
        // Poll every distinct upload concurrently; duplicate references share one result.
        // join_all lets successful siblings finish and persist even if one upload fails.
        let results =
            futures::future::join_all(unique.into_iter().map(|(key, (bytes, extension))| {
                let upload = &upload;
                let uploaded = &uploaded;
                async move {
                    let cell = self
                        .ids
                        .lock()
                        .await
                        .entry(key.clone())
                        .or_default()
                        .clone();
                    let mut id = cell.lock().await;
                    if id.is_none() {
                        let path = self.root.join(format!("{key}.json"));
                        *id = match tokio::fs::read(&path).await {
                            Ok(data) => Some(serde_json::from_slice::<String>(&data)?),
                            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                            Err(error) => return Err(error),
                        };
                        if id.is_none() {
                            let file_id = upload(bytes, format!("{key}.{extension}")).await?;
                            uploaded.fetch_add(1, Ordering::Relaxed);
                            // No-clobber publication makes concurrent processes converge on the
                            // same stable ID, even if both initially missed the on-disk cache.
                            let root = self.root.clone();
                            let stable_id =
                                tokio::task::spawn_blocking(move || -> io::Result<String> {
                                    let mut temp = tempfile::NamedTempFile::new_in(root)?;
                                    temp.write_all(&serde_json::to_vec(&file_id)?)?;
                                    match temp.persist_noclobber(&path) {
                                        Ok(_) => Ok(file_id),
                                        Err(error)
                                            if error.error.kind()
                                                == io::ErrorKind::AlreadyExists =>
                                        {
                                            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
                                        }
                                        Err(error) => Err(error.error),
                                    }
                                })
                                .await
                                .map_err(io::Error::other)??;
                            *id = Some(stable_id);
                        }
                    }
                    Ok((key, id.as_ref().expect("resolved image file ID").clone()))
                }
            }))
            .await
            .into_iter()
            .collect::<io::Result<HashMap<_, _>>>()?;
        for (index, key) in replacements {
            *references[index] = ImageReference::File {
                file_id: results[&key].clone(),
            };
        }
        tracing::info!(
            images = references.len(),
            unique_images = count,
            uploaded_images = uploaded.load(Ordering::Relaxed),
            elapsed_ms = started.elapsed().as_millis(),
            "materialized image file references"
        );
        Ok(())
    }
}

fn image_references(items: &mut [ResponseItem]) -> Vec<&mut ImageReference> {
    let mut images = Vec::new();
    for item in items {
        match item {
            ResponseItem::Message { content, .. } => {
                for item in content {
                    if let ContentItem::InputImage { image, .. } = item {
                        images.push(image);
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                if let Some(content) = output.content_items_mut() {
                    for item in content {
                        if let FunctionCallOutputContentItem::InputImage { image, .. } = item {
                            images.push(image);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use tokio::sync::Barrier;

    fn items() -> Vec<ResponseItem> {
        serde_json::from_value(json!([
            {"type":"message","role":"user","content":[
                {"type":"input_text","text":"Compare all frames"},
                {"type":"input_image","image_url":"data:image/png;base64,YQ==","detail":"high"}]},
            {"type":"custom_tool_call_output","call_id":"frames","output":[
                {"type":"input_image","image_url":"data:image/png;base64,Yg==","detail":"original"},
                {"type":"input_image","image_url":"data:image/png;base64,YQ==","detail":"high"}]}
        ]))
        .unwrap()
    }

    // Catches serialized uploads, duplicate uploads, dropped images, and missing disk reuse.
    #[tokio::test]
    async fn concurrent_uploads_preserve_images_and_reuse_persistent_ids() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageFileCache::new(dir.path().to_owned());
        let original = items();
        let mut request = original.clone();
        let barrier = Barrier::new(2);
        let calls = AtomicUsize::new(0);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            cache.materialize_with("backend/account", &mut request, |bytes, _| {
                let barrier = &barrier;
                let calls = &calls;
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    barrier.wait().await;
                    Ok(format!("file-{}", String::from_utf8(bytes).unwrap()))
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value[0]["content"][1]["file_id"], "file-a");
        assert_eq!(value[1]["output"][0]["file_id"], "file-b");
        assert_eq!(value[1]["output"][1]["file_id"], "file-a");
        assert_eq!(value[1]["output"][0]["detail"], "original");
        assert_eq!(value[0]["content"][0]["text"], "Compare all frames");
        let cache = ImageFileCache::new(dir.path().to_owned());
        let mut replay = original.clone();
        cache
            .materialize_with("backend/account", &mut replay, |_, _| async {
                panic!("cached image uploaded again")
            })
            .await
            .unwrap();
        assert_eq!(request, replay);
        assert_eq!(original, items());
        let mut other_account = original;
        cache
            .materialize_with("backend/other", &mut other_account, |_, _| async {
                Ok("file-other-account".to_owned())
            })
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(other_account).unwrap()[0]["content"][1]["file_id"],
            "file-other-account"
        );
    }
}
