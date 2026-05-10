use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::image::RemoveImageOptions;
use bollard::models::ImageInspect;
use tokio::fs;
use tracing::{debug, warn};

use crate::oci::{apple_container_command, exit_status_label};

const LAST_BUILT_IDS_PATH: &str = ".my-ci/last-built-image-ids.json";

async fn load_last_built_map() -> HashMap<String, String> {
    let path = Path::new(LAST_BUILT_IDS_PATH);
    let raw = match fs::read_to_string(path).await {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

async fn save_last_built_map(map: &HashMap<String, String>) -> Result<()> {
    if let Some(parent) = Path::new(LAST_BUILT_IDS_PATH).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(map).context("serialize last-built image map")?;
    fs::write(LAST_BUILT_IDS_PATH, json)
        .await
        .with_context(|| format!("failed to write {}", LAST_BUILT_IDS_PATH))?;
    Ok(())
}

/// Records the image id now referenced by `image_tag` under `.my-ci/last-built-image-ids.json`.
pub async fn record_built_image_id(image_tag: &str, id: &str) -> Result<()> {
    let mut map = load_last_built_map().await;
    map.insert(image_tag.to_string(), id.to_string());
    save_last_built_map(&map).await
}

fn docker_image_not_found(err: &BollardError) -> bool {
    match err {
        BollardError::DockerResponseServerError { status_code, .. } => *status_code == 404,
        _ => {
            let s = err.to_string();
            s.contains("404") || s.contains("No such image")
        }
    }
}

fn inspect_image_id(inspect: &ImageInspect) -> Option<String> {
    inspect
        .id
        .clone()
        .map(|id| id.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Returns the content-addressable image id for `image_tag`, if it exists locally.
pub async fn docker_inspect_image_id(docker: &Docker, image_tag: &str) -> Option<String> {
    docker
        .inspect_image(image_tag)
        .await
        .ok()
        .as_ref()
        .and_then(inspect_image_id)
}

/// Removes an image by id or digest, then walks `Deleted` entries from the engine response
/// the same way (iterative stack). Missing images are ignored.
pub async fn remove_docker_image_recursive_best_effort(docker: &Docker, root: &str) {
    let mut queue = VecDeque::new();
    queue.push_back(root.trim().to_string());
    let mut seen = HashSet::<String>::new();

    while let Some(id) = queue.pop_front() {
        if id.is_empty() {
            continue;
        }
        if !seen.insert(id.clone()) {
            continue;
        }

        match docker
            .remove_image(
                &id,
                Some(RemoveImageOptions {
                    force: true,
                    noprune: true,
                }),
                None,
            )
            .await
        {
            Ok(items) => {
                debug!(image = %id, removed = items.len(), "removed docker image layer(s)");
                for item in items {
                    if let Some(d) = item.deleted.as_ref().map(|s| s.trim().to_string()) {
                        if !d.is_empty() {
                            queue.push_back(d);
                        }
                    }
                }
            }
            Err(err) if docker_image_not_found(&err) => {
                debug!(image = %id, "docker image already absent");
            }
            Err(err) => {
                warn!(image = %id, error = %err, "docker remove_image failed");
            }
        }
    }
}

/// After a successful rebuild, removes the previous image id if the tag now points elsewhere.
pub async fn docker_supersede_prior_image(
    docker: &Docker,
    image_tag: &str,
    prior_id: Option<String>,
) {
    let Some(new_id) = docker_inspect_image_id(docker, image_tag).await else {
        warn!(%image_tag, "could not inspect image after build; skipping superseded cleanup");
        return;
    };

    if let Err(err) = record_built_image_id(image_tag, &new_id).await {
        warn!(error = %err, "failed to persist last-built image id record");
    }

    let Some(prev) = prior_id.filter(|p| !p.is_empty()) else {
        return;
    };
    if prev == new_id {
        return;
    }

    debug!(%image_tag, %prev, %new_id, "removing superseded workflow image chain");
    remove_docker_image_recursive_best_effort(docker, &prev).await;
}

fn apple_inspect_index_digest(json: &serde_json::Value) -> Option<String> {
    json.as_array()?
        .first()?
        .get("index")?
        .get("digest")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Returns the index digest for `image_tag`, if the image exists locally.
pub async fn apple_inspect_image_digest(image_tag: &str) -> Option<String> {
    let output = apple_container_command()
        .arg("image")
        .arg("inspect")
        .arg(image_tag)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    apple_inspect_index_digest(&v)
}

pub async fn remove_apple_image_best_effort(image_ref: &str) {
    let id = image_ref.trim();
    if id.is_empty() {
        return;
    }
    let status = match apple_container_command()
        .arg("image")
        .arg("delete")
        .arg(id)
        .status()
        .await
    {
        Ok(s) => s,
        Err(err) => {
            warn!(image = %id, error = %err, "failed to spawn container image delete");
            return;
        }
    };

    if status.success() {
        debug!(image = %id, "deleted apple container image");
        return;
    }
    warn!(
        image = %id,
        status = %exit_status_label(status),
        "apple container image delete failed"
    );
}

pub async fn apple_supersede_prior_image(image_tag: &str, prior_digest: Option<String>) {
    let Some(new_digest) = apple_inspect_image_digest(image_tag).await else {
        warn!(%image_tag, "could not inspect image after build; skipping superseded cleanup");
        return;
    };

    if let Err(err) = record_built_image_id(image_tag, &new_digest).await {
        warn!(error = %err, "failed to persist last-built image id record");
    }

    let Some(prev) = prior_digest.filter(|p| !p.is_empty()) else {
        return;
    };
    if prev == new_digest {
        return;
    }

    debug!(%image_tag, %prev, %new_digest, "removing superseded apple workflow image chain");
    remove_apple_image_best_effort(&prev).await;
}
