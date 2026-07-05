mod stremio;
mod cinemeta;
mod catalog;
mod scraper;

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use stremio::{Catalog, CatalogResponse, Manifest, StreamResponse};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    meta_cache: Arc<RwLock<HashMap<String, (String, Option<String>)>>>,
}

#[tokio::main]
async fn main() {
    // Set up tracing / logging
    println!("Starting Bitlab Stremio Addon...");

    let state = AppState {
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(4))
            .build()
            .unwrap(),
        meta_cache: Arc::new(RwLock::new(HashMap::new())),
    };

    // Configure CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_headers(Any)
        .allow_methods(Any);

    // Set up routes
    let app = Router::new()
        .route("/", get(landing_handler))
        .route("/manifest.json", get(manifest_handler))
        .route("/catalog/:type/:id", get(catalog_handler))
        .route("/catalog/:type/:id/:extra", get(catalog_handler)) // handles extra parameters
        .route("/stream/:type/:id", get(stream_handler))
        .layer(middleware::from_fn(host_validation_middleware))
        .layer(cors)
        .with_state(state);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Listening on http://{}", addr);
    println!("Addon manifest is available at http://{}/manifest.json", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn manifest_handler() -> impl IntoResponse {
    let manifest = Manifest {
        id: "org.bitlab.stremio".to_string(),
        version: "1.0.0".to_string(),
        name: "Bitlab".to_string(),
        description: Some("A high-performance Stremio scraper addon by Bitlab.".to_string()),
        resources: vec!["catalog".to_string(), "stream".to_string()],
        types: vec!["movie".to_string(), "series".to_string()],
        catalogs: vec![
            Catalog {
                r#type: "movie".to_string(),
                id: "popular_movies".to_string(),
                name: "🚀 Popular Movies (Scraped)".to_string(),
                extra: None,
            },
            Catalog {
                r#type: "series".to_string(),
                id: "popular_series".to_string(),
                name: "🚀 Popular TV Shows (Scraped)".to_string(),
                extra: None,
            },
        ],
        id_prefixes: vec!["tt".to_string()],
    };

    Json(manifest)
}

async fn catalog_handler(
    Path((r#type, id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let clean_id = id.strip_suffix(".json").unwrap_or(&id);
    println!("Catalog requested: type={}, id={}", r#type, clean_id);

    let metas = match (r#type.as_str(), clean_id) {
        ("movie", "popular_movies") => {
            catalog::get_popular_movies(&state.client).await
        }
        ("series", "popular_series") => {
            catalog::get_popular_series(&state.client).await
        }
        _ => Vec::new(),
    };

    Json(CatalogResponse { metas })
}

async fn stream_handler(
    Path((r#type, id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let clean_id = id.strip_suffix(".json").unwrap_or(&id);
    println!("Stream requested: type={}, id={}", r#type, clean_id);

    let streams = match r#type.as_str() {
        "movie" => {
            scraper::get_movie_streams(&state.client, &state.meta_cache, clean_id).await
        }
        "series" => {
            // Series IDs are formatted as imdb_id:season:episode
            let parts: Vec<&str> = clean_id.split(':').collect();
            if parts.len() == 3 {
                let imdb_id = parts[0];
                let season = parts[1].parse::<u32>().unwrap_or(1);
                let episode = parts[2].parse::<u32>().unwrap_or(1);
                scraper::get_series_streams(&state.client, &state.meta_cache, imdb_id, season, episode).await
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    Json(StreamResponse { streams })
}

async fn landing_handler(headers: HeaderMap) -> impl IntoResponse {
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost:3000");

    let manifest_url = if let Ok(allowed) = std::env::var("ALLOWED_URL") {
        format!("{}/manifest.json", allowed.trim_end_matches('/'))
    } else {
        let proto = headers
            .get("x-forwarded-proto")
            .and_then(|p| p.to_str().ok())
            .unwrap_or("http");
        format!("{}://{}/manifest.json", proto, host)
    };

    let html_content = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Bitlab</title>
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;600;800&display=swap" rel="stylesheet">
    <style>
        :root {{
            --bg-color: #000000;
            --text-color: #ffffff;
            --text-muted: #888888;
            --accent: #00ff66;
            --border-color: #222222;
        }}

        * {{
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }}

        body {{
            font-family: 'Outfit', sans-serif;
            background-color: var(--bg-color);
            color: var(--text-color);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: 20px;
        }}

        .container {{
            max-width: 480px;
            width: 100%;
            text-align: center;
        }}

        h1 {{
            font-size: 3rem;
            font-weight: 800;
            letter-spacing: -0.03em;
            margin-bottom: 8px;
            text-transform: uppercase;
            background: linear-gradient(180deg, #ffffff 0%, #aaaaaa 100%);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
        }}

        .subtitle {{
            font-size: 1rem;
            color: var(--text-muted);
            margin-bottom: 40px;
            letter-spacing: 0.1em;
            text-transform: uppercase;
        }}

        .url-box {{
            background: #0a0a0a;
            border: 1px solid var(--border-color);
            border-radius: 12px;
            padding: 8px 8px 8px 16px;
            display: flex;
            align-items: center;
            justify-content: space-between;
            gap: 12px;
            transition: border-color 0.3s ease;
        }}

        .url-box:hover {{
            border-color: #333333;
        }}

        .url-text {{
            font-family: monospace;
            font-size: 0.9rem;
            color: var(--text-color);
            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
            user-select: all;
        }}

        .btn-copy {{
            background: #ffffff;
            color: #000000;
            border: none;
            padding: 12px 24px;
            border-radius: 8px;
            font-size: 0.85rem;
            font-weight: 600;
            cursor: pointer;
            transition: all 0.2s ease;
            white-space: nowrap;
        }}

        .btn-copy:hover {{
            background: #dddddd;
            transform: scale(1.02);
        }}

        .btn-copy:active {{
            transform: scale(0.98);
        }}

        .copied {{
            background: var(--accent) !important;
            color: #000000 !important;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>Bitlab</h1>
        <div class="subtitle">Stremio Addon</div>

        <div class="url-box">
            <span class="url-text" id="manifest-url">{manifest_url}</span>
            <button class="btn-copy" onclick="copyManifestUrl()" id="copy-btn">Copy</button>
        </div>
    </div>

    <script>
        function copyManifestUrl() {{
            const urlText = document.getElementById('manifest-url').innerText;
            navigator.clipboard.writeText(urlText).then(() => {{
                const copyBtn = document.getElementById('copy-btn');
                copyBtn.innerText = 'Copied!';
                copyBtn.classList.add('copied');
                setTimeout(() => {{
                    copyBtn.innerText = 'Copy';
                    copyBtn.classList.remove('copied');
                }}, 2000);
            }});
        }}
    </script>
</body>
</html>"#
    );

    Html(html_content)
}

async fn host_validation_middleware(
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Ok(allowed_url) = std::env::var("ALLOWED_URL") {
        // Extract host header (prefer X-Forwarded-Host if behind a proxy)
        let host = req
            .headers()
            .get("x-forwarded-host")
            .or_else(|| req.headers().get("host"))
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
            
        // Extract protocol
        let proto = req
            .headers()
            .get("x-forwarded-proto")
            .and_then(|p| p.to_str().ok())
            .unwrap_or("http");
            
        let request_url = format!("{}://{}", proto, host);
        
        if let (Some(req_parsed), Some(allow_parsed)) = (
            parse_scheme_host_port(&request_url),
            parse_scheme_host_port(&allowed_url),
        ) {
            if req_parsed != allow_parsed {
                println!(
                    "Forbidden access: Request URL scheme/host/port ({:?}) does not match ALLOWED_URL ({:?})",
                    req_parsed, allow_parsed
                );
                return Err(StatusCode::FORBIDDEN);
            }
        } else {
            println!(
                "Forbidden access: Failed to parse request URL ({}) or ALLOWED_URL ({})",
                request_url, allowed_url
            );
            return Err(StatusCode::FORBIDDEN);
        }
    }
    
    Ok(next.run(req).await)
}

fn parse_scheme_host_port(url_str: &str) -> Option<(String, String, Option<u16>)> {
    let parts: Vec<&str> = url_str.splitn(2, "://").collect();
    if parts.len() != 2 {
        return None;
    }
    let scheme = parts[0].to_lowercase();
    
    // Split by '/' to remove path, e.g. "bitlab.dill.moe/manifest.json" -> "bitlab.dill.moe"
    let host_port_path: Vec<&str> = parts[1].splitn(2, '/').collect();
    let host_port = host_port_path[0];
    
    let host_port_parts: Vec<&str> = host_port.splitn(2, ':').collect();
    let host = host_port_parts[0].to_lowercase();
    let mut port = None;
    if host_port_parts.len() == 2 {
        if let Ok(p) = host_port_parts[1].parse::<u16>() {
            port = Some(p);
        }
    }
    
    let normalized_port = match (scheme.as_str(), port) {
        ("http", Some(80)) => None,
        ("https", Some(443)) => None,
        _ => port,
    };
    
    Some((scheme, host, normalized_port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scheme_host_port() {
        assert_eq!(
            parse_scheme_host_port("https://bitlab.dill.moe"),
            Some(("https".to_string(), "bitlab.dill.moe".to_string(), None))
        );
        assert_eq!(
            parse_scheme_host_port("https://bitlab.dill.moe/"),
            Some(("https".to_string(), "bitlab.dill.moe".to_string(), None))
        );
        assert_eq!(
            parse_scheme_host_port("https://bitlab.dill.moe/manifest.json"),
            Some(("https".to_string(), "bitlab.dill.moe".to_string(), None))
        );
        assert_eq!(
            parse_scheme_host_port("https://bitlab.dill.moe:443"),
            Some(("https".to_string(), "bitlab.dill.moe".to_string(), None))
        );
        assert_eq!(
            parse_scheme_host_port("http://localhost:3000/manifest.json"),
            Some(("http".to_string(), "localhost".to_string(), Some(3000)))
        );
        assert_eq!(
            parse_scheme_host_port("http://127.0.0.1:80/"),
            Some(("http".to_string(), "127.0.0.1".to_string(), None))
        );
        assert_eq!(
            parse_scheme_host_port("invalid_url"),
            None
        );
    }
}
