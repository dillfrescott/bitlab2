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
    "udp://tracker.moeking.me:6969/announce",
    "udp://explodie.org:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.cyberia.is:6969/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://tracker.internetwarriors.net:1337/announce",
    "udp://tracker.dump.cl:6969/announce",
    "udp://tracker.ipv6tracker.ru:80/announce",
    "udp://movies.zack.si:80/announce",
    "udp://tracker.bittor.co:80/announce",
];

fn get_sources_for_torrent(_info_hash: &str, _name: &str) -> Vec<String> {
    let mut sources = Vec::new();
    for tracker in PUBLIC_TRACKERS {
        sources.push(format!("tracker:{}", tracker));
    }
    sources
}

fn extract_trackers_from_magnet(magnet: &str) -> Vec<String> {
    let mut sources = Vec::new();
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
                                let magnet = build_magnet_url(&torrent.hash, &movie.title);
                                streams.push(Stream {
                                    name: format!("[Bitlab] {}", quality),
                                    title: format!(
                                        "🎬 YTS: {}\n📦 {}\n{}\n⚡ Magnet (P2P Stream)",
                                        movie.title,
                                        torrent.size,
                                        peers_info
                                    ),
                                    url: Some(magnet),
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
        
        let sources = extract_trackers_from_magnet(magnet);
        let normalized_magnet = build_magnet_url(&info_hash, &name);
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
            url: Some(normalized_magnet),
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
        let magnet = build_magnet_url(&hash, &name);
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
            info_hash: Some(normalize_info_hash(&hash)),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
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
                        
                        let sources = extract_trackers_from_magnet(&magnet);
                        let normalized_magnet = build_magnet_url(&info_hash, &title);
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
                            url: Some(normalized_magnet),
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
                    let magnet = build_magnet_url(&hash, &title);
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
            let magnet = build_magnet_url(&hash, &title);
            streams.push(Stream {
                name: format!("[Bitlab] {}", quality),
                title: format!(
                    "📺 EZTV: {}\n📦 {}\n{}\n⚡ Magnet (P2P Stream)",
                    title,
                    size_formatted,
                    peers_info
                ),
                url: Some(magnet),
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

fn verify_torrent_match(
    title: &str,
    show_name: &str,
    target_season: u32,
    target_episode: u32,
) -> bool {
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
    let mut meta_resolved = false;
    let timeout_dur = std::time::Duration::from_millis(3500);

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
                            if !all_streams.iter().any(|x| x.info_hash == s.info_hash) {
                                all_streams.push(s);
                            }
                        }
                    }
                    ScraperTaskResult::Meta(meta_res) => {
                        if !meta_resolved {
                            meta_resolved = true;
                            if let Some((name, year)) = meta_res {
                                let cleaned = clean_title(&name);
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

                                // Spawn Nyaa Anime search (first resolves Kitsu concurrently)
                                let client_c4 = client.clone();
                                let name_clone = name.clone();
                                let query_nyaa = query.clone();
                                let year_clone = year.clone();
                                set.spawn(async move {
                                    let kitsu_title = fetch_kitsu_romaji_title(&client_c4, &name_clone).await;
                                    let cleaned_romaji = kitsu_title.map(|t| clean_title(&t));
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
                                    let (res1, res2) = tokio::join!(fut1, fut2);
                                    let mut combined = res1;
                                    for stream in res2 {
                                        if !combined.iter().any(|s| s.info_hash == stream.info_hash) {
                                            combined.push(stream);
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
    let mut meta_resolved = false;
    let timeout_dur = std::time::Duration::from_millis(3500);

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
                                if !verify_torrent_match(&torrent_title, show_name, season, episode) {
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
                            if let Some((name, _year)) = meta_res {
                                resolved_show_name = Some(name.clone());
                                let cleaned = clean_title(&name);

                                // Format 1: "Show Name S01E01"
                                let query1 = format!("{} S{:02}E{:02}", cleaned, season, episode);
                                // Format 2: "Show Name 1x01"
                                let query2 = format!("{} {}x{:02}", cleaned, season, episode);

                                // Spawn SolidTorrents, APIBay, TPB searches immediately
                                let client_c = client.clone();
                                
                                let c_solid1 = client_c.clone();
                                let q1_solid = query1.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_solidtorrents(&c_solid1, &q1_solid, "SolidTorrents").await)
                                });

                                let c_solid2 = client_c.clone();
                                let q2_solid = query2.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_solidtorrents(&c_solid2, &q2_solid, "SolidTorrents").await)
                                });

                                let c_api1 = client_c.clone();
                                let q1_api = query1.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_apibay(&c_api1, &q1_api, "APIBay").await)
                                });

                                let c_api2 = client_c.clone();
                                let q2_api = query2.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_apibay(&c_api2, &q2_api, "APIBay").await)
                                });

                                let c_tpb1 = client_c.clone();
                                let q1_tpb = query1.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_tpb_html(&c_tpb1, &q1_tpb, "TPB").await)
                                });

                                let c_tpb2 = client_c.clone();
                                let q2_tpb = query2.clone();
                                set.spawn(async move {
                                    ScraperTaskResult::Streams(scrape_tpb_html(&c_tpb2, &q2_tpb, "TPB").await)
                                });

                                // Spawn Nyaa (Anime) search
                                let client_c2 = client.clone();
                                let name_clone = name.clone();
                                let q1_nyaa = query1.clone();
                                set.spawn(async move {
                                    let kitsu_title = fetch_kitsu_romaji_title(&client_c2, &name_clone).await;
                                    let cleaned_romaji = kitsu_title.map(|t| clean_title(&t));

                                    let q2_nyaa = format!("{} {:02}", cleaned, episode);
                                    
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_torrent_match() {
        let show = "Re:Zero kara Hajimeru Isekai Seikatsu";
        
        // 1. OVA / Special mismatch
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - Memory Snow - 02 [1080p].mkv", show, 1, 2));
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - Memory Snow - 02 [1080p].mkv", show, 0, 2));
        
        // 2. Correct season and episode
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02 [1080p].mkv", show, 1, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 12 [1080p].mkv", show, 1, 2));
        
        // 3. Explicit season
        assert!(verify_torrent_match("[Erai-raws] Re:Zero S1 - 02 [1080p].mkv", show, 1, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero S2 - 02 [1080p].mkv", show, 1, 2));
        
        // 4. Batch/Pack verification
        assert!(verify_torrent_match("[SubsPlease] Re:Zero S1 Complete [1080p]", show, 1, 2));
        assert!(!verify_torrent_match("[SubsPlease] Re:Zero S2 Complete [1080p]", show, 1, 2));
        
        // 5. Versioning
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02v2 [1080p].mkv", show, 1, 2));

        // 6. Ordinal seasons
        assert!(verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 2nd Season - 02 [1080p].mkv", show, 2, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 2nd Season - 02 [1080p].mkv", show, 1, 2));
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 3rd Season - 02 [1080p].mkv", show, 1, 2));

        // 7. Season 1 protection: S1 Episode 2 should not match S2 Episode 2
        assert!(!verify_torrent_match("[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02 [1080p].mkv", show, 2, 2));
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
}
