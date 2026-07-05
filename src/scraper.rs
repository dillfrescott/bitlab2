use crate::stremio::Stream;
use crate::cinemeta::fetch_meta;
use serde::Deserialize;
use regex::Regex;

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct YtsSearchResponse {
    status: String,
    data: Option<YtsSearchData>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct YtsSearchData {
    #[serde(default)]
    movie_count: u32,
    movies: Option<Vec<YtsSearchMovie>>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct YtsSearchMovie {
    title: String,
    torrents: Option<Vec<YtsTorrent>>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct YtsTorrent {
    hash: String,
    quality: String,
    r#type: String,
    seeds: u32,
    peers: u32,
    size: String,
}

use scraper::{Html, Selector};

fn extract_hash_from_magnet(magnet: &str) -> Option<String> {
    let prefix = "magnet:?xt=urn:btih:";
    if let Some(idx) = magnet.find(prefix) {
        let hash_start = idx + prefix.len();
        let sub = &magnet[hash_start..];
        if let Some(end_idx) = sub.find('&') {
            return Some(sub[..end_idx].to_string());
        }
        return Some(sub.to_string());
    }
    None
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&#39;", "'")
     .replace("&#x27;", "'")
     .replace("&amp;", "&")
     .replace("&quot;", "\"")
     .replace("&lt;", "<")
     .replace("&gt;", ">")
}

fn format_size(bytes_str: &str) -> String {
    if let Ok(bytes) = bytes_str.parse::<u64>() {
        if bytes >= 1024 * 1024 * 1024 {
            format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        } else if bytes >= 1024 * 1024 {
            format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{} B", bytes)
        }
    } else {
        "Unknown size".to_string()
    }
}

fn get_json_string(val: &serde_json::Value) -> String {
    if let Some(s) = val.as_str() {
        s.to_string()
    } else if let Some(n) = val.as_i64() {
        n.to_string()
    } else if let Some(f) = val.as_f64() {
        f.to_string()
    } else {
        String::new()
    }
}

fn get_json_u32(val: &serde_json::Value, default: u32) -> u32 {
    if let Some(n) = val.as_u64() {
        n as u32
    } else if let Some(s) = val.as_str() {
        s.parse::<u32>().unwrap_or(default)
    } else {
        default
    }
}

#[allow(dead_code)]
const PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.coppersurfer.tk:6969/announce",
    "udp://tracker.leechers-paradise.org:6969/announce",
    "udp://tracker.cyberia.is:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://ipv4.tracker.harry.lu:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://opentracker.i2p.rocks:6969/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://denis.stalker.upeer.me:6969/announce",
    "udp://tracker.moeking.me:6969/announce",
    "http://p4p.arenabg.com:1337/announce",
    "udp://tracker.tryhackx.org:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://tracker.internetwarriors.net:1337/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://tracker.pirateparty.gr:6969/announce",
];

fn get_sources_for_torrent(info_hash: &str, name: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let magnet = build_magnet_url(info_hash, name);
    sources.push(magnet);
    for tracker in PUBLIC_TRACKERS {
        sources.push(format!("tracker:{}", tracker));
    }
    sources
}

fn extract_trackers_from_magnet(magnet: &str) -> Vec<String> {
    let mut sources = Vec::new();
    sources.push(magnet.to_string());
    for t in PUBLIC_TRACKERS {
        sources.push(format!("tracker:{}", t));
    }
    let parts: Vec<&str> = magnet.split("&tr=").collect();
    if parts.len() > 1 {
        for part in parts.iter().skip(1) {
            let tracker_encoded = part.split('&').next().unwrap_or("");
            if !tracker_encoded.is_empty() {
                if let Ok(decoded) = urlencoding::decode(tracker_encoded) {
                    let decoded_str = decoded.into_owned();
                    let source = format!("tracker:{}", decoded_str);
                    if !sources.contains(&source) {
                        sources.push(source);
                    }
                }
            }
        }
    }
    sources
}

fn build_magnet_url(info_hash: &str, display_name: &str) -> String {
    let mut url = format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        info_hash,
        urlencoding::encode(display_name)
    );
    for tracker in PUBLIC_TRACKERS {
        url.push_str("&tr=");
        url.push_str(urlencoding::encode(tracker).as_ref());
    }
    url
}

fn clean_title(title: &str) -> String {
    title
        .replace('\'', "")
        .replace('’', "")
        .replace('‘', "")
        .replace('`', "")
        .replace('´', "")
        .replace(|c: char| !c.is_alphanumeric() && c != ' ', " ")
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

fn detect_quality(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("2160p") || lower.contains("4k") || lower.contains("uhd") {
        "2160p (4K)"
    } else if lower.contains("1080p") || lower.contains("fhd") {
        "1080p (FHD)"
    } else if lower.contains("720p") || lower.contains("hd") {
        "720p (HD)"
    } else if lower.contains("480p") || lower.contains("sd") {
        "480p"
    } else {
        "SD / Unknown"
    }
}

/// Scrapes YTS for movie streams using its API
pub async fn scrape_yts_movies(client: &reqwest::Client, imdb_id: &str) -> Vec<Stream> {
    let mut streams = Vec::new();
    let urls = vec![
        format!("https://movies-api.accel.li/api/v2/list_movies.json?query_term={}", imdb_id),
        format!("https://yts.mx/api/v2/list_movies.json?query_term={}", imdb_id),
    ];
    
    for url in urls {
        if let Ok(resp) = client.get(&url).send().await {
            if let Ok(json_resp) = resp.json::<YtsSearchResponse>().await {
                if json_resp.status == "ok" && json_resp.data.is_some() {
                    let data = json_resp.data.unwrap();
                    if let Some(movies) = data.movies {
                        for movie in movies {
                            if let Some(torrents) = movie.torrents {
                                for torrent in torrents {
                                    let quality = format!("{} ({})", torrent.quality, torrent.r#type.to_uppercase());
                                    
                                    let peers_info = if torrent.seeds == 0 {
                                        "👥 Active (YTS Swarm)".to_string()
                                    } else {
                                        format!("👥 {} seeders | 📥 {} peers", torrent.seeds, torrent.peers)
                                    };
                                    
                                    let magnet = build_magnet_url(&torrent.hash, &movie.title);
                                    let sources = get_sources_for_torrent(&torrent.hash, &movie.title);
                                    streams.push(Stream {
                                        name: format!("[Bitlab] {}", quality),
                                        title: format!(
                                            "🎬 YTS: {}\n📦 {}\n{}\n⚡ Magnet (P2P Stream)",
                                            movie.title,
                                            torrent.size,
                                            peers_info
                                        ),
                                        url: Some(magnet),
                                        info_hash: Some(torrent.hash.clone().to_lowercase()),
                                        file_idx: None,
                                        sources: Some(sources),
                                        behavior_hints: None,
                                    });
                                }
                            }
                        }
                        if !streams.is_empty() {
                            break;
                        }
                    }
                }
            }
        }
    }
    
    streams
}

/// Scrapes TPB Proxy (tpb.party) using HTML selectors
pub async fn scrape_tpb_html(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let encoded_query = urlencoding::encode(query);
    let url = format!("https://tpb.party/search/{}/1/99/0", encoded_query);
    
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36");
        
    let html_text = match req.send().await {
        Ok(resp) => match resp.text().await {
            Ok(t) => t,
            Err(_) => return streams,
        },
        Err(_) => return streams,
    };
    
    let document = Html::parse_document(&html_text);
    
    let row_selector = Selector::parse("#searchResult tr").unwrap();
    let col2_link_selector = Selector::parse("td:nth-child(2) a").unwrap();
    let magnet_selector = Selector::parse("a[href^=\"magnet:?\"]").unwrap();
    let col5_selector = Selector::parse("td:nth-child(5)").unwrap();
    let col6_selector = Selector::parse("td:nth-child(6)").unwrap();
    let col7_selector = Selector::parse("td:nth-child(7)").unwrap();
    
    for row in document.select(&row_selector) {
        let magnet = match row.select(&magnet_selector).next() {
            Some(el) => el.value().attr("href").unwrap_or(""),
            None => continue,
        };
        
        if magnet.is_empty() {
            continue;
        }
        
        let info_hash = match extract_hash_from_magnet(magnet) {
            Some(h) => h.to_lowercase(),
            None => continue,
        };
        
        let name = match row.select(&col2_link_selector).next() {
            Some(el) => el.text().collect::<Vec<_>>().concat(),
            None => "Unknown Torrent".to_string(),
        };
        
        let size = match row.select(&col5_selector).next() {
            Some(el) => el.text().collect::<Vec<_>>().concat().replace("&nbsp;", " "),
            None => "Unknown size".to_string(),
        };
        
        let seeds_str = match row.select(&col6_selector).next() {
            Some(el) => el.text().collect::<Vec<_>>().concat(),
            None => "0".to_string(),
        };
        let seeds = seeds_str.trim().parse::<u32>().unwrap_or(0);
        if seeds == 0 {
            continue;
        }
        
        let leechers_str = match row.select(&col7_selector).next() {
            Some(el) => el.text().collect::<Vec<_>>().concat(),
            None => "0".to_string(),
        };
        let leechers = leechers_str.trim().parse::<u32>().unwrap_or(0);
        
        let quality = detect_quality(&name);
        let seeds_display = format!("{} seeders", seeds);
        let leechers_display = format!("{} peers", leechers);
        
        let sources = extract_trackers_from_magnet(magnet);
        streams.push(Stream {
            name: format!("[Bitlab] {}", quality),
            title: format!(
                "🎬 {}: {}\n📦 {}\n👥 {} | 📥 {}\n⚡ Magnet (P2P Stream)",
                provider_label,
                name,
                size,
                seeds_display,
                leechers_display
            ),
            url: Some(magnet.to_string()),
            info_hash: Some(info_hash),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
    }
    
    streams
}

#[derive(Deserialize, Debug)]
pub struct ApibayTorrent {
    pub name: Option<String>,
    pub info_hash: Option<String>,
    pub size: Option<serde_json::Value>,
    pub seeders: Option<serde_json::Value>,
    pub leechers: Option<serde_json::Value>,
}

/// Scrapes APIBay for torrents using its JSON API
pub async fn scrape_apibay(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let encoded_query = urlencoding::encode(query);
    let url = format!("https://apibay.org/q.php?q={}&cat=", encoded_query);
    
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36");
        
    let resp = match req.send().await {
        Ok(r) => r,
        Err(_) => return streams,
    };
    
    let text = match resp.text().await {
        Ok(t) => t,
        Err(_) => return streams,
    };

    let torrents: Vec<ApibayTorrent> = match serde_json::from_str(&text) {
        Ok(t) => t,
        Err(_) => return streams,
    };
    
    for torrent in torrents {
        let name = torrent.name.unwrap_or_default();
        let info_hash = torrent.info_hash.unwrap_or_default();
        if name.is_empty() || info_hash.is_empty() || name == "No results found" {
            continue;
        }
        
        let hash = info_hash.to_lowercase();
        let quality = detect_quality(&name);
        
        let size_str = torrent.size.map(|v| get_json_string(&v)).unwrap_or_default();
        let size_formatted = format_size(&size_str);
        
        let seeds_str = torrent.seeders.map(|v| get_json_string(&v)).unwrap_or_default();
        let seeds = seeds_str.parse::<u32>().unwrap_or(0);
        if seeds == 0 {
            continue;
        }
        
        let peers_str = torrent.leechers.map(|v| get_json_string(&v)).unwrap_or_default();
        let peers = peers_str.parse::<u32>().unwrap_or(0);
        
        let seeds_display = format!("{} seeders", seeds);
        let leechers_display = format!("{} peers", peers);
        
        let magnet = build_magnet_url(&hash, &name);
        let sources = get_sources_for_torrent(&hash, &name);
        streams.push(Stream {
            name: format!("[Bitlab] {}", quality),
            title: format!(
                "🎬 {}: {}\n📦 {}\n👥 {} | 📥 {}\n⚡ Magnet (P2P Stream)",
                provider_label,
                name,
                size_formatted,
                seeds_display,
                leechers_display
            ),
            url: Some(magnet),
            info_hash: Some(hash),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
    }
    
    streams
}

/// Scrapes Nyaa RSS feed using standard regex XML matching
pub async fn scrape_nyaa(client: &reqwest::Client, query: &str) -> Vec<Stream> {
    let mut streams = Vec::new();
    let encoded_query = urlencoding::encode(query);
    let url = format!("https://nyaa.si/?page=rss&c=1_2&q={}", encoded_query);
    
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36");
        
    let xml_text = match req.send().await {
        Ok(resp) => match resp.text().await {
            Ok(t) => t,
            Err(_) => return streams,
        },
        Err(_) => return streams,
    };
    
    let item_re = match Regex::new(r"(?s)<item>(.*?)</item>") {
        Ok(re) => re,
        Err(_) => return streams,
    };
    let title_re = Regex::new(r"<title>(.*?)</title>").unwrap();
    let hash_re = Regex::new(r"<nyaa:infoHash>(.*?)</nyaa:infoHash>").unwrap();
    let size_re = Regex::new(r"<nyaa:size>(.*?)</nyaa:size>").unwrap();
    let seeders_re = Regex::new(r"<nyaa:seeders>(\d+)</nyaa:seeders>").unwrap();
    let leechers_re = Regex::new(r"<nyaa:leechers>(\d+)</nyaa:leechers>").unwrap();
    
    for cap in item_re.captures_iter(&xml_text) {
        let item_content = &cap[1];
        let raw_title = title_re.captures(item_content).map(|c| c[1].to_string()).unwrap_or_default();
        let title = decode_html_entities(&raw_title);
        let hash = hash_re.captures(item_content).map(|c| c[1].to_string()).unwrap_or_default();
        let size = size_re.captures(item_content).map(|c| c[1].to_string()).unwrap_or_default();
        let seeders = seeders_re.captures(item_content).and_then(|c| c[1].parse::<u32>().ok()).unwrap_or(0);
        let leechers = leechers_re.captures(item_content).and_then(|c| c[1].parse::<u32>().ok()).unwrap_or(0);
        
        if hash.is_empty() || title.is_empty() || seeders == 0 {
            continue;
        }
        
        let quality = detect_quality(&title);
        
        let magnet = build_magnet_url(&hash, &title);
        let sources = get_sources_for_torrent(&hash, &title);
        streams.push(Stream {
            name: format!("[Bitlab] {}", quality),
            title: format!(
                "🌸 Nyaa: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Magnet (P2P Stream)",
                title,
                size,
                seeders,
                leechers
            ),
            url: Some(magnet),
            info_hash: Some(hash.to_lowercase()),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
    }
    
    streams
}

/// Scrapes EZTV API for series episodes using IMDb ID (with pagination)
pub async fn scrape_eztv(
    client: &reqwest::Client,
    imdb_id: &str,
    target_season: u32,
    target_episode: u32,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let clean_imdb_id = imdb_id.strip_prefix("tt").unwrap_or(imdb_id);
    
    // EZTV paginates at 30 per page — fetch up to 5 pages to cover long-running shows
    for page in 1..=5 {
        let url = format!(
            "https://eztv.re/api/get-torrents?imdb_id={}&limit=30&page={}",
            clean_imdb_id, page
        );
        
        let req = client.get(&url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36");
            
        let resp_text = match req.send().await {
            Ok(resp) => match resp.text().await {
                Ok(t) => t,
                Err(_) => break,
            },
            Err(_) => break,
        };
        
        let json_val: serde_json::Value = match serde_json::from_str(&resp_text) {
            Ok(v) => v,
            Err(_) => break,
        };
        
        let torrents = match json_val.get("torrents") {
            Some(t) => match t.as_array() {
                Some(arr) => arr,
                None => break,
            },
            None => break,
        };

        if torrents.is_empty() {
            break;
        }
        
        for item in torrents {
            let hash = get_json_string(item.get("hash").unwrap_or(&serde_json::Value::Null));
            let title = get_json_string(item.get("title").unwrap_or(&serde_json::Value::Null));
            let season_str = get_json_string(item.get("season").unwrap_or(&serde_json::Value::Null));
            let episode_str = get_json_string(item.get("episode").unwrap_or(&serde_json::Value::Null));
            let seeds = get_json_u32(item.get("seeds").unwrap_or(&serde_json::Value::Null), 0);
            let peers = get_json_u32(item.get("peers").unwrap_or(&serde_json::Value::Null), 0);
            let size_bytes_str = get_json_string(item.get("size_bytes").unwrap_or(&serde_json::Value::Null));
            
            if hash.is_empty() || title.is_empty() {
                continue;
            }
            
            let season = season_str.parse::<u32>().unwrap_or(0);
            let episode = episode_str.parse::<u32>().unwrap_or(0);
            
            if season != target_season || episode != target_episode {
                continue;
            }
            
            let size_formatted = format_size(&size_bytes_str);
            let quality = detect_quality(&title);
            
            let peers_info = if seeds == 0 {
                "👥 Active (EZTV Swarm)".to_string()
            } else {
                format!("👥 {} seeders | 📥 {} peers", seeds, peers)
            };
            
            let magnet = build_magnet_url(&hash, &title);
            let sources = get_sources_for_torrent(&hash, &title);
            streams.push(Stream {
                name: format!("[Bitlab] {}", quality),
                title: format!(
                    "📺 EZTV: {}\n📦 {}\n{}\n⚡ Magnet (P2P Stream)",
                    title,
                    size_formatted,
                    peers_info
                ),
                url: Some(magnet),
                info_hash: Some(hash.to_lowercase()),
                file_idx: None,
                sources: Some(sources),
                behavior_hints: None,
            });
        }

        // If we already found matches for this episode, no need for more pages
        if !streams.is_empty() {
            break;
        }

        // Check if there are more pages
        let total_count = json_val.get("torrents_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if (page as u64) * 30 >= total_count {
            break;
        }
    }
    
    streams
}

pub async fn fetch_meta_cached(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>>,
    r#type: &str,
    imdb_id: &str,
) -> Option<(String, Option<String>)> {
    {
        let cache = meta_cache.read().await;
        if let Some(meta) = cache.get(imdb_id) {
            return Some(meta.clone());
        }
    }
    
    let meta_fut = fetch_meta(client, r#type, imdb_id);
    let meta_res = tokio::time::timeout(
        std::time::Duration::from_millis(3000),
        meta_fut
    ).await;
    
    if let Ok(Some(meta)) = meta_res {
        let mut cache = meta_cache.write().await;
        cache.insert(imdb_id.to_string(), meta.clone());
        Some(meta)
    } else {
        None
    }
}

async fn fetch_kitsu_romaji_title(client: &reqwest::Client, english_title: &str) -> Option<String> {
    let encoded = urlencoding::encode(english_title);
    let url = format!("https://kitsu.io/api/edge/anime?filter[text]={}", encoded);
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "application/vnd.api+json")
        .header("Content-Type", "application/vnd.api+json");
    
    let resp = req.send().await.ok()?;
    if resp.status().is_success() {
        let json: serde_json::Value = resp.json().await.ok()?;
        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            if let Some(first) = data.first() {
                if let Some(attributes) = first.get("attributes") {
                    if let Some(titles) = attributes.get("titles") {
                        if let Some(en_jp) = titles.get("en_jp").and_then(|t| t.as_str()) {
                            if en_jp.to_lowercase() != english_title.to_lowercase() {
                                return Some(en_jp.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Main entry point to get streams for a movie
pub async fn get_movie_streams(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>>,
    stream_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (Vec<Stream>, std::time::Instant)>>>,
    imdb_id: &str,
) -> Vec<Stream> {
    // Check stream cache
    {
        let cache = stream_cache.read().await;
        if let Some((streams, timestamp)) = cache.get(imdb_id) {
            if timestamp.elapsed().as_secs() < 1800 {
                println!("[INFO] Returning cached streams for movie: {}", imdb_id);
                return streams.clone();
            }
        }
    }

    println!("[INFO] Resolving streams for movie: {}", imdb_id);
    let start_time = std::time::Instant::now();
    
    // Spawn ID-based scrapes immediately in parallel
    let client_yts = client.clone();
    let imdb_id_clone = imdb_id.to_string();
    let yts_handle = tokio::spawn(async move {
        scrape_yts_movies(&client_yts, &imdb_id_clone).await
    });

    let client_apibay_id = client.clone();
    let imdb_id_clone2 = imdb_id.to_string();
    let apibay_id_handle = tokio::spawn(async move {
        scrape_apibay(&client_apibay_id, &imdb_id_clone2, "APIBay").await
    });
    
    let client_meta = client.clone();
    let meta_cache_clone = meta_cache.clone();
    let imdb_id_clone3 = imdb_id.to_string();
    let meta_handle = tokio::spawn(async move {
        fetch_meta_cached(&client_meta, &meta_cache_clone, "movie", &imdb_id_clone3).await
    });

    // Await metadata fetch
    let meta_res = match tokio::time::timeout(std::time::Duration::from_millis(2000), meta_handle).await {
        Ok(Ok(Some(meta))) => Some(meta),
        _ => None,
    };

    let mut title_handles = Vec::new();
    if let Some((name, year)) = &meta_res {
        let cleaned = clean_title(name);
        let client_kitsu = client.clone();
        let name_clone = name.clone();
        let kitsu_title_fut = async move {
            fetch_kitsu_romaji_title(&client_kitsu, &name_clone).await
        };
        let kitsu_title = kitsu_title_fut.await;
        let cleaned_romaji = kitsu_title.map(|t| clean_title(&t));
        
        let query = if let Some(yr) = year {
            format!("{} {}", cleaned, yr)
        } else {
            cleaned.clone()
        };

        // TPB Title Search
        let client_tpb = client.clone();
        let query_tpb = query.clone();
        title_handles.push(tokio::spawn(async move {
            scrape_tpb_html(&client_tpb, &query_tpb, "TPB").await
        }));

        // APIBay Title Search
        let client_apibay = client.clone();
        let query_apibay = query.clone();
        title_handles.push(tokio::spawn(async move {
            scrape_apibay(&client_apibay, &query_apibay, "APIBay").await
        }));

        // Nyaa RSS Search (Anime)
        let client_nyaa = client.clone();
        let query_nyaa = query.clone();
        let cleaned_romaji_clone = cleaned_romaji.clone();
        let year_clone = year.clone();
        title_handles.push(tokio::spawn(async move {
            let fut1 = scrape_nyaa(&client_nyaa, &query_nyaa);
            let fut2 = async {
                if let Some(q) = &cleaned_romaji_clone {
                    let mut q_with_yr = q.clone();
                    if let Some(yr) = &year_clone {
                        q_with_yr = format!("{} {}", q, yr);
                    }
                    scrape_nyaa(&client_nyaa, &q_with_yr).await
                } else {
                    Vec::new()
                }
            };
            let (res1, res2) = tokio::join!(fut1, fut2);
            let mut combined = res1;
            for stream in res2 {
                if !combined.iter().any(|s| s.info_hash == stream.info_hash) {
                    combined.push(stream);
                }
            }
            combined
        }));
    }

    let mut all_streams = Vec::new();

    // Gather YTS results
    if let Ok(Ok(streams)) = tokio::time::timeout(std::time::Duration::from_millis(2500), yts_handle).await {
        all_streams.extend(streams);
    }

    // Gather APIBay ID results
    if let Ok(Ok(streams)) = tokio::time::timeout(std::time::Duration::from_millis(2500), apibay_id_handle).await {
        for s in streams {
            if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                all_streams.push(s);
            }
        }
    }

    // Gather Title-based search results
    for handle in title_handles {
        if let Ok(Ok(streams)) = tokio::time::timeout(std::time::Duration::from_millis(3000), handle).await {
            for s in streams {
                if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                    all_streams.push(s);
                }
            }
        }
    }

    // Sort by seeders descending
    all_streams.sort_by(|a, b| {
        let a_seeds = extract_seeds(&a.title);
        let b_seeds = extract_seeds(&b.title);
        b_seeds.cmp(&a_seeds)
    });

    println!(
        "[INFO] Movie stream resolution completed in {}ms. Returning {} total streams.",
        start_time.elapsed().as_millis(),
        all_streams.len()
    );

    // Save to cache
    {
        let mut cache = stream_cache.write().await;
        cache.insert(imdb_id.to_string(), (all_streams.clone(), std::time::Instant::now()));
    }

    all_streams
}

/// Main entry point to get streams for a series episode
pub async fn get_series_streams(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>>,
    stream_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (Vec<Stream>, std::time::Instant)>>>,
    imdb_id: &str,
    season: u32,
    episode: u32,
) -> Vec<Stream> {
    let cache_key = format!("{}:{}:{}", imdb_id, season, episode);
    {
        let cache = stream_cache.read().await;
        if let Some((streams, timestamp)) = cache.get(&cache_key) {
            if timestamp.elapsed().as_secs() < 1800 {
                println!("[INFO] Returning cached streams for series: {}", cache_key);
                return streams.clone();
            }
        }
    }

    println!("[INFO] Resolving streams for series: {} S{:02}E{:02}", imdb_id, season, episode);
    let start_time = std::time::Instant::now();
    
    // Spawn EZTV ID search immediately
    let client_eztv = client.clone();
    let imdb_id_eztv = imdb_id.to_string();
    let eztv_handle = tokio::spawn(async move {
        scrape_eztv(&client_eztv, &imdb_id_eztv, season, episode).await
    });
    
    let client_meta = client.clone();
    let meta_cache_clone = meta_cache.clone();
    let imdb_id_clone = imdb_id.to_string();
    let meta_handle = tokio::spawn(async move {
        fetch_meta_cached(&client_meta, &meta_cache_clone, "series", &imdb_id_clone).await
    });

    // Await metadata fetch
    let meta_res = match tokio::time::timeout(std::time::Duration::from_millis(2000), meta_handle).await {
        Ok(Ok(Some(meta))) => Some(meta),
        _ => None,
    };

    let mut title_handles = Vec::new();
    if let Some((name, _)) = &meta_res {
        let cleaned = clean_title(name);
        let client_kitsu = client.clone();
        let name_clone = name.clone();
        let kitsu_title_fut = async move {
            fetch_kitsu_romaji_title(&client_kitsu, &name_clone).await
        };
        let kitsu_title = kitsu_title_fut.await;
        let cleaned_romaji = kitsu_title.map(|t| clean_title(&t));

        // Format 1: "Show Name S01E01"
        let query1 = format!("{} S{:02}E{:02}", cleaned, season, episode);
        // Format 2: "Show Name 1x01"
        let query2 = format!("{} {}x{:02}", cleaned, season, episode);

        // APIBay Title searches
        let client_apibay1 = client.clone();
        let q1_apibay = query1.clone();
        title_handles.push(tokio::spawn(async move {
            scrape_apibay(&client_apibay1, &q1_apibay, "APIBay").await
        }));

        let client_apibay2 = client.clone();
        let q2_apibay = query2.clone();
        title_handles.push(tokio::spawn(async move {
            scrape_apibay(&client_apibay2, &q2_apibay, "APIBay").await
        }));

        // TPB Title searches
        let client_tpb1 = client.clone();
        let q1_tpb = query1.clone();
        title_handles.push(tokio::spawn(async move {
            scrape_tpb_html(&client_tpb1, &q1_tpb, "TPB").await
        }));

        let client_tpb2 = client.clone();
        let q2_tpb = query2.clone();
        title_handles.push(tokio::spawn(async move {
            scrape_tpb_html(&client_tpb2, &q2_tpb, "TPB").await
        }));

        // Nyaa Search (Anime)
        let client_nyaa = client.clone();
        let q1_nyaa = query1.clone();
        let q2_nyaa = format!("{} {:02}", cleaned, episode);
        let cleaned_romaji_clone1 = cleaned_romaji.clone();
        let cleaned_romaji_clone2 = cleaned_romaji.clone();
        title_handles.push(tokio::spawn(async move {
            let fut1 = scrape_nyaa(&client_nyaa, &q1_nyaa);
            let fut2 = scrape_nyaa(&client_nyaa, &q2_nyaa);
            let fut3 = async {
                if let Some(romaji) = &cleaned_romaji_clone1 {
                    scrape_nyaa(&client_nyaa, &format!("{} {:02}", romaji, episode)).await
                } else {
                    Vec::new()
                }
            };
            let fut4 = async {
                if let Some(romaji) = &cleaned_romaji_clone2 {
                    scrape_nyaa(&client_nyaa, &format!("{} S{:02}E{:02}", romaji, season, episode)).await
                } else {
                    Vec::new()
                }
            };
            let (r1, r2, r3, r4) = tokio::join!(fut1, fut2, fut3, fut4);
            let mut combined = r1;
            for s in r2 {
                if !combined.iter().any(|x| x.info_hash == s.info_hash) {
                    combined.push(s);
                }
            }
            for s in r3 {
                if !combined.iter().any(|x| x.info_hash == s.info_hash) {
                    combined.push(s);
                }
            }
            for s in r4 {
                if !combined.iter().any(|x| x.info_hash == s.info_hash) {
                    combined.push(s);
                }
            }
            combined
        }));
    }

    let mut all_streams = Vec::new();

    // Gather EZTV results
    if let Ok(Ok(streams)) = tokio::time::timeout(std::time::Duration::from_millis(2500), eztv_handle).await {
        all_streams.extend(streams);
    }

    // Gather Title-based search results
    for handle in title_handles {
        if let Ok(Ok(streams)) = tokio::time::timeout(std::time::Duration::from_millis(3000), handle).await {
            for s in streams {
                if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                    all_streams.push(s);
                }
            }
        }
    }

    // Sort by seeders descending
    all_streams.sort_by(|a, b| {
        let a_seeds = extract_seeds(&a.title);
        let b_seeds = extract_seeds(&b.title);
        b_seeds.cmp(&a_seeds)
    });

    println!(
        "[INFO] Series stream resolution completed in {}ms. Returning {} total streams.",
        start_time.elapsed().as_millis(),
        all_streams.len()
    );

    // Save to cache
    {
        let mut cache = stream_cache.write().await;
        cache.insert(cache_key, (all_streams.clone(), std::time::Instant::now()));
    }

    all_streams
}

fn extract_seeds(title: &str) -> u32 {
    // Extract seeders number from formatted title string: "👥 X seeders | 📥 Y peers"
    if let Some(idx) = title.find("👥 ") {
        let sub = &title[idx + "👥 ".len()..];
        if let Some(space_idx) = sub.find(' ') {
            return sub[..space_idx].parse::<u32>().unwrap_or(0);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_title() {
        assert_eq!(clean_title("Clarkson's Farm"), "Clarksons Farm");
        assert_eq!(clean_title("Grey's Anatomy"), "Greys Anatomy");
        assert_eq!(clean_title("It’s Always Sunny in Philadelphia"), "Its Always Sunny in Philadelphia");
        assert_eq!(clean_title("Spider-Man"), "Spider Man");
        assert_eq!(clean_title("S.W.A.T."), "S W A T");
        assert_eq!(clean_title("Marvel's Agents of S.H.I.E.L.D."), "Marvels Agents of S H I E L D");
        assert_eq!(clean_title("Bob`s Burgers"), "Bobs Burgers");
        assert_eq!(clean_title("Doctor Who (2005)"), "Doctor Who 2005");
    }

    #[test]
    fn test_extract_seeds() {
        assert_eq!(extract_seeds("👥 12 seeders | 📥 0 peers"), 12);
        assert_eq!(extract_seeds("some text 👥 1234 seeders"), 1234);
        assert_eq!(extract_seeds("no emoji here"), 0);
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("Frieren&#39;s Journey"), "Frieren's Journey");
        assert_eq!(decode_html_entities("Frieren&#x27;s Journey"), "Frieren's Journey");
        assert_eq!(decode_html_entities("Spice &amp; Wolf"), "Spice & Wolf");
        assert_eq!(decode_html_entities("&quot;Gate&quot;"), "\"Gate\"");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size("2080103644"), "1.94 GiB");
        assert_eq!(format_size("1048576"), "1.00 MiB");
        assert_eq!(format_size("500"), "500 B");
        assert_eq!(format_size("invalid"), "Unknown size");
    }

    #[tokio::test]
    async fn test_nyaa_reqwest() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let url = "https://nyaa.si/?page=rss&q=frieren";
        let req = client.get(url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36");
        match req.send().await {
            Ok(resp) => {
                println!("Response status: {}", resp.status());
                match resp.text().await {
                    Ok(text) => {
                        println!("Text length: {}", text.len());
                        if text.contains("<title>") {
                            println!("Contains title!");
                        } else {
                            let end = std::cmp::min(500, text.len());
                            println!("First {} chars: {}", end, &text[..end]);
                        }
                    }
                    Err(e) => println!("Text error: {:?}", e),
                }
            }
            Err(e) => println!("Send error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_kitsu_reqwest() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let url = "https://kitsu.io/api/edge/anime?filter[text]=Food%20for%20the%20Soul";
        let req = client.get(url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept", "application/vnd.api+json")
            .header("Content-Type", "application/vnd.api+json");
        match req.send().await {
            Ok(resp) => {
                println!("Response status: {}", resp.status());
                match resp.text().await {
                    Ok(text) => {
                        println!("Text length: {}", text.len());
                        let end = std::cmp::min(1000, text.len());
                        println!("First {} chars: {}", end, &text[..end]);
                    }
                    Err(e) => println!("Text error: {:?}", e),
                }
            }
            Err(e) => println!("Send error: {:?}", e),
        }
    }

}

