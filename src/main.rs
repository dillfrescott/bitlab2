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
use stremio::{Catalog, CatalogResponse, Manifest, StreamResponse, Stream};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    meta_cache: Arc<RwLock<HashMap<String, (String, Option<String>)>>>,
    stream_cache: Arc<RwLock<HashMap<String, (Vec<Stream>, std::time::Instant)>>>,
}

#[tokio::main]
async fn main() {
    // Set up tracing / logging
    println!("Starting Bitlab Stremio Addon...");

    let state = AppState {
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap(),
        meta_cache: Arc::new(RwLock::new(HashMap::new())),
        stream_cache: Arc::new(RwLock::new(HashMap::new())),
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
            catalog::get_popular_movies(&state.client, &state.meta_cache).await
        }
        ("series", "popular_series") => {
            catalog::get_popular_series(&state.client, &state.meta_cache).await
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
            scraper::get_movie_streams(&state.client, &state.meta_cache, &state.stream_cache, clean_id).await
        }
        "series" => {
            // Series IDs are formatted as imdb_id:season:episode
            let parts: Vec<&str> = clean_id.split(':').collect();
            if parts.len() == 3 {
                let imdb_id = parts[0];
                let season = parts[1].parse::<u32>().unwrap_or(1);
                let episode = parts[2].parse::<u32>().unwrap_or(1);
                scraper::get_series_streams(&state.client, &state.meta_cache, &state.stream_cache, imdb_id, season, episode).await
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

    let stremio_url = manifest_url
        .replace("https://", "stremio://")
        .replace("http://", "stremio://");

    let html_content = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Bitlab - Stremio Addon</title>
    <link href="https://fonts.googleapis.com/css2?family=Outfit:wght@300;400;500;600;700;800&display=swap" rel="stylesheet">
    <style>
        :root {{
            --bg-color: #030303;
            --card-bg: rgba(15, 15, 15, 0.6);
            --text-color: #f5f5f7;
            --text-muted: #86868b;
            --accent: #00ff66;
            --accent-glow: rgba(0, 255, 102, 0.15);
            --border-color: rgba(255, 255, 255, 0.08);
            --purple-glow: rgba(147, 51, 234, 0.1);
        }}

        * {{
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }}

        body {{
            font-family: 'Outfit', sans-serif;
            background-color: var(--bg-color);
            background-image: 
                radial-gradient(circle at 10% 20%, var(--purple-glow) 0%, transparent 40%),
                radial-gradient(circle at 90% 80%, rgba(0, 255, 102, 0.05) 0%, transparent 40%);
            color: var(--text-color);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: 24px;
        }}

        .container {{
            max-width: 520px;
            width: 100%;
            display: flex;
            flex-direction: column;
            gap: 24px;
        }}

        .card {{
            background: var(--card-bg);
            backdrop-filter: blur(16px);
            -webkit-backdrop-filter: blur(16px);
            border: 1px solid var(--border-color);
            border-radius: 20px;
            padding: 32px;
            box-shadow: 0 8px 32px 0 rgba(0, 0, 0, 0.37);
            display: flex;
            flex-direction: column;
            gap: 20px;
            position: relative;
            overflow: hidden;
        }}

        .card::before {{
            content: '';
            position: absolute;
            top: 0;
            left: 0;
            right: 0;
            height: 2px;
            background: linear-gradient(90deg, transparent, var(--accent), transparent);
        }}

        .header {{
            text-align: center;
            margin-bottom: 8px;
        }}

        h1 {{
            font-size: 2.8rem;
            font-weight: 800;
            letter-spacing: -0.04em;
            text-transform: uppercase;
            background: linear-gradient(135deg, #ffffff 0%, #a1a1aa 100%);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            margin-bottom: 6px;
        }}

        .subtitle {{
            font-size: 0.9rem;
            color: var(--accent);
            letter-spacing: 0.15em;
            text-transform: uppercase;
            font-weight: 600;
        }}

        .desc {{
            font-size: 0.95rem;
            color: var(--text-muted);
            line-height: 1.6;
            text-align: center;
        }}

        .divider {{
            height: 1px;
            background: var(--border-color);
            width: 100%;
        }}

        .install-section {{
            display: flex;
            flex-direction: column;
            gap: 12px;
        }}

        .btn-install {{
            background: var(--accent);
            color: #000000;
            border: none;
            padding: 16px 28px;
            border-radius: 12px;
            font-size: 1rem;
            font-weight: 700;
            cursor: pointer;
            text-decoration: none;
            text-align: center;
            transition: all 0.3s ease;
            box-shadow: 0 4px 14px 0 var(--accent-glow);
            display: inline-block;
        }}

        .btn-install:hover {{
            transform: translateY(-2px);
            box-shadow: 0 6px 20px 0 var(--accent-glow);
            filter: brightness(1.1);
        }}

        .btn-install:active {{
            transform: translateY(0);
        }}

        .url-box {{
            background: rgba(0, 0, 0, 0.4);
            border: 1px solid var(--border-color);
            border-radius: 12px;
            padding: 6px 6px 6px 16px;
            display: flex;
            align-items: center;
            justify-content: space-between;
            gap: 12px;
            transition: border-color 0.3s ease;
        }}

        .url-box:hover {{
            border-color: rgba(255, 255, 255, 0.15);
        }}

        .url-text {{
            font-family: monospace;
            font-size: 0.85rem;
            color: var(--text-color);
            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
            user-select: all;
            opacity: 0.8;
        }}

        .btn-copy {{
            background: rgba(255, 255, 255, 0.08);
            color: var(--text-color);
            border: 1px solid var(--border-color);
            padding: 10px 18px;
            border-radius: 8px;
            font-size: 0.8rem;
            font-weight: 600;
            cursor: pointer;
            transition: all 0.2s ease;
            white-space: nowrap;
        }}

        .btn-copy:hover {{
            background: rgba(255, 255, 255, 0.12);
            border-color: rgba(255, 255, 255, 0.2);
        }}

        .copied {{
            background: var(--accent) !important;
            color: #000000 !important;
            border-color: var(--accent) !important;
        }}

        .features {{
            display: flex;
            flex-direction: column;
            gap: 14px;
        }}

        .feature-title {{
            font-size: 0.85rem;
            color: var(--text-muted);
            text-transform: uppercase;
            letter-spacing: 0.05em;
            font-weight: 600;
            margin-bottom: 2px;
        }}

        .badge-list {{
            display: flex;
            flex-wrap: wrap;
            gap: 8px;
        }}

        .badge {{
            background: rgba(255, 255, 255, 0.04);
            border: 1px solid var(--border-color);
            color: var(--text-color);
            font-size: 0.8rem;
            padding: 6px 12px;
            border-radius: 20px;
            display: flex;
            align-items: center;
            gap: 6px;
            font-weight: 500;
            transition: all 0.2s ease;
        }}

        .badge:hover {{
            background: rgba(255, 255, 255, 0.08);
            border-color: rgba(255, 255, 255, 0.15);
        }}

        .badge .dot {{
            width: 6px;
            height: 6px;
            background: var(--accent);
            border-radius: 50%;
            box-shadow: 0 0 8px var(--accent);
        }}

        .footer {{
            text-align: center;
            font-size: 0.75rem;
            color: var(--text-muted);
            display: flex;
            flex-direction: column;
            gap: 6px;
        }}
    </style>
</head>
<body>
    <div class="container">
        <div class="card">
            <div class="header">
                <h1>Bitlab</h1>
                <div class="subtitle">Stremio Addon</div>
            </div>
            
            <p class="desc">A high-performance Stremio scraper addon. Resolves high-speed streaming links from popular trackers instantly with background caching.</p>
            
            <div class="divider"></div>
            
            <div class="install-section">
                <a href="{stremio_url}" class="btn-install">Install Addon</a>
            </div>
            
            <div class="url-box">
                <span class="url-text" id="manifest-url">{manifest_url}</span>
                <button class="btn-copy" onclick="copyManifestUrl()" id="copy-btn">Copy URL</button>
            </div>

            <div class="divider"></div>

            <div class="features">
                <div class="feature-title">Active Scraping Backends</div>
                <div class="badge-list">
                    <div class="badge"><span class="dot"></span>YTS Movies</div>
                    <div class="badge"><span class="dot"></span>APIBay TPB</div>
                    <div class="badge"><span class="dot"></span>ThePirateBay</div>
                    <div class="badge"><span class="dot"></span>Nyaa Anime</div>
                    <div class="badge"><span class="dot"></span>EZTV Shows</div>
                    <div class="badge"><span class="dot"></span>Cinemeta API</div>
                </div>
            </div>
        </div>
        
        <div class="footer">
            <p>Bitlab Addon v1.0.0 &bull; Running in high-performance mode</p>
            <p>Powered by Rust, Tokio, and Axum</p>
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
                    copyBtn.innerText = 'Copy URL';
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
