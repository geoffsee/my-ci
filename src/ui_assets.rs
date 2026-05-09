use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use tracing::{debug, warn};

const ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ui-dist.tar.gz"));

static ASSETS: OnceLock<HashMap<String, Bytes>> = OnceLock::new();

pub fn has_assets() -> bool {
    !assets().is_empty()
}

fn assets() -> &'static HashMap<String, Bytes> {
    ASSETS.get_or_init(unpack)
}

fn unpack() -> HashMap<String, Bytes> {
    let mut out: HashMap<String, Bytes> = HashMap::new();
    if ARCHIVE.is_empty() {
        return out;
    }

    let decoder = flate2::read::GzDecoder::new(ARCHIVE);
    let mut archive = tar::Archive::new(decoder);
    let entries = match archive.entries() {
        Ok(entries) => entries,
        Err(err) => {
            warn!(error = %err, "failed to read embedded UI archive");
            return out;
        }
    };

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warn!(error = %err, "skipping malformed UI archive entry");
                continue;
            }
        };
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let raw_path = match entry.path() {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(err) => {
                warn!(error = %err, "skipping UI archive entry with bad path");
                continue;
            }
        };
        let mut buf = Vec::with_capacity(entry.size() as usize);
        if let Err(err) = entry.read_to_end(&mut buf) {
            warn!(error = %err, path = %raw_path, "failed to read UI archive entry");
            continue;
        }
        let key = normalize(&raw_path);
        out.insert(key, Bytes::from(buf));
    }

    debug!(file_count = out.len(), "unpacked embedded UI archive");
    out
}

fn normalize(path: &str) -> String {
    path.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

pub async fn fallback(uri: Uri) -> Response {
    let map = assets();
    let raw = uri.path().trim_start_matches('/');
    let lookup = if raw.is_empty() { "index.html" } else { raw };

    if let Some(body) = map.get(lookup) {
        return ok_response(lookup, body.clone());
    }
    // SPA fallback: serve index.html for unknown paths so client-side routing works.
    if let Some(body) = map.get("index.html") {
        return ok_response("index.html", body.clone());
    }
    (
        StatusCode::NOT_FOUND,
        "UI assets not embedded in this build",
    )
        .into_response()
}

fn ok_response(path: &str, body: Bytes) -> Response {
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(guess_mime(path)),
    );
    response
}

fn guess_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
