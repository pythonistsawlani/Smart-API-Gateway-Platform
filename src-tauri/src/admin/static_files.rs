use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use include_dir::{include_dir, Dir};

use std::path::{Component, Path, PathBuf};

static WEB_ADMIN_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../dist-web-admin");

fn dist_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dist-web-admin")
}

fn read_embedded_text(path: &str) -> Option<String> {
    WEB_ADMIN_DIST
        .get_file(path)
        .and_then(|file| file.contents_utf8())
        .map(ToString::to_string)
}

fn read_embedded_bytes(path: &str) -> Option<Vec<u8>> {
    WEB_ADMIN_DIST
        .get_file(path)
        .map(|file| file.contents().to_vec())
}

fn read_text(path: &Path, embedded_path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .or_else(|| read_embedded_text(embedded_path))
}

fn read_bytes(path: &Path, embedded_path: &str) -> Option<Vec<u8>> {
    std::fs::read(path)
        .ok()
        .or_else(|| read_embedded_bytes(embedded_path))
}

fn content_type_for(path: &str) -> &'static str {
    if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "application/octet-stream"
    }
}

fn cache_control_for(path: &str) -> &'static str {
    if path.ends_with("index.html") {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    }
}

fn safe_dist_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path == "assets" || path.contains('\\') {
        return None;
    }

    let relative = Path::new(path);
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }

    Some(dist_dir().join(relative))
}

pub async fn admin_index() -> impl IntoResponse {
    let dist = dist_dir();
    let index_path = dist.join("index.html");
    let Some(mut html) = read_text(&index_path, "index.html") else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Dynamic assets already use hashed filenames directly in build output (dist) index.html,
    // so there is no need for placeholder replacement at runtime. A check can be added here if ensuring entry assets exist is still needed.
    // The current implementation preserves raw HTML content, avoiding hardcoded filenames.
    // If custom asset injection is needed in the future, the result of `entry_assets()` can be used here.
    // If the backend relocates the Admin service in single-port mode, the new base path will be provided via the ADMIN_BASE_URL environment variable.
    if let Ok(base_url) = std::env::var("ADMIN_BASE_URL") {
        html = html.replace("%ADMIN_BASE_URL%", &base_url);
    }

    // Example (commented out):
    // if let Some((js, css_list)) = entry_assets() {
    //     // Logs can be recorded or other business logic executed here
    // }
    //
    // Directly return the read HTML.
    //
    // (Preserve original code structure, only removing hardcoded replacements)

    let mut response = Html(html).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

pub async fn admin_asset_root(uri: Uri) -> Response {
    let path = uri.path();

    // Strip root prefixes: /assets/, /logo/, etc.
    let stripped = if let Some(rest) = path.strip_prefix("/assets/") {
        format!("assets/{rest}")
    } else if let Some(rest) = path.strip_prefix("/logo/") {
        format!("logo/{rest}")
    } else if path == "/star.jpg" {
        "star.jpg".to_string()
    } else if path == "/favicon.ico" {
        "favicon.ico".to_string()
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if stripped.is_empty() || stripped == "assets" {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(full_path) = safe_dist_path(&stripped) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(bytes) = read_bytes(&full_path, &stripped) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut response = Response::new(bytes.into());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for(&stripped)),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for(&stripped)),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_web_admin_index_is_available() {
        let html = read_embedded_text("index.html").expect("Web Admin index.html should be embedded in binary");
        assert!(html.contains("<html") || html.contains("<!doctype html"));
    }
}
