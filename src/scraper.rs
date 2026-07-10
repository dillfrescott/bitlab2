use crate::stremio::Stream;
use crate::cinemeta::fetch_meta;
use serde::Deserialize;
use regex::Regex;
use scraper::{Html, Selector};

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

fn base32_to_hex(b32: &str) -> Option<String> {
    let mut bits = 0u64;
    let mut bit_count = 0;
    let mut bytes = Vec::new();
    
    for c in b32.chars() {
        if c == '=' {
            continue;
        }
        let val = match c.to_ascii_uppercase() {
            'A'..='Z' => c.to_ascii_uppercase() as u8 - b'A',
            '2'..='7' => c.to_ascii_uppercase() as u8 - b'2' + 26,
            _ => return None,
        };
        bits = (bits << 5) | (val as u64);
        bit_count += 5;
        if bit_count >= 8 {
            bytes.push((bits >> (bit_count - 8)) as u8);
            bit_count -= 8;
        }
    }
    
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{:02x}", b));
    }
    Some(hex)
}

fn normalize_info_hash(hash: &str) -> String {
    let cleaned = hash.trim();
    let hash_nopad = cleaned.replace('=', "");
    if hash_nopad.len() == 32 {
        if let Some(hex) = base32_to_hex(&hash_nopad) {
            return hex.to_lowercase();
        }
    }
    cleaned.to_lowercase()
}

fn build_magnet_url(info_hash: &str, display_name: &str) -> String {
    let normalized = normalize_info_hash(info_hash);
    let mut url = format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        normalized,
        urlencoding::encode(display_name)
    );
    for tracker in PUBLIC_TRACKERS {
        url.push_str("&tr=");
        url.push_str(urlencoding::encode(tracker).as_ref());
    }
    url
}

fn extract_hash_from_magnet(magnet: &str) -> Option<String> {
    let prefix = "magnet:?xt=urn:btih:";
    let magnet_lower = magnet.to_lowercase();
    if let Some(idx) = magnet_lower.find(prefix) {
        let hash_start = idx + prefix.len();
        let sub = &magnet[hash_start..];
        let hash_part = if let Some(end_idx) = sub.find('&') {
            &sub[..end_idx]
        } else {
            sub
        };
        return Some(normalize_info_hash(hash_part));
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
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
];

fn get_sources_for_torrent(info_hash: &str, _name: &str) -> Vec<String> {
    let mut sources = Vec::new();
    sources.push(format!("dht:{}", info_hash));
    for tracker in PUBLIC_TRACKERS {
        sources.push(format!("tracker:{}", tracker));
    }
    sources
}

fn extract_trackers_from_magnet(magnet: &str, info_hash: &str) -> Vec<String> {
    let mut sources = Vec::new();
    sources.push(format!("dht:{}", info_hash));
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

/// Scrapes a single YTS mirror
async fn scrape_single_yts(client: reqwest::Client, url: String) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client.get(&url).timeout(std::time::Duration::from_millis(2500));
    
    if let Ok(resp) = req.send().await {
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
                                
                                let sources = get_sources_for_torrent(&torrent.hash, &movie.title);
                                let _magnet = build_magnet_url(&torrent.hash, &movie.title);
                                streams.push(Stream {
                                    name: format!("[Bitlab] {}", quality),
                                    title: format!(
                                        "🎬 YTS: {}\n📦 {}\n{}\n⚡ Magnet (P2P Stream)",
                                        movie.title,
                                        torrent.size,
                                        peers_info
                                    ),
                                    url: None,
                                    info_hash: Some(normalize_info_hash(&torrent.hash)),
                                    file_idx: None,
                                    sources: Some(sources),
                                    behavior_hints: None,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    
    streams
}

/// Scrapes YTS for movie streams using its API (parallel mirrors)
pub async fn scrape_yts_movies(client: &reqwest::Client, imdb_id: &str) -> Vec<Stream> {
    let urls = vec![
        format!("https://movies-api.accel.li/api/v2/list_movies.json?query_term={}", imdb_id),
        format!("https://yts.mx/api/v2/list_movies.json?query_term={}", imdb_id),
    ];
    
    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client_clone = client.clone();
        set.spawn(async move {
            scrape_single_yts(client_clone, url).await
        });
    }
    
    let mut all_streams: Vec<Stream> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(streams) = res {
            for s in streams {
                if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                    all_streams.push(s);
                }
            }
        }
    }
    
    all_streams
}

/// Scrapes a single TPB Mirror HTML page
async fn scrape_single_tpb(
    client: reqwest::Client,
    url: String,
    provider_label: String,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_millis(2500));
        
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
        
        let sources = extract_trackers_from_magnet(magnet, &info_hash);
        let _normalized_magnet = build_magnet_url(&info_hash, &name);
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
            url: None,
            info_hash: Some(info_hash),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
    }
    
    streams
}

/// Scrapes TPB Proxy using HTML selectors with parallel mirror queries
pub async fn scrape_tpb_html(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let encoded_query = urlencoding::encode(query);
    let urls = vec![
        format!("https://tpb.party/search/{}/1/99/0", encoded_query),
        format!("https://thepiratebay10.org/search/{}/1/99/0", encoded_query),
        format!("https://thepiratebay0.org/search/{}/1/99/0", encoded_query),
    ];
    
    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client_clone = client.clone();
        let label_clone = provider_label.to_string();
        set.spawn(async move {
            scrape_single_tpb(client_clone, url, label_clone).await
        });
    }
    
    let mut all_streams: Vec<Stream> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(streams) = res {
            for s in streams {
                if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                    all_streams.push(s);
                }
            }
        }
    }
    
    all_streams
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
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_millis(2500));
        
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
        
        let sources = get_sources_for_torrent(&hash, &name);
        let _magnet = build_magnet_url(&hash, &name);
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
            url: None,
            info_hash: Some(normalize_info_hash(&hash)),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
    }
    
    streams
}

#[derive(Deserialize, Debug)]
struct BitsearchResponse {
    results: Option<Vec<BitsearchTorrent>>,
}

#[derive(Deserialize, Debug)]
struct BitsearchTorrent {
    title: Option<String>,
    infohash: Option<String>,
    size: Option<u64>,
    seeders: Option<u32>,
    leechers: Option<u32>,
}

/// Scrapes Bitsearch for torrents using its JSON API
pub async fn scrape_bitsearch(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let encoded_query = urlencoding::encode(query);
    let url = format!("https://bitsearch.to/api/v1/search?q={}&limit=50&sort=seeders", encoded_query);
    
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_millis(2500));
        
    let resp = match req.send().await {
        Ok(r) => r,
        Err(_) => return streams,
    };
    
    if !resp.status().is_success() {
        return streams;
    }

    let json_resp: BitsearchResponse = match resp.json().await {
        Ok(j) => j,
        Err(_) => return streams,
    };
    
    if let Some(results) = json_resp.results {
        for t in results {
            let title = t.title.unwrap_or_default();
            let infohash = t.infohash.unwrap_or_default();
            
            if title.is_empty() || infohash.is_empty() {
                continue;
            }
            
            let hash = infohash.to_lowercase();
            let quality = detect_quality(&title);
            
            let size_bytes = t.size.unwrap_or(0);
            let size_formatted = format_size(&size_bytes.to_string());
            
            let seeds = t.seeders.unwrap_or(0);
            if seeds == 0 {
                continue;
            }
            
            let peers = t.leechers.unwrap_or(0);
            
            let seeds_display = format!("{} seeders", seeds);
            let leechers_display = format!("{} peers", peers);
            
            let sources = get_sources_for_torrent(&hash, &title);
            streams.push(Stream {
                name: format!("[Bitlab] {}", quality),
                title: format!(
                    "🎬 {}: {}\n📦 {}\n👥 {} | 📥 {}\n⚡ Magnet (P2P Stream)",
                    provider_label,
                    title,
                    size_formatted,
                    seeds_display,
                    leechers_display
                ),
                url: None,
                info_hash: Some(normalize_info_hash(&hash)),
                file_idx: None,
                sources: Some(sources),
                behavior_hints: None,
            });
        }
    }
    
    streams
}

#[derive(Deserialize, Debug)]
struct SolidTorrentsResponse {
    results: Option<Vec<SolidTorrent>>,
}

#[derive(Deserialize, Debug)]
struct SolidTorrent {
    title: Option<String>,
    magnet: Option<String>,
    size: Option<u64>,
    swarm: Option<SolidSwarm>,
}

#[derive(Deserialize, Debug)]
struct SolidSwarm {
    seeders: Option<u32>,
    leechers: Option<u32>,
}

/// Scrapes a single SolidTorrents mirror
async fn scrape_single_solidtorrent(
    client: reqwest::Client,
    url: String,
    provider_label: String,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_millis(2500));
        
    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            if let Ok(json_resp) = resp.json::<SolidTorrentsResponse>().await {
                if let Some(results) = json_resp.results {
                    for t in results {
                        let title = t.title.unwrap_or_default();
                        let magnet = t.magnet.unwrap_or_default();
                        if title.is_empty() || magnet.is_empty() {
                            continue;
                        }
                        
                        let info_hash = match extract_hash_from_magnet(&magnet) {
                            Some(h) => h.to_lowercase(),
                            None => continue,
                        };
                        
                        let size_bytes = t.size.unwrap_or(0);
                        let size_formatted = format_size(&size_bytes.to_string());
                        
                        let swarm = t.swarm.unwrap_or(SolidSwarm { seeders: Some(0), leechers: Some(0) });
                        let seeds = swarm.seeders.unwrap_or(0);
                        let leechers = swarm.leechers.unwrap_or(0);
                        if seeds == 0 {
                            continue;
                        }
                        
                        let quality = detect_quality(&title);
                        let seeds_display = format!("{} seeders", seeds);
                        let leechers_display = format!("{} peers", leechers);
                        
                        let sources = extract_trackers_from_magnet(&magnet, &info_hash);
                        let _normalized_magnet = build_magnet_url(&info_hash, &title);
                        streams.push(Stream {
                            name: format!("[Bitlab] {}", quality),
                            title: format!(
                                "🎬 {}: {}\n📦 {}\n👥 {} | 📥 {}\n⚡ Magnet (P2P Stream)",
                                provider_label,
                                title,
                                size_formatted,
                                seeds_display,
                                leechers_display
                            ),
                            url: None,
                            info_hash: Some(info_hash),
                            file_idx: None,
                            sources: Some(sources),
                            behavior_hints: None,
                        });
                    }
                }
            }
        }
    }
    
    streams
}

/// Scrapes SolidTorrents search API with parallel mirror queries
pub async fn scrape_solidtorrents(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let encoded_query = urlencoding::encode(query);
    let urls = vec![
        format!("https://solidtorrents.to/api/v1/search?q={}&category=video&sort=seeders", encoded_query),
        format!("https://solidtorrents.net/api/v1/search?q={}&category=video&sort=seeders", encoded_query),
    ];

    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client_clone = client.clone();
        let label_clone = provider_label.to_string();
        set.spawn(async move {
            scrape_single_solidtorrent(client_clone, url, label_clone).await
        });
    }
    
    let mut all_streams: Vec<Stream> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(streams) = res {
            for s in streams {
                if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                    all_streams.push(s);
                }
            }
        }
    }
    
    all_streams
}

/// Scrapes a single Nyaa mirror
async fn scrape_single_nyaa(client: reqwest::Client, url: String) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_millis(2500));
        
    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            if let Ok(xml_text) = resp.text().await {
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
                    
                    let sources = get_sources_for_torrent(&hash, &title);
                    let _magnet = build_magnet_url(&hash, &title);
                    streams.push(Stream {
                        name: format!("[Bitlab] {}", quality),
                        title: format!(
                            "🌸 Nyaa: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Magnet (P2P Stream)",
                            title,
                            size,
                            seeders,
                            leechers
                        ),
                        url: None,
                        info_hash: Some(normalize_info_hash(&hash)),
                        file_idx: None,
                        sources: Some(sources),
                        behavior_hints: None,
                    });
                }
            }
        }
    }
    
    streams
}

/// Scrapes Nyaa RSS feed using standard regex XML matching with parallel mirror queries
pub async fn scrape_nyaa(client: &reqwest::Client, query: &str) -> Vec<Stream> {
    let encoded_query = urlencoding::encode(query);
    let urls = vec![
        format!("https://nyaa.si/?page=rss&c=1_2&q={}", encoded_query),
        format!("https://nyaa.land/?page=rss&c=1_2&q={}", encoded_query),
    ];
    
    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client_clone = client.clone();
        set.spawn(async move {
            scrape_single_nyaa(client_clone, url).await
        });
    }
    
    let mut all_streams: Vec<Stream> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(streams) = res {
            for s in streams {
                if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                    all_streams.push(s);
                }
            }
        }
    }
    
    all_streams
}

/// Scrapes a single EZTV mirror
async fn scrape_single_eztv(
    client: reqwest::Client,
    domain: String,
    imdb_id: String,
    target_season: u32,
    target_episode: u32,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let clean_imdb_id = imdb_id.strip_prefix("tt").unwrap_or(&imdb_id);
    
    // EZTV paginates at 30 per page — fetch up to 5 pages
    for page in 1..=5 {
        let url = format!(
            "{}/api/get-torrents?imdb_id={}&limit=30&page={}",
            domain, clean_imdb_id, page
        );
        
        let req = client.get(&url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .timeout(std::time::Duration::from_millis(3000));
            
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
            
            let sources = get_sources_for_torrent(&hash, &title);
            let _magnet = build_magnet_url(&hash, &title);
            streams.push(Stream {
                name: format!("[Bitlab] {}", quality),
                title: format!(
                    "📺 EZTV: {}\n📦 {}\n{}\n⚡ Magnet (P2P Stream)",
                    title,
                    size_formatted,
                    peers_info
                ),
                url: None,
                info_hash: Some(normalize_info_hash(&hash)),
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

/// Scrapes EZTV API for series episodes using IMDb ID with parallel mirror queries
pub async fn scrape_eztv(
    client: &reqwest::Client,
    imdb_id: &str,
    target_season: u32,
    target_episode: u32,
) -> Vec<Stream> {
    let domains = vec![
        "https://eztv.re".to_string(),
        "https://eztv.yt".to_string(),
        "https://eztv.ag".to_string(),
        "https://eztv.tf".to_string(),
        "https://eztv.wf".to_string(),
    ];
    
    let mut set = tokio::task::JoinSet::new();
    for domain in domains {
        let client_clone = client.clone();
        let imdb_clone = imdb_id.to_string();
        set.spawn(async move {
            scrape_single_eztv(client_clone, domain, imdb_clone, target_season, target_episode).await
        });
    }
    
    let mut all_streams: Vec<Stream> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(streams) = res {
            for s in streams {
                if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                    all_streams.push(s);
                }
            }
        }
    }
    
    all_streams
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
        std::time::Duration::from_millis(1500),
        meta_fut
    ).await;
    
    if let Ok(Some(meta)) = meta_res {
        let mut cache = meta_cache.write().await;
        let val = (meta.name, meta.year);
        cache.insert(imdb_id.to_string(), val.clone());
        Some(val)
    } else {
        None
    }
}

async fn check_if_anime_and_get_romaji(client: &reqwest::Client, english_title: &str, target_year: Option<&str>) -> (bool, Option<String>) {
    let encoded = urlencoding::encode(english_title);
    let url = format!("https://kitsu.io/api/edge/anime?filter[text]={}", encoded);
    let req = client.get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "application/vnd.api+json")
        .header("Content-Type", "application/vnd.api+json");
    
    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                    let target = english_title.to_lowercase();
                    let clean_target = clean_title(&target);
                    for item in data.iter().take(3) {
                        if let Some(attributes) = item.get("attributes") {
                            let mut is_match = false;
                            let mut romaji_title = None;
                            
                            if let Some(canonical) = attributes.get("canonicalTitle").and_then(|t| t.as_str()) {
                                if clean_title(&canonical.to_lowercase()) == clean_target {
                                    is_match = true;
                                }
                            }
                            
                            if let Some(titles) = attributes.get("titles") {
                                if let Some(en) = titles.get("en").and_then(|t| t.as_str()) {
                                    if clean_title(&en.to_lowercase()) == clean_target { is_match = true; }
                                }
                                if let Some(en_us) = titles.get("en_us").and_then(|t| t.as_str()) {
                                    if clean_title(&en_us.to_lowercase()) == clean_target { is_match = true; }
                                }
                                if let Some(en_jp) = titles.get("en_jp").and_then(|t| t.as_str()) {
                                    romaji_title = Some(en_jp.to_string());
                                    if clean_title(&en_jp.to_lowercase()) == clean_target { is_match = true; }
                                }
                            }
                            
                            if is_match {
                                if let Some(t_year_str) = target_year {
                                    let clean_t = t_year_str.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
                                    if let Ok(t_year) = clean_t.parse::<i32>() {
                                        if let Some(start_date) = attributes.get("startDate").and_then(|d| d.as_str()) {
                                            if let Some(year_str) = start_date.split('-').next() {
                                                if let Ok(k_year) = year_str.parse::<i32>() {
                                                    // Anime release dates can be slightly off, but Live Action is 24 years apart
                                                    if (t_year - k_year).abs() > 2 {
                                                        is_match = false;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            
                            if is_match {
                                let romaji_to_return = if let Some(rt) = &romaji_title {
                                    if clean_title(&rt.to_lowercase()) != clean_target {
                                        Some(rt.clone())
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                };
                                return (true, romaji_to_return);
                            }
                        }
                    }
                }
            }
        }
    }
    (false, None)
}

fn is_special_or_ova_mismatch(torrent_title: &str, show_name: &str, target_season: u32) -> bool {
    if target_season == 0 {
        return false; // specials are expected
    }
    
    let title_lower = torrent_title.to_lowercase();
    let show_lower = show_name.to_lowercase();
    
    // Disallowed special keywords
    let special_keywords = &[
        "ova", "oad", "special", "movie", "film", 
        "memory snow", "frozen bond", "hyouketsu no kizuna"
    ];
    
    for kw in special_keywords {
        if title_lower.contains(kw) && !show_lower.contains(kw) {
            return true;
        }
    }
    
    false
}

#[derive(Debug)]
struct TorrentInfo {
    seasons: Vec<u32>,
    episodes: Vec<u32>,
    is_pack: bool,
}

fn parse_torrent_info(title: &str) -> TorrentInfo {
    let title_lower = title.to_lowercase();
    let mut seasons = Vec::new();
    let mut episodes = Vec::new();
    let mut is_pack = false;

    // Remove common confusing numbers
    let mut cleaned_title = title_lower.clone();
    cleaned_title = cleaned_title.replace("1080p", " ")
                                 .replace("720p", " ")
                                 .replace("480p", " ")
                                 .replace("2160p", " ")
                                 .replace("10bit", " ")
                                 .replace("8bit", " ")
                                 .replace("x264", " ")
                                 .replace("x265", " ")
                                 .replace("h264", " ")
                                 .replace("h265", " ")
                                 .replace("5.1", " ")
                                 .replace("2.0", " ");

    // Remove CRC hashes (e.g. [a1b2c3d4])
    if let Ok(crc_re) = Regex::new(r"\[[0-9a-f]{8}\]") {
        cleaned_title = crc_re.replace_all(&cleaned_title, " ").to_string();
    }

    // Check for batch/pack keywords
    if cleaned_title.contains("complete") || cleaned_title.contains("batch") || cleaned_title.contains("pack") || cleaned_title.contains("season box") {
        is_pack = true;
    }

    // Pattern 1: SxxExx or Sxx.Exx or Sxx - Exx or SxxExx-xx
    if let Ok(sxx_exx_re) = Regex::new(r"s(\d+)\s*[e\.\-]\s*(\d+)(?:\s*\-\s*(\d+))?") {
        for cap in sxx_exx_re.captures_iter(&cleaned_title) {
            if let Ok(s) = cap[1].parse::<u32>() {
                if !seasons.contains(&s) { seasons.push(s); }
            }
            if let Ok(e1) = cap[2].parse::<u32>() {
                if let Some(e2_str) = cap.get(3) {
                    if let Ok(e2) = e2_str.as_str().parse::<u32>() {
                        is_pack = true;
                        for e in e1..=e2 {
                            if !episodes.contains(&e) { episodes.push(e); }
                        }
                    }
                } else {
                    if !episodes.contains(&e1) { episodes.push(e1); }
                }
            }
        }
    }

    // Pattern 2: sxx or season xx or seasonxx
    if let Ok(s_re) = Regex::new(r"\bs(?:eason)?\s*(\d+)\b") {
        for cap in s_re.captures_iter(&cleaned_title) {
            if let Ok(s) = cap[1].parse::<u32>() {
                if !seasons.contains(&s) { seasons.push(s); }
            }
        }
    }

    // Pattern 2b: ordinal season (e.g. "4th season", "2nd season", "3rd season")
    if let Ok(ord_s_re) = Regex::new(r"\b(\d+)(?:st|nd|rd|th)\s+season\b") {
        for cap in ord_s_re.captures_iter(&cleaned_title) {
            if let Ok(s) = cap[1].parse::<u32>() {
                if !seasons.contains(&s) { seasons.push(s); }
            }
        }
    }

    // Pattern 2c: part xx or cour xx (common in anime seasons)
    if let Ok(part_re) = Regex::new(r"\b(?:part|cour)\s*(\d+)\b") {
        for cap in part_re.captures_iter(&cleaned_title) {
            if let Ok(s) = cap[1].parse::<u32>() {
                if !seasons.contains(&s) { seasons.push(s); }
            }
        }
    }

    // Pattern 2d: Sxx-Sxx or Season x-y
    if let Ok(s_range_re) = Regex::new(r"\bs(?:easons?)?\s*(\d+)\s*(?:\-|\~|to)\s*(?:s(?:easons?)?\s*)?(\d+)\b") {
        for cap in s_range_re.captures_iter(&cleaned_title) {
            if let (Ok(s1), Ok(s2)) = (cap[1].parse::<u32>(), cap[2].parse::<u32>()) {
                if s1 < 100 && s2 < 100 && s1 < s2 {
                    is_pack = true;
                    for s in s1..=s2 {
                        if !seasons.contains(&s) { seasons.push(s); }
                    }
                }
            }
        }
    }

    // Pattern 3: xx x xx (e.g. 1x02, 01x02, 1x02-04)
    if let Ok(x_re) = Regex::new(r"\b(\d+)\s*x\s*(\d+)(?:\s*\-\s*(\d+))?\b") {
        for cap in x_re.captures_iter(&cleaned_title) {
            if let Ok(s) = cap[1].parse::<u32>() {
                if !seasons.contains(&s) { seasons.push(s); }
            }
            if let Ok(e1) = cap[2].parse::<u32>() {
                if let Some(e2_str) = cap.get(3) {
                    if let Ok(e2) = e2_str.as_str().parse::<u32>() {
                        is_pack = true;
                        for e in e1..=e2 {
                            if !episodes.contains(&e) { episodes.push(e); }
                        }
                    }
                } else {
                    if !episodes.contains(&e1) { episodes.push(e1); }
                }
            }
        }
    }

    // If we have seasons but no episodes yet, it's likely a season pack
    if !seasons.is_empty() && episodes.is_empty() {
        is_pack = true;
    }

    // Pattern 4: standalone episode numbers for single episode files (often preceded by " - " or " ep ")
    if episodes.is_empty() {
        if let Ok(ep_re) = Regex::new(r"(?:\-\s*|ep(?:isode)?\s*|e\s*|\[)(\d+)(?:v\d+)?(?:\]|\b)") {
            for cap in ep_re.captures_iter(&cleaned_title) {
                if let Ok(e) = cap[1].parse::<u32>() {
                    if e < 1000 {
                        if !episodes.contains(&e) { episodes.push(e); }
                    }
                }
            }
        }
    }

    // Pattern 5: episode ranges like "01-12", "01~12", "01 to 12"
    if let Ok(range_re) = Regex::new(r"\b(\d+)\s*(?:\-|\~|to)\s*(\d+)\b") {
        for cap in range_re.captures_iter(&cleaned_title) {
            if let (Ok(e1), Ok(e2)) = (cap[1].parse::<u32>(), cap[2].parse::<u32>()) {
                if e1 < 100 && e2 < 100 && e1 < e2 {
                    is_pack = true;
                    for e in e1..=e2 {
                        if !episodes.contains(&e) { episodes.push(e); }
                    }
                }
            }
        }
    }

    TorrentInfo { seasons, episodes, is_pack }
}

fn is_torrent_mismatch(torrent_title: &str, show_name: &str, romaji_name: Option<&str>, target_year: Option<&str>, is_series: bool) -> bool {
    let t_clean = clean_title(&torrent_title.to_lowercase());
    
    let mut base_title = t_clean.clone();
    if let Ok(re) = Regex::new(r"\b(s\d+e\d+|s\d+|ep?\d+|\d+x\d+|season\s*\d+|episode\s*\d+|19\d{2}|20\d{2}|1080p|720p|2160p|4k)\b") {
        if let Some(m) = re.find(&t_clean) {
            base_title = t_clean[..m.start()].trim().to_string();
        }
    }
    
    let ignore_words = ["the", "and", "for", "with", "from", "into", "upon", "a", "an", "of", "in", "to"];
    let s_clean = clean_title(&show_name.to_lowercase());
    let mut sig_words: Vec<String> = s_clean.split_whitespace().filter(|w| w.len() > 1 && !ignore_words.contains(w)).map(|w| w.to_string()).collect();
    
    if let Some(r_name) = romaji_name {
        let r_clean = clean_title(&r_name.to_lowercase());
        for w in r_clean.split_whitespace().filter(|w| w.len() > 1 && !ignore_words.contains(w)) {
            sig_words.push(w.to_string());
        }
    }
    
    if !sig_words.is_empty() {
        let base_words: Vec<&str> = base_title.split_whitespace().collect();
        let mut has_overlap = false;
        for sw in &sig_words {
            if base_words.contains(&sw.as_str()) {
                has_overlap = true;
                break;
            }
        }
        if !has_overlap {
            return true; 
        }
    }
    
    let check_spinoff = |name: &str| -> Option<bool> {
        let clean_name = clean_title(&name.to_lowercase());
        if let Some(idx) = t_clean.find(&clean_name) {
            let after_match = &t_clean[idx + clean_name.len()..];
            let words: Vec<&str> = after_match.split_whitespace().collect();
            if let Some(&w) = words.first() {
                if w.parse::<u32>().is_ok() { return Some(false); }
                let tags = [
                    "s", "e", "se", "ep", "season", "episode", "complete", "batch", "part", "pt", "vol", "volume",
                    "1080p", "720p", "2160p", "4k", "hd", "fhd", "uhd", "bluray", "blu", "ray", "brrip", "bdrip",
                    "web", "webrip", "webdl", "dvd", "dvdrip", "x264", "h264", "x265", "hevc", "10bit", "dual", "audio",
                    "sub", "subs", "dub", "dubbed", "eng", "english", "raw", "raws", "uncensored", "cen", "uncen",
                    "remux", "amzn", "nf", "dsnp", "hulu", "max", "tv", "movie", "film", "ova", "oad", "special",
                    "v2", "v3", "v4", "xvid", "divx", "aac", "flac", "mp3", "mkv", "mp4", "avi", "us", "uk", "jp",
                    "book", "ch", "chapter", "cour", "memory", "snow", "frozen", "bond", "hyouketsu", "kizuna"
                ];
                if tags.contains(&w) { return Some(false); }
                if let Ok(re) = Regex::new(r"^(?:s\d+|e\d+|ep\d+|s\d+e\d+|\d+x\d+|v\d+|\d+v\d+|ep\d+v\d+|\d+(?:st|nd|rd|th)|s\d+-\d+)$") {
                    if re.is_match(w) { return Some(false); }
                }
                return Some(true);
            }
        }
        None
    };

    if let Some(true) = check_spinoff(show_name) { return true; }
    if let Some(r_name) = romaji_name {
        if let Some(true) = check_spinoff(r_name) { return true; }
    }

    // Year check: if the torrent has a 19xx/20xx year, it must match the target year.
    if let Some(t_year_str) = target_year {
        let clean_t = t_year_str.chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
        if let Ok(t_y) = clean_t.parse::<i32>() {
            let mut found_any_year = false;
            let mut year_matches = false;
            if let Ok(re_year) = Regex::new(r"\b(19\d{2}|20\d{2})\b") {
                for cap in re_year.captures_iter(&t_clean) {
                    if let Ok(y) = cap[1].parse::<i32>() {
                        if y == 1920 { continue; } // ignore 1920x1080 resolution artifact
                        found_any_year = true;
                        if is_series {
                            if y >= t_y - 1 {
                                year_matches = true;
                            }
                        } else {
                            if (t_y - y).abs() <= 1 {
                                year_matches = true;
                            }
                        }
                    }
                }
            }
            if found_any_year && !year_matches {
                return true; 
            }
        }
    }

    false
}

fn verify_torrent_match(
    title: &str,
    show_name: &str,
    romaji_name: Option<&str>,
    target_year: Option<&str>,
    target_season: u32,
    target_episode: u32,
) -> bool {
    if is_torrent_mismatch(title, show_name, romaji_name, target_year, true) {
        return false;
    }

    if is_special_or_ova_mismatch(title, show_name, target_season) {
        return false;
    }

    let info = parse_torrent_info(title);

    if !info.seasons.is_empty() {
        if !info.seasons.contains(&target_season) {
            return false;
        }
    }

    // Season 1 protection: if target_season > 1 and the uploader didn't specify a season,
    // a standalone episode matching target_episode is highly likely Season 1.
    if target_season > 1 && info.seasons.is_empty() {
        if !info.is_pack && !info.episodes.is_empty() && info.episodes.contains(&target_episode) {
            return false;
        }
    }

    if info.is_pack {
        return true;
    }

    if !info.episodes.is_empty() {
        if !info.episodes.contains(&target_episode) {
            return false;
        }
    }

    true
}

fn extract_torrent_title(stream_title: &str) -> String {
    let first_line = stream_title.lines().next().unwrap_or("");
    let prefixes = &[
        "🌸 Nyaa: ",
        "🎬 TPB: ",
        "🎬 APIBay: ",
        "🎬 SolidTorrents: ",
        "🎬 Bitsearch: ",
        "📺 EZTV: ",
        "🎬 YTS: "
    ];
    let mut title = first_line.to_string();
    for prefix in prefixes {
        if title.starts_with(prefix) {
            title = title[prefix.len()..].to_string();
            break;
        }
    }
    title
}

enum ScraperTaskResult {
    Streams(Vec<Stream>),
    Meta(Option<(String, Option<String>)>),
}

/// Main entry point to get streams for a movie
pub async fn get_movie_streams(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>>,
    stream_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (Vec<Stream>, std::time::Instant)>>>,
    imdb_id: &str,
) -> Vec<Stream> {
    // Check stream cache (24 hours)
    {
        let cache = stream_cache.read().await;
        if let Some((streams, timestamp)) = cache.get(imdb_id) {
            if timestamp.elapsed().as_secs() < 86400 {
                println!("[INFO] Returning cached streams for movie: {}", imdb_id);
                return streams.clone();
            }
        }
    }

    println!("[INFO] Resolving streams for movie: {}", imdb_id);
    let start_time = std::time::Instant::now();
    
    let mut set: tokio::task::JoinSet<ScraperTaskResult> = tokio::task::JoinSet::new();

    // 1. Spawn ID-based scrapes immediately in parallel
    let client_yts = client.clone();
    let imdb_id_clone = imdb_id.to_string();
    set.spawn(async move {
        ScraperTaskResult::Streams(scrape_yts_movies(&client_yts, &imdb_id_clone).await)
    });

    let client_apibay_id = client.clone();
    let imdb_id_clone2 = imdb_id.to_string();
    set.spawn(async move {
        ScraperTaskResult::Streams(scrape_apibay(&client_apibay_id, &imdb_id_clone2, "APIBay").await)
    });

    // 2. Spawn metadata fetch concurrently
    let client_meta = client.clone();
    let meta_cache_clone = meta_cache.clone();
    let imdb_id_clone3 = imdb_id.to_string();
    set.spawn(async move {
        ScraperTaskResult::Meta(fetch_meta_cached(&client_meta, &meta_cache_clone, "movie", &imdb_id_clone3).await)
    });

    let mut all_streams: Vec<Stream> = Vec::new();
    let mut resolved_show_name: Option<String> = None;
    let mut resolved_romaji_name: Option<String> = None;
    let mut resolved_year: Option<String> = None;
    let mut meta_resolved = false;
    let timeout_dur = std::time::Duration::from_millis(6500);

    while !set.is_empty() {
        let elapsed = start_time.elapsed();
        if elapsed >= timeout_dur {
            break;
        }
        let remaining = timeout_dur - elapsed;

        match tokio::time::timeout(remaining, set.join_next()).await {
            Ok(Some(Ok(task_res))) => {
                match task_res {
                    ScraperTaskResult::Streams(streams) => {
                        for s in streams {
                            if let Some(show_name) = &resolved_show_name {
                                let torrent_title = extract_torrent_title(&s.title);
                                if is_torrent_mismatch(&torrent_title, show_name, resolved_romaji_name.as_deref(), resolved_year.as_deref(), false) {
                                    continue;
                                }
                            }
                            if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                                all_streams.push(s);
                            }
                        }
                    }
                    ScraperTaskResult::Meta(meta_res) => {
                        if !meta_resolved {
                            meta_resolved = true;
                            if let Some((name, year)) = meta_res {
                                resolved_show_name = Some(name.clone());
                                resolved_year = year.clone();
                                let cleaned = clean_title(&name);
                                
                                let (is_anime, romaji_opt) = check_if_anime_and_get_romaji(&client, &name, year.as_deref()).await;
                                resolved_romaji_name = romaji_opt.clone();
                                let cleaned_romaji = romaji_opt.map(|t| clean_title(&t));
                                let query = if let Some(yr) = &year {
                                    format!("{} {}", cleaned, yr)
                                } else {
                                    cleaned.clone()
                                };

                                // Spawn SolidTorrents, TPB title, and APIBay title searches immediately
                                let client_c = client.clone();
                                let query_solid = query.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_solidtorrents(&client_c, &query_solid, "SolidTorrents").await)
                                });

                                let client_c2 = client.clone();
                                let query_tpb = query.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_tpb_html(&client_c2, &query_tpb, "TPB").await)
                                });

                                let client_c3 = client.clone();
                                let query_apibay = query.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_apibay(&client_c3, &query_apibay, "APIBay").await)
                                });

                                let client_c_bit = client.clone();
                                let query_bit = query.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_bitsearch(&client_c_bit, &query_bit, "Bitsearch").await)
                                });

                                // Spawn Nyaa Anime search
                                let client_c4 = client.clone();
                                let query_nyaa = query.clone();
                                let year_clone = year.clone();
                                set.spawn(async move {
                                    let mut combined = Vec::new();
                                    if is_anime {
                                        let fut1 = scrape_nyaa(&client_c4, &query_nyaa);
                                        let fut2 = async {
                                            if let Some(q) = &cleaned_romaji {
                                                let mut q_with_yr = q.clone();
                                                if let Some(yr) = &year_clone {
                                                    q_with_yr = format!("{} {}", q, yr);
                                                }
                                                scrape_nyaa(&client_c4, &q_with_yr).await
                                            } else {
                                                Vec::new()
                                            }
                                        };
                                        let fut3 = scrape_nyaa(&client_c4, &cleaned);
                                        let fut4 = async {
                                            if let Some(q) = &cleaned_romaji {
                                                scrape_nyaa(&client_c4, q).await
                                            } else {
                                                Vec::new()
                                            }
                                        };
                                        let (res1, res2, res3, res4) = tokio::join!(fut1, fut2, fut3, fut4);
                                        combined = res1;
                                        for stream in res2 {
                                            if !combined.iter().any(|s| s.info_hash == stream.info_hash) {
                                                combined.push(stream);
                                            }
                                        }
                                        for stream in res3 {
                                            if !combined.iter().any(|s| s.info_hash == stream.info_hash) {
                                                combined.push(stream);
                                            }
                                        }
                                        for stream in res4 {
                                            if !combined.iter().any(|s| s.info_hash == stream.info_hash) {
                                                combined.push(stream);
                                            }
                                        }
                                    }
                                    ScraperTaskResult::Streams(combined)
                                });
                            }
                        }
                    }
                }
            }
            Ok(Some(Err(_))) => {}
            Ok(None) => break,
            Err(_) => break,
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
    torrent_files_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, Vec<TorrentFile>>>>,
    imdb_id: &str,
    season: u32,
    episode: u32,
) -> Vec<Stream> {
    let cache_key = format!("{}:{}:{}", imdb_id, season, episode);
    // Check stream cache (24 hours)
    {
        let cache = stream_cache.read().await;
        if let Some((streams, timestamp)) = cache.get(&cache_key) {
            if timestamp.elapsed().as_secs() < 86400 {
                println!("[INFO] Returning cached streams for series: {}", cache_key);
                return streams.clone();
            }
        }
    }

    println!("[INFO] Resolving streams for series: {} S{:02}E{:02}", imdb_id, season, episode);
    let start_time = std::time::Instant::now();
    
    let mut set: tokio::task::JoinSet<ScraperTaskResult> = tokio::task::JoinSet::new();

    // 1. Spawn EZTV ID search immediately
    let client_eztv = client.clone();
    let imdb_id_eztv = imdb_id.to_string();
    set.spawn(async move {
        ScraperTaskResult::Streams(scrape_eztv(&client_eztv, &imdb_id_eztv, season, episode).await)
    });
    
    // 2. Fetch metadata (cached or Cinemeta) concurrently
    let client_meta = client.clone();
    let meta_cache_clone = meta_cache.clone();
    let imdb_id_clone = imdb_id.to_string();
    set.spawn(async move {
        ScraperTaskResult::Meta(fetch_meta_cached(&client_meta, &meta_cache_clone, "series", &imdb_id_clone).await)
    });

    let mut all_streams: Vec<Stream> = Vec::new();
    let mut resolved_show_name: Option<String> = None;
    let mut resolved_romaji_name: Option<String> = None;
    let mut resolved_year: Option<String> = None;
    let mut meta_resolved = false;
    let timeout_dur = std::time::Duration::from_millis(6500);

    while !set.is_empty() {
        let elapsed = start_time.elapsed();
        if elapsed >= timeout_dur {
            break;
        }
        let remaining = timeout_dur - elapsed;

        match tokio::time::timeout(remaining, set.join_next()).await {
            Ok(Some(Ok(task_res))) => {
                match task_res {
                    ScraperTaskResult::Streams(streams) => {
                        for s in streams {
                            if let Some(show_name) = &resolved_show_name {
                                let torrent_title = extract_torrent_title(&s.title);
                                if !verify_torrent_match(&torrent_title, show_name, resolved_romaji_name.as_deref(), resolved_year.as_deref(), season, episode) {
                                    continue;
                                }
                            }
                            if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                                all_streams.push(s);
                            }
                        }
                    }
                    ScraperTaskResult::Meta(meta_res) => {
                        if !meta_resolved {
                            meta_resolved = true;
                            if let Some((name, year)) = meta_res {
                                resolved_show_name = Some(name.clone());
                                resolved_year = year.clone();
                                let cleaned = clean_title(&name);
                                
                                let (is_anime, romaji_opt) = check_if_anime_and_get_romaji(&client, &name, year.as_deref()).await;
                                resolved_romaji_name = romaji_opt.clone();
                                let cleaned_romaji = romaji_opt.map(|t| clean_title(&t));

                                // Format 1: "Show Name S01E01"
                                let query1 = format!("{} S{:02}E{:02}", cleaned, season, episode);
                                // Format 2: "Show Name 1x01"
                                let query2 = format!("{} {}x{:02}", cleaned, season, episode);
                                // Format 3: "Show Name Season 1"
                                let query3 = format!("{} Season {}", cleaned, season);
                                // Format 4: "Show Name S01"
                                let query4 = format!("{} S{:02}", cleaned, season);
                                // Format 5: Base name (for Complete Series packs)
                                let query5 = cleaned.clone();
                                // Format 6: Base name + "Complete"
                                let query6 = format!("{} Complete", cleaned);

                                let queries_to_run = vec![
                                    query1.clone(), query2.clone(), query3.clone(), 
                                    query4.clone(), query5.clone(), query6.clone()
                                ];
                                
                                for q in queries_to_run {
                                    let c_solid = client.clone();
                                    let q_solid = q.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(scrape_solidtorrents(&c_solid, &q_solid, "SolidTorrents").await)
                                    });

                                    let c_api = client.clone();
                                    let q_api = q.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(scrape_apibay(&c_api, &q_api, "APIBay").await)
                                    });

                                    let c_tpb = client.clone();
                                    let q_tpb = q.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(scrape_tpb_html(&c_tpb, &q_tpb, "TPB").await)
                                    });

                                    let c_bit = client.clone();
                                    let q_bit = q.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(scrape_bitsearch(&c_bit, &q_bit, "Bitsearch").await)
                                    });
                                }

                                // Spawn Nyaa (Anime) search
                                let client_c2 = client.clone();
                                let q1_nyaa = query1.clone();
                                set.spawn(async move {
                                    let q2_nyaa = format!("{} {:02}", cleaned, episode);
                                    let q3_nyaa = cleaned.clone();
                                    let q4_nyaa = format!("{} S{:02}", cleaned, season);
                                    
                                    let mut combined = Vec::new();
                                    if is_anime {
                                        let fut1 = scrape_nyaa(&client_c2, &q1_nyaa);
                                        let fut2 = scrape_nyaa(&client_c2, &q2_nyaa);
                                        let fut3 = async {
                                            if let Some(romaji) = &cleaned_romaji {
                                                scrape_nyaa(&client_c2, &format!("{} {:02}", romaji, episode)).await
                                            } else {
                                                Vec::new()
                                            }
                                        };
                                        let fut4 = async {
                                            if let Some(romaji) = &cleaned_romaji {
                                                scrape_nyaa(&client_c2, &format!("{} S{:02}E{:02}", romaji, season, episode)).await
                                            } else {
                                                Vec::new()
                                            }
                                        };
                                        let fut5 = scrape_nyaa(&client_c2, &q3_nyaa);
                                        let fut6 = async {
                                            if let Some(romaji) = &cleaned_romaji {
                                                scrape_nyaa(&client_c2, romaji).await
                                            } else {
                                                Vec::new()
                                            }
                                        };
                                        let fut7 = scrape_nyaa(&client_c2, &q4_nyaa);
                                        let fut8 = async {
                                            if let Some(romaji) = &cleaned_romaji {
                                                scrape_nyaa(&client_c2, &format!("{} S{:02}", romaji, season)).await
                                            } else {
                                                Vec::new()
                                            }
                                        };

                                        let (r1, r2, r3, r4, r5, r6, r7, r8) = tokio::join!(fut1, fut2, fut3, fut4, fut5, fut6, fut7, fut8);
                                        combined = r1;
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
                                        for s in r5 {
                                            if !combined.iter().any(|x| x.info_hash == s.info_hash) {
                                                combined.push(s);
                                            }
                                        }
                                        for s in r6 {
                                            if !combined.iter().any(|x| x.info_hash == s.info_hash) {
                                                combined.push(s);
                                            }
                                        }
                                        for s in r7 {
                                            if !combined.iter().any(|x| x.info_hash == s.info_hash) {
                                                combined.push(s);
                                            }
                                        }
                                        for s in r8 {
                                            if !combined.iter().any(|x| x.info_hash == s.info_hash) {
                                                combined.push(s);
                                            }
                                        }
                                    }
                                    ScraperTaskResult::Streams(combined)
                                });
                            }
                        }
                    }
                }
            }
            Ok(Some(Err(_))) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }

    // Sort by seeders descending
    all_streams.sort_by(|a, b| {
        let a_seeds = extract_seeds(&a.title);
        let b_seeds = extract_seeds(&b.title);
        b_seeds.cmp(&a_seeds)
    });

    // Resolve file indices for better accuracy
    resolve_file_indices(client, torrent_files_cache, &mut all_streams, season, episode).await;

    // Filter out streams that would play the wrong episode if file_idx is missing
    if episode > 1 {
        all_streams.retain(|s| {
            if s.file_idx.is_some() {
                return true;
            }
            let torrent_title = extract_torrent_title(&s.title);
            let info = parse_torrent_info(&torrent_title);
            if info.is_pack {
                return false;
            }
            if !info.episodes.contains(&episode) {
                return false;
            }
            true
        });
    }

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
    if let Some(idx) = title.find("👥 ") {
        let sub = &title[idx + "👥 ".len()..];
        if let Some(space_idx) = sub.find(' ') {
            return sub[..space_idx].parse::<u32>().unwrap_or(0);
        }
    }
    0
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct TorrentFile {
    pub path: String,
    pub size: u64,
    pub index: u32,
}

enum Bencode {
    Int(i64),
    ByteString(Vec<u8>),
    List(Vec<Bencode>),
    Dict(std::collections::BTreeMap<Vec<u8>, Bencode>),
}

fn parse_bencode(data: &[u8], pos: &mut usize) -> Option<Bencode> {
    if *pos >= data.len() {
        return None;
    }
    match data[*pos] {
        b'i' => {
            *pos += 1;
            let start = *pos;
            while *pos < data.len() && data[*pos] != b'e' {
                *pos += 1;
            }
            if *pos >= data.len() {
                return None;
            }
            let s = std::str::from_utf8(&data[start..*pos]).ok()?;
            let val = s.parse::<i64>().ok()?;
            *pos += 1; // skip 'e'
            Some(Bencode::Int(val))
        }
        b'l' => {
            *pos += 1;
            let mut list = Vec::new();
            while *pos < data.len() && data[*pos] != b'e' {
                list.push(parse_bencode(data, pos)?);
            }
            if *pos >= data.len() {
                return None;
            }
            *pos += 1; // skip 'e'
            Some(Bencode::List(list))
        }
        b'd' => {
            *pos += 1;
            let mut dict = std::collections::BTreeMap::new();
            while *pos < data.len() && data[*pos] != b'e' {
                let key = match parse_bencode(data, pos)? {
                    Bencode::ByteString(b) => b,
                    _ => return None,
                };
                let val = parse_bencode(data, pos)?;
                dict.insert(key, val);
            }
            if *pos >= data.len() {
                return None;
            }
            *pos += 1; // skip 'e'
            Some(Bencode::Dict(dict))
        }
        b'0'..=b'9' => {
            let start = *pos;
            while *pos < data.len() && data[*pos] != b':' {
                *pos += 1;
            }
            if *pos >= data.len() {
                return None;
            }
            let len_str = std::str::from_utf8(&data[start..*pos]).ok()?;
            let len = len_str.parse::<usize>().ok()?;
            *pos += 1; // skip ':'
            if *pos + len > data.len() {
                return None;
            }
            let bytes = data[*pos..*pos + len].to_vec();
            *pos += len;
            Some(Bencode::ByteString(bytes))
        }
        _ => None,
    }
}

fn parse_torrent_bytes(bytes: &[u8]) -> Option<Vec<TorrentFile>> {
    let mut pos = 0;
    let bencode = parse_bencode(bytes, &mut pos)?;
    let dict = match bencode {
        Bencode::Dict(d) => d,
        _ => return None,
    };
    let info = match dict.get(b"info".as_ref())? {
        Bencode::Dict(d) => d,
        _ => return None,
    };
    
    let mut files_list = Vec::new();
    if let Some(files_val) = info.get(b"files".as_ref()) {
        let files = match files_val {
            Bencode::List(l) => l,
            _ => return None,
        };
        for (idx, file_val) in files.iter().enumerate() {
            let file_dict = match file_val {
                Bencode::Dict(d) => d,
                _ => continue,
            };
            let length = match file_dict.get(b"length".as_ref()) {
                Some(Bencode::Int(l)) => *l as u64,
                _ => 0,
            };
            let path_val = match file_dict.get(b"path".as_ref()) {
                Some(Bencode::List(l)) => l,
                _ => continue,
            };
            let mut path_parts = Vec::new();
            for part in path_val {
                if let Bencode::ByteString(b) = part {
                    if let Ok(s) = std::str::from_utf8(b) {
                        path_parts.push(s.to_string());
                    }
                }
            }
            if !path_parts.is_empty() {
                let path = path_parts.join("/");
                files_list.push(TorrentFile {
                    path,
                    size: length,
                    index: idx as u32,
                });
            }
        }
    } else {
        let name_bytes = match info.get(b"name".as_ref())? {
            Bencode::ByteString(b) => b,
            _ => return None,
        };
        let name = std::str::from_utf8(name_bytes).ok()?.to_string();
        let length = match info.get(b"length".as_ref()) {
            Some(Bencode::Int(l)) => *l as u64,
            _ => 0,
        };
        files_list.push(TorrentFile {
            path: name,
            size: length,
            index: 0,
        });
    }
    Some(files_list)
}

async fn fetch_torrent_files_list(
    client: &reqwest::Client,
    info_hash: &str,
) -> Option<Vec<TorrentFile>> {
    let info_hash_upper = info_hash.to_uppercase();
    let urls = vec![
        format!("https://itorrents.net/torrent/{}.torrent", info_hash_upper),
        format!("https://itorrents.org/torrent/{}.torrent", info_hash_upper),
        format!("https://torrage.info/torrent.php?h={}", info_hash_upper),
        format!("https://btcache.me/torrent/{}", info_hash_upper),
    ];
    
    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client_clone = client.clone();
        set.spawn(async move {
            let req = client_clone.get(&url)
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                .timeout(std::time::Duration::from_millis(2000));
            if let Ok(resp) = req.send().await {
                if resp.status().is_success() {
                    if let Some(content_type) = resp.headers().get("content-type") {
                        if let Ok(ct_str) = content_type.to_str() {
                            if ct_str.contains("html") {
                                return None;
                            }
                        }
                    }
                    if let Ok(bytes) = resp.bytes().await {
                        if let Some(files) = parse_torrent_bytes(&bytes) {
                            return Some(files);
                        }
                    }
                }
            }
            None
        });
    }
    
    while let Some(res) = set.join_next().await {
        if let Ok(Some(files)) = res {
            return Some(files);
        }
    }
    None
}

fn parse_episode_from_filename(filename: &str) -> Option<(Option<u32>, u32)> {
    let lower = filename.to_lowercase();
    let mut cleaned = lower.clone();
    
    // Remove version indicators attached to digits, e.g. "01v2" -> "01"
    if let Ok(re) = Regex::new(r"(\d+)v\d+") {
        cleaned = re.replace_all(&cleaned, "$1").to_string();
    }
    
    // Remove resolution specs, codecs, and audio channels
    cleaned = cleaned.replace("1080p", " ")
                     .replace("720p", " ")
                     .replace("480p", " ")
                     .replace("2160p", " ")
                     .replace("10bit", " ")
                     .replace("8bit", " ")
                     .replace("x264", " ")
                     .replace("x265", " ")
                     .replace("h264", " ")
                     .replace("h265", " ")
                     .replace("h.264", " ")
                     .replace("h.265", " ")
                     .replace("5.1", " ")
                     .replace("7.1", " ")
                     .replace("2.0", " ");
                     
    if let Ok(re) = Regex::new(r"\[[0-9a-f]{8}\]") {
        cleaned = re.replace_all(&cleaned, " ").to_string();
    }
    if let Ok(re) = Regex::new(r"\b(19\d{2}|20\d{2})\b") {
        cleaned = re.replace_all(&cleaned, " ").to_string();
    }

    if let Ok(re) = Regex::new(r"\bseason\s*(\d+)\s*(?:episode|ep|e|-)?\s*(\d+)\b") {
        if let Some(cap) = re.captures(&cleaned) {
            let s = cap[1].parse::<u32>().ok();
            let e = cap[2].parse::<u32>().ok()?;
            return Some((s, e));
        }
    }
    if let Ok(re) = Regex::new(r"s(\d+)\s*[e\.\-]\s*(\d+)") {
        if let Some(cap) = re.captures(&cleaned) {
            let s = cap[1].parse::<u32>().ok();
            let e = cap[2].parse::<u32>().ok()?;
            return Some((s, e));
        }
    }
    if let Ok(re) = Regex::new(r"\b(\d+)\s*x\s*(\d+)\b") {
        if let Some(cap) = re.captures(&cleaned) {
            let s = cap[1].parse::<u32>().ok();
            let e = cap[2].parse::<u32>().ok()?;
            return Some((s, e));
        }
    }
    if let Ok(re) = Regex::new(r"\bep(?:isode)?\s*(\d+)\b") {
        if let Some(cap) = re.captures(&cleaned) {
            let e = cap[1].parse::<u32>().ok()?;
            return Some((None, e));
        }
    }
    if let Ok(re) = Regex::new(r"(?:^|[\s\-\_\[\(\.])(\d+)(?:[\s\-\_\]\)\.]|$)") {
        for cap in re.captures_iter(&cleaned) {
            if let Ok(e) = cap[1].parse::<u32>() {
                if e > 0 && e < 1000 {
                    return Some((None, e));
                }
            }
        }
    }
    None
}

fn parse_season_from_path(path: &str) -> Option<u32> {
    let lower = path.to_lowercase();
    let components: Vec<&str> = lower.split(|c| c == '/' || c == '\\').collect();
    if components.len() > 1 {
        for folder in components.iter().take(components.len() - 1).rev() {
            if let Ok(re) = Regex::new(r"\bs(?:eason)?\s*(\d+)\b") {
                if let Some(cap) = re.captures(folder) {
                    if let Ok(s) = cap[1].parse::<u32>() {
                        return Some(s);
                    }
                }
            }
            if let Ok(re) = Regex::new(r"\b(\d+)(?:st|nd|rd|th)\s+season\b") {
                if let Some(cap) = re.captures(folder) {
                    if let Ok(s) = cap[1].parse::<u32>() {
                        return Some(s);
                    }
                }
            }
            if let Ok(re) = Regex::new(r"^\s*(\d+)\s*$") {
                if let Some(cap) = re.captures(folder) {
                    if let Ok(s) = cap[1].parse::<u32>() {
                        if s > 0 && s < 100 {
                            return Some(s);
                        }
                    }
                }
            }
        }
    }
    None
}

fn is_file_match(
    file_path: &str,
    target_season: u32,
    target_episode: u32,
    torrent_info: &TorrentInfo,
) -> bool {
    let lower_path = file_path.to_lowercase();
    let is_video = lower_path.ends_with(".mkv")
        || lower_path.ends_with(".mp4")
        || lower_path.ends_with(".avi")
        || lower_path.ends_with(".mov")
        || lower_path.ends_with(".wmv")
        || lower_path.ends_with(".flv")
        || lower_path.ends_with(".webm")
        || lower_path.ends_with(".m4v")
        || lower_path.ends_with(".mpg")
        || lower_path.ends_with(".mpeg");
    if !is_video {
        return false;
    }
    
    // Ignore samples
    if lower_path.contains("sample") {
        return false;
    }

    // If target_season is not 0 (specials season), ignore files that look like specials/OVAs/extras/NC
    if target_season > 0 {
        let ignore_keywords = [
            "nced", "ncop", "ost", "soundtrack", "bonus", "extras", "extra", 
            "special", "ova", "preview", "trailer", "recap", "interview"
        ];
        for kw in ignore_keywords {
            if lower_path.contains(kw) {
                return false;
            }
        }
    }

    let filename = lower_path.split(|c| c == '/' || c == '\\').last().unwrap_or(&lower_path);
    if let Some((file_season_opt, file_episode)) = parse_episode_from_filename(filename) {
        if file_episode == target_episode {
            let season = file_season_opt.or_else(|| parse_season_from_path(&lower_path));
            match season {
                Some(s) => {
                    return s == target_season;
                }
                None => {
                    if !torrent_info.seasons.is_empty() {
                        return torrent_info.seasons.contains(&target_season);
                    }
                    return target_season == 1;
                }
            }
        }
    }
    false
}

pub async fn resolve_file_indices(
    client: &reqwest::Client,
    torrent_files_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, Vec<TorrentFile>>>>,
    streams: &mut [Stream],
    season: u32,
    episode: u32,
) {
    let mut set = tokio::task::JoinSet::new();
    
    for (idx, stream) in streams.iter().enumerate().take(12) {
        if let Some(ref hash) = stream.info_hash {
            let torrent_title = extract_torrent_title(&stream.title);
            let info = parse_torrent_info(&torrent_title);
            if true { // always try to resolve file indices for better accuracy
                let client = client.clone();
                let cache = torrent_files_cache.clone();
                let hash = hash.clone();
                
                set.spawn(async move {
                    let cached_files = {
                        let cache_read = cache.read().await;
                        cache_read.get(&hash).cloned()
                    };
                    
                    let files = match cached_files {
                        Some(f) => Some(f),
                        None => {
                            if let Some(f) = fetch_torrent_files_list(&client, &hash).await {
                                let mut cache_write = cache.write().await;
                                cache_write.insert(hash.clone(), f.clone());
                                Some(f)
                            } else {
                                None
                            }
                        }
                    };
                    
                    let mut file_idx = None;
                    let mut matched_filename = None;
                    if let Some(ref files_list) = files {
                        for file in files_list {
                            if is_file_match(&file.path, season, episode, &info) {
                                file_idx = Some(file.index);
                                matched_filename = Some(file.path.clone());
                                break;
                            }
                        }
                    }
                    
                    (idx, file_idx, matched_filename, hash)
                });
            }
        }
    }
    
    let resolve_timeout = std::time::Duration::from_millis(3500);
    let _ = tokio::time::timeout(resolve_timeout, async {
        while let Some(res) = set.join_next().await {
            if let Ok((idx, file_idx, matched_filename, hash)) = res {
                if let Some(f_idx) = file_idx {
                    streams[idx].file_idx = Some(f_idx);
                    if let Some(fname) = matched_filename {
                        streams[idx].behavior_hints = Some(crate::stremio::BehaviorHints {
                            not_video: None,
                            proxy_headers: None,
                            binge_group: Some(format!("bitlab|{}", hash)),
                            filename: Some(fname),
                        });
                    }
                }
            }
        }
    }).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_torrent_match() {
        let show = "Re:Zero kara Hajimeru Isekai Seikatsu";
        
        // 1. OVA / Special mismatch
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - Memory Snow - 02 [1080p].mkv", show, None, None, 1, 2));
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - Memory Snow - 02 [1080p].mkv", show, None, None, 0, 2));
        
        // 2. Correct season and episode
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02 [1080p].mkv", show, None, None, 1, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 12 [1080p].mkv", show, None, None, 1, 2));
        
        // 3. Explicit season
        assert!(verify_torrent_match("[Erai-raws] Re:Zero S1 - 02 [1080p].mkv", show, None, None, 1, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero S2 - 02 [1080p].mkv", show, None, None, 1, 2));
        
        // 4. Batch/Pack verification
        assert!(verify_torrent_match("[SubsPlease] Re:Zero S1 Complete [1080p]", show, None, None, 1, 2));
        assert!(!verify_torrent_match("[SubsPlease] Re:Zero S2 Complete [1080p]", show, None, None, 1, 2));
        
        // 5. Versioning
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02v2 [1080p].mkv", show, None, None, 1, 2));

        // 6. Ordinal seasons
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 2nd Season - 02 [1080p].mkv", show, None, None, 2, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 2nd Season - 02 [1080p].mkv", show, None, None, 1, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 3rd Season - 02 [1080p].mkv", show, None, None, 1, 2));

        // 7. Season 1 protection: S1 Episode 2 should not match S2 Episode 2
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02 [1080p].mkv", show, None, None, 2, 2));
    }

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

    #[test]
    fn test_normalize_info_hash() {
        assert_eq!(
            normalize_info_hash("1588987DB4C7D98F74FB436AD8FEDE1CBE9F1F63"),
            "1588987db4c7d98f74fb436ad8fede1cbe9f1f63"
        );
        assert_eq!(
            normalize_info_hash("WRN7ZT6NKMA6SSXYKAFRUGDDIFJUNKI2"),
            "b45bfccfcd5301e94af8500b1a1863415346a91a"
        );
        assert_eq!(
            normalize_info_hash("WRN7ZT6NKMA6SSXYKAFRUGDDIFJUNKI2==="),
            "b45bfccfcd5301e94af8500b1a1863415346a91a"
        );
        assert_eq!(
            extract_hash_from_magnet("magnet:?xt=urn:btih:1588987db4c7d98f74fb436ad8fede1cbe9f1f63&dn=Test"),
            Some("1588987db4c7d98f74fb436ad8fede1cbe9f1f63".to_string())
        );
        assert_eq!(
            extract_hash_from_magnet("magnet:?xt=urn:btih:WRN7ZT6NKMA6SSXYKAFRUGDDIFJUNKI2&dn=Test"),
            Some("b45bfccfcd5301e94af8500b1a1863415346a91a".to_string())
        );
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

    #[test]
    fn test_parse_episode_from_filename() {
        assert_eq!(parse_episode_from_filename("Bocchi the Rock! - S01E01.mkv"), Some((Some(1), 1)));
        assert_eq!(parse_episode_from_filename("02.mp4"), Some((None, 2)));
        assert_eq!(parse_episode_from_filename("[SubsPlease] Bocchi the Rock! - 12 (1080p).mkv"), Some((None, 12)));
        assert_eq!(parse_episode_from_filename("Bocchi the Rock! - Ep 05.mkv"), Some((None, 5)));
        assert_eq!(parse_episode_from_filename("2x03.mkv"), Some((Some(2), 3)));
        assert_eq!(parse_episode_from_filename("Clarksons Farm Season 1 Episode 2.mkv"), Some((Some(1), 2)));
        assert_eq!(parse_episode_from_filename("Clarksons Farm Season 4 Episode 05.mkv"), Some((Some(4), 5)));
        assert_eq!(parse_episode_from_filename("Season 1 - 02.mkv"), Some((Some(1), 2)));
        assert_eq!(parse_episode_from_filename("The.Chosen.I.Have.Called.You.By.Name.1080p.WEB-DL.DDP5.1.H.264-NTb.mkv"), None);
    }

    #[test]
    fn test_parse_season_from_path() {
        assert_eq!(parse_season_from_path("Season 2/01.mkv"), Some(2));
        assert_eq!(parse_season_from_path("S3/01.mkv"), Some(3));
        assert_eq!(parse_season_from_path("2nd Season/01.mkv"), Some(2));
        assert_eq!(parse_season_from_path("Bocchi the Rock/01.mkv"), None);
    }
}
