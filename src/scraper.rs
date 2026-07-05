use crate::stremio::Stream;
use crate::cinemeta::fetch_meta;
use serde::Deserialize;

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

const PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.coppersurfer.tk:6969/announce",
    "udp://glotorrents.pw:6969/announce",
    "udp://tracker.leechers-paradise.org:6969/announce",
    "udp://tracker.cyberia.is:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://ipv4.tracker.harry.lu:80/announce",
];

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
    let url = format!("https://movies-api.accel.li/api/v2/list_movies.json?query_term={}", imdb_id);
    
    if let Ok(resp) = client.get(&url).send().await {
        if let Ok(json_resp) = resp.json::<YtsSearchResponse>().await {
            if json_resp.status == "ok" && json_resp.data.is_some() {
                let data = json_resp.data.unwrap();
                if let Some(movies) = data.movies {
                    for movie in movies {
                        if let Some(torrents) = movie.torrents {
                            for torrent in torrents {
                                let magnet = build_magnet_url(&torrent.hash, &movie.title);
                                let quality = format!("{} ({})", torrent.quality, torrent.r#type.to_uppercase());
                                
                                let peers_info = if torrent.seeds == 0 {
                                    "👥 Active (YTS Swarm)".to_string()
                                } else {
                                    format!("👥 {} seeders | 📥 {} peers", torrent.seeds, torrent.peers)
                                };
                                
                                streams.push(Stream {
                                    name: format!("[🚀 AG] {}", quality),
                                    title: format!(
                                        "🎬 YTS: {}\n📦 {}\n{}\n⚡ Magnet (P2P Stream)",
                                        movie.title,
                                        torrent.size,
                                        peers_info
                                    ),
                                    url: Some(magnet),
                                    info_hash: Some(torrent.hash.clone().to_lowercase()),
                                    file_idx: Some(0),
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
        
        let leechers_str = match row.select(&col7_selector).next() {
            Some(el) => el.text().collect::<Vec<_>>().concat(),
            None => "0".to_string(),
        };
        let leechers = leechers_str.trim().parse::<u32>().unwrap_or(0);
        
        let quality = detect_quality(&name);
        
        streams.push(Stream {
            name: format!("[🚀 AG] {}", quality),
            title: format!(
                "🎬 {}: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Magnet (P2P Stream)",
                provider_label,
                name,
                size,
                seeds,
                leechers
            ),
            url: Some(magnet.to_string()),
            info_hash: Some(info_hash),
            file_idx: Some(0),
            behavior_hints: None,
        });
    }
    
    streams
}

/// Main entry point to get streams for a movie
pub async fn get_movie_streams(client: &reqwest::Client, imdb_id: &str) -> Vec<Stream> {
    println!("[INFO] Resolving streams for movie: {}", imdb_id);
    let start_time = std::time::Instant::now();
    
    let client_yts = client.clone();
    let client_tpb = client.clone();
    let imdb_id_clone = imdb_id.to_string();
    
    let yts_fut = scrape_yts_movies(&client_yts, &imdb_id_clone);
    let tpb_fut = async {
        // Search HTML TPB Proxy with the IMDb ID
        let mut results = scrape_tpb_html(&client_tpb, &imdb_id_clone, "TPB").await;
        
        // If no results, lookup metadata to search by name and release year
        if results.is_empty() {
            println!("[INFO] No direct IMDb ID results on TPB for {}. Querying Cinemeta fallback...", imdb_id_clone);
            if let Some((name, year)) = fetch_meta(&client_tpb, "movie", &imdb_id_clone).await {
                let cleaned = clean_title(&name);
                let query = if let Some(yr) = year {
                    format!("{} {}", cleaned, yr)
                } else {
                    cleaned
                };
                println!("[INFO] Querying TPB HTML for movie by title search: \"{}\"", query);
                results = scrape_tpb_html(&client_tpb, &query, "TPB").await;
            }
        }
        results
    };
    
    // Concurrently run YTS with 2.0s timeout and TPB with 3.5s timeout
    let (yts_res, tpb_res) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_millis(2000), yts_fut),
        tokio::time::timeout(std::time::Duration::from_millis(3500), tpb_fut)
    );
    
    let mut all_streams = Vec::new();
    
    match yts_res {
        Ok(streams) => {
            println!("[INFO] YTS scraper finished. Found {} streams.", streams.len());
            all_streams.extend(streams);
        }
        Err(_) => {
            println!("[WARN] YTS scraper timed out (2.0s limit reached, likely ISP blocked).");
        }
    }
    
    match tpb_res {
        Ok(streams) => {
            println!("[INFO] TPB scraper finished. Found {} streams.", streams.len());
            all_streams.extend(streams);
        }
        Err(_) => {
            println!("[WARN] TPB scraper timed out (3.5s limit reached).");
        }
    }
    
    // Sort streams: put high-seeding streams first
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
    
    all_streams
}

/// Main entry point to get streams for a series episode
pub async fn get_series_streams(
    client: &reqwest::Client,
    imdb_id: &str,
    season: u32,
    episode: u32,
) -> Vec<Stream> {
    println!("[INFO] Resolving streams for series: {} S{:02}E{:02}", imdb_id, season, episode);
    let start_time = std::time::Instant::now();
    
    let client_clone = client.clone();
    let imdb_id_clone = imdb_id.to_string();
    
    let resolve_fut = async {
        let mut streams = Vec::new();
        if let Some((name, _)) = fetch_meta(&client_clone, "series", &imdb_id_clone).await {
            let cleaned = clean_title(&name);
            
            // Search query format: "Show Name S01E01"
            let query1 = format!("{} S{:02}E{:02}", cleaned, season, episode);
            // Fallback search query format: "Show Name 1x01"
            let query2 = format!("{} {}x{:02}", cleaned, season, episode);
            
            println!("[INFO] Searching TPB HTML for series episode: \"{}\" and \"{}\"", query1, query2);
            
            let client1 = client_clone.clone();
            let client2 = client_clone.clone();
            
            let (res1, res2) = tokio::join!(
                scrape_tpb_html(&client1, &query1, "TPB"),
                scrape_tpb_html(&client2, &query2, "TPB")
            );
            
            println!("[INFO] TPB HTML SxxExx results: {} streams. TPB HTML XxXX results: {} streams.", res1.len(), res2.len());
            streams.extend(res1);
            
            // Deduplicate if we find duplicate hashes
            for stream in res2 {
                if !streams.iter().any(|s| s.info_hash == stream.info_hash) {
                    streams.push(stream);
                }
            }
        } else {
            println!("[WARN] Failed to fetch series metadata from Cinemeta for {}", imdb_id_clone);
        }
        streams
    };
    
    // Wrap the entire resolution in a 3.5-second timeout
    let result = tokio::time::timeout(std::time::Duration::from_millis(3500), resolve_fut).await;
    let mut all_streams = result.unwrap_or_default();
    
    // Sort streams by seeders
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
    
    all_streams
}

fn extract_seeds(title: &str) -> u32 {
    // Extract seeders number from formatted title string: "👥 X seeders | 📥 Y peers"
    if let Some(idx) = title.find("👥 ") {
        let sub = &title[idx + 4..];
        if let Some(space_idx) = sub.find(' ') {
            return sub[..space_idx].parse::<u32>().unwrap_or(0);
        }
    }
    0
}
