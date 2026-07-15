use crate::cinemeta::fetch_meta;
use crate::stremio::Stream;
use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tokio::sync::RwLock;

type AnimeCacheValue = ((bool, Option<String>), std::time::Instant);
type AniZipCacheValue = (Option<u32>, std::time::Instant);

static ANIME_CACHE: OnceLock<RwLock<HashMap<String, AnimeCacheValue>>> = OnceLock::new();
static ANIZIP_CACHE: OnceLock<RwLock<HashMap<String, AniZipCacheValue>>> = OnceLock::new();

pub const STREAM_CACHE_TTL_SECS: u64 = 3600; // 1 hour cache duration
const AUX_CACHE_SUCCESS_TTL_SECS: u64 = 6 * 3600;
const AUX_CACHE_FAILURE_TTL_SECS: u64 = 5 * 60;
const AUX_CACHE_MAX_ITEMS: usize = 5000;

fn get_anime_cache() -> &'static RwLock<HashMap<String, AnimeCacheValue>> {
    ANIME_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn get_anizip_cache() -> &'static RwLock<HashMap<String, AniZipCacheValue>> {
    ANIZIP_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

async fn check_if_anime_and_get_romaji_cached(
    client: &reqwest::Client,
    english_title: &str,
    target_year: Option<&str>,
) -> (bool, Option<String>) {
    let cache_key = format!("{}:{:?}", english_title.to_lowercase(), target_year);
    {
        let cache = get_anime_cache().read().await;
        if let Some((cached, timestamp)) = cache.get(&cache_key) {
            let ttl = if cached.0 {
                AUX_CACHE_SUCCESS_TTL_SECS
            } else {
                AUX_CACHE_FAILURE_TTL_SECS
            };
            if timestamp.elapsed().as_secs() < ttl {
                return cached.clone();
            }
        }
    }

    let res = check_if_anime_and_get_romaji(client, english_title, target_year).await;

    {
        let mut cache = get_anime_cache().write().await;
        if cache.len() >= AUX_CACHE_MAX_ITEMS {
            cache.clear();
        }
        cache.insert(cache_key, (res.clone(), std::time::Instant::now()));
    }

    res
}

async fn fetch_anizip_absolute_episode_cached(
    client: &reqwest::Client,
    imdb_or_kitsu: &str,
    episode: u32,
) -> Option<u32> {
    let cache_key = format!("{}:{}", imdb_or_kitsu, episode);
    {
        let cache = get_anizip_cache().read().await;
        if let Some((cached, timestamp)) = cache.get(&cache_key) {
            let ttl = if cached.is_some() {
                AUX_CACHE_SUCCESS_TTL_SECS
            } else {
                AUX_CACHE_FAILURE_TTL_SECS
            };
            if timestamp.elapsed().as_secs() < ttl {
                return *cached;
            }
        }
    }

    let res = fetch_anizip_absolute_episode(client, imdb_or_kitsu, episode).await;

    {
        let mut cache = get_anizip_cache().write().await;
        if cache.len() >= AUX_CACHE_MAX_ITEMS {
            cache.clear();
        }
        cache.insert(cache_key, (res, std::time::Instant::now()));
    }

    res
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct AniZipResponse {
    pub episodes: HashMap<String, AniZipEpisode>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AniZipEpisode {
    pub absolute_episode_number: Option<u32>,
}

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

#[allow(dead_code)]
const PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
];

// Helper to convert base32 info hashes to hex
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

/// Returns true if the info hash is the all-zeros sentinel some torrent indexes
/// (notably APIBay) use to mark a "no real torrent" / placeholder row.
fn is_zero_info_hash(hash: &str) -> bool {
    let h = hash.trim();
    !h.is_empty() && h.chars().all(|c| c == '0')
}

fn is_valid_info_hash(hash: &str) -> bool {
    let normalized = normalize_info_hash(hash);
    normalized.len() == 40
        && normalized.chars().all(|c| c.is_ascii_hexdigit())
        && !is_zero_info_hash(&normalized)
}

/// Extracts the start (premiere) year from a metadata year string that may be a
/// range (e.g. "2005-2008" or "2005–2008") or a single year ("2023").
/// Returns None if no 4-digit year is found. Used to build search queries so we
/// never send a raw range like "2005-2008" to an index (which matches nothing).
fn extract_start_year(year: &str) -> Option<String> {
    let year_re = Regex::new(r"\b(19\d{2}|20\d{2})\b").unwrap();
    year_re.captures(year).map(|c| c[1].to_string())
}

/// Returns true if an APIBay torrent row represents a real, indexable torrent
/// (i.e. not the "No results returned" sentinel and not an all-zeros hash).
fn is_apibay_result_valid(name: &str, info_hash: &str) -> bool {
    !name.is_empty()
        && !info_hash.is_empty()
        && name != "No results returned"
        && name != "No results found"
        && is_valid_info_hash(info_hash)
}

#[allow(dead_code)]
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

fn get_sources_for_torrent(info_hash: &str) -> Vec<String> {
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

// -----------------------------------------------------------------------------
// Quality and Metadata Parsing
// -----------------------------------------------------------------------------
pub struct TorrentQualityMeta {
    pub quality: String,
    pub details: Vec<String>,
}

pub fn parse_quality_meta(name: &str) -> TorrentQualityMeta {
    let lower = name.to_lowercase();

    let quality = if lower.contains("2160p") || lower.contains("4k") || lower.contains("uhd") {
        "4K"
    } else if lower.contains("1080p") || lower.contains("fhd") || lower.contains("1080i") {
        "1080p"
    } else if lower.contains("720p") || lower.contains("hdtv") {
        "720p"
    } else if lower.contains("480p") || lower.contains("576p") {
        "480p"
    } else {
        "SD"
    };

    let mut details = Vec::new();

    // Codecs
    if lower.contains("x265") || lower.contains("h265") || lower.contains("hevc") {
        details.push("x265".to_string());
    } else if lower.contains("x264") || lower.contains("h264") || lower.contains("avc") {
        details.push("x264".to_string());
    }

    // Audio Layouts
    if lower.contains("7.1") || lower.contains("truehd") || lower.contains("atmos") {
        details.push("7.1".to_string());
    } else if lower.contains("5.1")
        || lower.contains("dd5")
        || lower.contains("dts")
        || lower.contains("ac3")
    {
        details.push("5.1".to_string());
    }

    // HDR/Dolby Vision
    if lower.contains("hdr") {
        details.push("HDR".to_string());
    }
    if lower.contains("dv") || lower.contains("dolby vision") || lower.contains("vision") {
        details.push("DV".to_string());
    }

    // Audio Languages
    if lower.contains("dual")
        || lower.contains("dual-audio")
        || lower.contains("multi")
        || lower.contains("dubbed")
    {
        details.push("Dual-Audio".to_string());
    }

    TorrentQualityMeta {
        quality: quality.to_string(),
        details,
    }
}

// -----------------------------------------------------------------------------
// Filename Parser & Matcher
// -----------------------------------------------------------------------------
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ParsedFilename {
    pub base_title: String,
    pub seasons: Vec<u32>,
    pub episodes: Vec<u32>,
    pub year: Option<u32>,
    pub resolution: Option<String>,
    pub is_pack: bool,
}

pub fn parse_seasons_episodes(title: &str, title_hints: &[&str]) -> (Vec<u32>, Vec<u32>, bool) {
    let is_valid_episode = |e: u32| -> bool {
        e != 1080
            && e != 720
            && e != 2160
            && e != 480
            && e != 576
            && e != 360
            && !(e >= 1900 && e <= 2099)
    };

    let mut seasons = Vec::new();
    let mut episodes = Vec::new();
    let mut is_pack = false;
    let title_lower = title.to_lowercase();

    // 1. Check for batch/complete keywords
    if title_lower.contains("complete")
        || title_lower.contains("batch")
        || title_lower.contains("pack")
        || title_lower.contains("season box")
        || title_lower.contains("seasons")
        || title_lower.contains("collection")
    {
        is_pack = true;
    }

    // 2. Pattern: S01E01-E08 or S01E01-08 or S01E01_E08 or S01E01_08 or S01E01-S01E08
    let sxx_exx_range =
        Regex::new(r"s(\d+)\s*e(\d+)\s*(?:\-|to|~|_)\s*(?:s\d+\s*)?e?(\d+)\b").unwrap();
    for cap in sxx_exx_range.captures_iter(&title_lower) {
        if let (Ok(s), Ok(e1), Ok(e2)) = (
            cap[1].parse::<u32>(),
            cap[2].parse::<u32>(),
            cap[3].parse::<u32>(),
        ) {
            if !seasons.contains(&s) {
                seasons.push(s);
            }
            if e1 < e2 && e2 - e1 < 100 {
                is_pack = true;
                for e in e1..=e2 {
                    if is_valid_episode(e) && !episodes.contains(&e) {
                        episodes.push(e);
                    }
                }
            }
        }
    }

    // 3. Pattern: S01E01 or S01.E01 or S01_E01 or S01-E01 or S01E01E02 (multi-episode)
    let sxx_exx = Regex::new(r"s(\d+)\s*(?:e|ep|\.e?p?|[\-_](?:e|p)+)\s*(\d+)").unwrap();
    for cap in sxx_exx.captures_iter(&title_lower) {
        if let (Ok(s), Ok(e)) = (cap[1].parse::<u32>(), cap[2].parse::<u32>()) {
            if !seasons.contains(&s) {
                seasons.push(s);
            }
            if is_valid_episode(e) && !episodes.contains(&e) {
                episodes.push(e);
            }
        }
    }

    // Pattern: S01E01E02E03. The general SxxExx expression above only
    // captures the first episode, so collect every chained E marker here.
    let chained_sxx_exx = Regex::new(r"s(\d+)(?:\s*e\d+){2,}").unwrap();
    let chained_season = Regex::new(r"^s(\d+)").unwrap();
    let chained_episode = Regex::new(r"e(\d+)").unwrap();
    for full_match in chained_sxx_exx.find_iter(&title_lower) {
        let matched = full_match.as_str();
        if let Some(season_cap) = chained_season.captures(matched) {
            if let Ok(season) = season_cap[1].parse::<u32>() {
                if !seasons.contains(&season) {
                    seasons.push(season);
                }
            }
        }
        for episode_cap in chained_episode.captures_iter(matched) {
            if let Ok(episode) = episode_cap[1].parse::<u32>() {
                if is_valid_episode(episode) && !episodes.contains(&episode) {
                    episodes.push(episode);
                }
            }
        }
        is_pack = true;
    }

    // 4. Pattern: 1x01-08 or 1x01-1x08
    let x_range =
        Regex::new(r"\b(\d+)\s*x\s*(\d+)\s*(?:\-|to|~|_)\s*(?:\d+\s*x\s*)?(\d+)\b").unwrap();
    for cap in x_range.captures_iter(&title_lower) {
        if let (Ok(s), Ok(e1), Ok(e2)) = (
            cap[1].parse::<u32>(),
            cap[2].parse::<u32>(),
            cap[3].parse::<u32>(),
        ) {
            if !seasons.contains(&s) {
                seasons.push(s);
            }
            if e1 < e2 && e2 - e1 < 100 {
                is_pack = true;
                for e in e1..=e2 {
                    if is_valid_episode(e) && !episodes.contains(&e) {
                        episodes.push(e);
                    }
                }
            }
        }
    }

    // 5. Pattern: 1x01 or 01x02
    let x_pattern = Regex::new(r"\b(\d+)\s*x\s*(\d+)\b").unwrap();
    for cap in x_pattern.captures_iter(&title_lower) {
        if let (Ok(s), Ok(e)) = (cap[1].parse::<u32>(), cap[2].parse::<u32>()) {
            if !seasons.contains(&s) {
                seasons.push(s);
            }
            if is_valid_episode(e) && !episodes.contains(&e) {
                episodes.push(e);
            }
        }
    }

    // 6. Pattern: S01-S03 or Season 1-3 or Season 1 to 3
    let s_range_prefix =
        Regex::new(r"\bs(?:easons?)?\s*(\d+)\s*(?:\-|\~|to|_)\s*s(?:easons?)?\s*(\d+)\b").unwrap();
    for cap in s_range_prefix.captures_iter(&title_lower) {
        if let (Ok(s1), Ok(s2)) = (cap[1].parse::<u32>(), cap[2].parse::<u32>()) {
            if s1 < s2 && s2 - s1 < 50 {
                is_pack = true;
                for s in s1..=s2 {
                    if !seasons.contains(&s) {
                        seasons.push(s);
                    }
                }
            }
        }
    }

    let s_range_nospace = Regex::new(r"\bs(?:easons?)?\s*(\d+)(?:\-|\~|to)(\d+)\b").unwrap();
    for cap in s_range_nospace.captures_iter(&title_lower) {
        if let (Ok(s1), Ok(s2)) = (cap[1].parse::<u32>(), cap[2].parse::<u32>()) {
            if s1 < s2 && s2 - s1 < 50 {
                is_pack = true;
                for s in s1..=s2 {
                    if !seasons.contains(&s) {
                        seasons.push(s);
                    }
                }
            }
        }
    }

    // 7. Pattern: Season 1 or S01
    let s_pattern = Regex::new(r"\b(?:s|season)\s*(\d+)\b").unwrap();
    for cap in s_pattern.captures_iter(&title_lower) {
        if let Ok(s) = cap[1].parse::<u32>() {
            if !seasons.contains(&s) {
                seasons.push(s);
            }
        }
    }

    // 8. Pattern: 2nd Season or 4th Season
    let ord_season = Regex::new(r"\b(\d+)(?:st|nd|rd|th)\s+season\b").unwrap();
    for cap in ord_season.captures_iter(&title_lower) {
        if let Ok(s) = cap[1].parse::<u32>() {
            if !seasons.contains(&s) {
                seasons.push(s);
            }
        }
    }

    // 9. Pattern: Ep 01 or Episode 01 or E01
    let ep_pattern = Regex::new(r"\b(?:ep|episode|e)\s*(\d+)\b").unwrap();
    for cap in ep_pattern.captures_iter(&title_lower) {
        if let Ok(e) = cap[1].parse::<u32>() {
            if is_valid_episode(e) && !episodes.contains(&e) {
                episodes.push(e);
            }
        }
    }

    // Packs often advertise a bare episode span after the season marker,
    // e.g. "Season 01 1-12 Complete".
    if is_pack || !seasons.is_empty() {
        let bare_episode_range =
            Regex::new(r"(?:^|[^a-z0-9])(\d+)\s*(?:-|to|~)\s*(\d+)\b").unwrap();
        for cap in bare_episode_range.captures_iter(&title_lower) {
            let range_start = cap.get(1).map(|m| m.start()).unwrap_or(0);
            if title_lower[..range_start].trim_end().ends_with("season") {
                continue;
            }
            if let (Ok(start), Ok(end)) = (cap[1].parse::<u32>(), cap[2].parse::<u32>()) {
                if start > 0
                    && start <= end
                    && end - start < 100
                    && is_valid_episode(start)
                    && is_valid_episode(end)
                {
                    for episode in start..=end {
                        if !episodes.contains(&episode) {
                            episodes.push(episode);
                        }
                    }
                    is_pack = true;
                }
            }
        }
    }

    // 10. Standalone episode number fallback (excluding digits belonging to season, year or resolution)
    let mut ep_clean = title_lower.clone();

    // Remove season markers and ranges
    let s_range_prefix_re =
        Regex::new(r"\bs(?:easons?)?\s*\d+\s*(?:\-|\~|to|_)\s*s(?:easons?)?\s*\d+\b").unwrap();
    ep_clean = s_range_prefix_re.replace_all(&ep_clean, " ").to_string();

    let s_range_nospace_re = Regex::new(r"\bs(?:easons?)?\s*\d+(?:\-|\~|to)\d+\b").unwrap();
    ep_clean = s_range_nospace_re.replace_all(&ep_clean, " ").to_string();

    let s_pattern_re = Regex::new(r"\b(?:s|season)\s*\d+\b").unwrap();
    ep_clean = s_pattern_re.replace_all(&ep_clean, " ").to_string();

    let ord_season_re = Regex::new(r"\b\d+(?:st|nd|rd|th)\s+season\b").unwrap();
    ep_clean = ord_season_re.replace_all(&ep_clean, " ").to_string();

    // Remove year markers
    let year_re = Regex::new(r"\b(19\d{2}|20\d{2})\b").unwrap();
    ep_clean = year_re.replace_all(&ep_clean, " ").to_string();

    // Remove resolution markers
    let res_re = Regex::new(r"\b(2160p|1080p|720p|480p|576p|360p|4k|8k|1080i)\b").unwrap();
    ep_clean = res_re.replace_all(&ep_clean, " ").to_string();

    // Remove codec markers (x264, x265, h264, h265, hevc, etc.)
    let codec_re = Regex::new(r"\b(?:x|h)?26[45]\b|\bhevc\b|\bav1\b").unwrap();
    ep_clean = codec_re.replace_all(&ep_clean, " ").to_string();

    // Remove audio channel markers (5.1, 7.1, 2.0, etc.)
    let audio_re = Regex::new(r"\d\.\d").unwrap();
    ep_clean = audio_re.replace_all(&ep_clean, " ").to_string();

    // Remove bit depth markers (10bit, 8bit, etc.)
    let bit_re = Regex::new(r"\b\d+bits?\b").unwrap();
    ep_clean = bit_re.replace_all(&ep_clean, " ").to_string();

    // Remove version markers (v1, v2, v3, etc.)
    let version_re = Regex::new(r"v\d+\b").unwrap();
    ep_clean = version_re.replace_all(&ep_clean, " ").to_string();

    if episodes.is_empty() {
        // Only treat a leading number, "- 02", or "[02]" as a standalone
        // episode. Matching every number also mistakes dates and group tags for
        // episodes.
        let number_re = Regex::new(r"(?:^|\-\s*|\[)(\d+)(?:\b|\])").unwrap();
        for cap in number_re.captures_iter(&ep_clean) {
            if let Ok(n) = cap[1].parse::<u32>() {
                if n > 0 && n < 10000 && is_valid_episode(n) {
                    // Check if this number is part of any of the target title hints
                    let mut is_part_of_hint = false;
                    for hint in title_hints {
                        if !hint.is_empty() {
                            let hint_lower = hint.to_lowercase();
                            let hint_num_pattern = format!(r"\b{}\b", n);
                            if let Ok(hint_re) = Regex::new(&hint_num_pattern) {
                                if hint_re.is_match(&hint_lower) {
                                    is_part_of_hint = true;
                                    break;
                                }
                            }
                        }
                    }
                    if is_part_of_hint {
                        continue;
                    }

                    if !episodes.contains(&n) {
                        episodes.push(n);
                    }
                }
            }
        }
    }

    if !seasons.is_empty() && episodes.is_empty() {
        is_pack = true;
    }

    (seasons, episodes, is_pack)
}

pub fn parse_filename(filename: &str, title_hints: &[&str]) -> ParsedFilename {
    let lower = filename.to_lowercase();

    // 1. Extract seasons, episodes, pack status
    let (seasons, episodes, is_pack) = parse_seasons_episodes(filename, title_hints);

    // 2. Extract year
    let mut year = None;
    let year_re = Regex::new(r"\b(19\d{2}|20\d{2})\b").unwrap();
    for cap in year_re.captures_iter(&lower) {
        if let Ok(y) = cap[1].parse::<u32>() {
            if y != 1080 && y != 720 && y != 2160 && y != 480 && y != 576 {
                year = Some(y);
                break;
            }
        }
    }

    // 3. Extract resolution
    let mut resolution = None;
    let res_tags = [
        ("2160p", "4K"),
        ("4k", "4K"),
        ("uhd", "4K"),
        ("1080p", "1080p"),
        ("fhd", "1080p"),
        ("1080i", "1080p"),
        ("720p", "720p"),
        ("hd", "720p"),
        ("480p", "480p"),
        ("sd", "SD"),
        ("576p", "576p"),
    ];
    for &(tag, label) in &res_tags {
        if lower.contains(tag) {
            resolution = Some(label.to_string());
            break;
        }
    }

    // 4. Split and extract base title
    let split_keywords = [
        "2160p", "1080p", "720p", "480p", "576p", "360p", "bluray", "blu-ray", "webdl", "web-dl",
        "webrip", "hdtv", "x264", "x265", "h264", "h265", "hevc", "10bit", "8bit", "complete",
        "batch", "pack", "season", "episode", "multi", "dual",
    ];

    let mut split_idx = filename.len();

    let find_earliest_regex = |re_str: &str, current_min: &mut usize| {
        if let Ok(re) = Regex::new(re_str) {
            if let Some(m) = re.find(&lower) {
                if m.start() < *current_min {
                    *current_min = m.start();
                }
            }
        }
    };

    find_earliest_regex(r"\bs\d+", &mut split_idx);
    find_earliest_regex(r"\b\d+x\d+", &mut split_idx);
    find_earliest_regex(r"\bseason\b", &mut split_idx);
    find_earliest_regex(r"\bepisode\b", &mut split_idx);
    find_earliest_regex(r"\bep\d+", &mut split_idx);
    find_earliest_regex(r"\be\d+", &mut split_idx);
    find_earliest_regex(r"\b\d+v\d+\b", &mut split_idx);

    if let Some(m) = year_re.find(&lower) {
        let y_val = m.as_str().parse::<u32>().unwrap_or(0);
        if y_val != 1080 && y_val != 720 && y_val != 2160 {
            if m.start() < split_idx {
                split_idx = m.start();
            }
        }
    }

    for kw in &split_keywords {
        let kw_pattern = format!(r"\b{}\b", kw);
        if let Ok(re) = Regex::new(&kw_pattern) {
            if let Some(m) = re.find(&lower) {
                if m.start() < split_idx {
                    split_idx = m.start();
                }
            }
        }
    }

    // Recognize standalone numbers as split points
    let number_re = Regex::new(r"(?:\-\s*|\[|\b)(\d+)(?:\b|\])").unwrap();
    for cap in number_re.captures_iter(&lower) {
        if let Some(whole_match) = cap.get(0) {
            if let Ok(n) = cap[1].parse::<u32>() {
                if n != 1080
                    && n != 720
                    && n != 2160
                    && n != 480
                    && n != 360
                    && n != 576
                    && !(n >= 1900 && n <= 2099)
                    && n > 0
                    && n < 10000
                {
                    // Check if this number is part of any of the target title hints
                    let mut is_part_of_hint = false;
                    for hint in title_hints {
                        if !hint.is_empty() {
                            let hint_lower = hint.to_lowercase();
                            let hint_num_pattern = format!(r"\b{}\b", n);
                            if let Ok(hint_re) = Regex::new(&hint_num_pattern) {
                                if hint_re.is_match(&hint_lower) {
                                    is_part_of_hint = true;
                                    break;
                                }
                            }
                        }
                    }
                    if is_part_of_hint {
                        continue;
                    }

                    // Check if this is NOT a season number (preceded by s/season)
                    let start = whole_match.start();
                    let before = &lower[..start];
                    let is_season = before.ends_with('s')
                        || before.ends_with("s ")
                        || before.ends_with("season")
                        || before.ends_with("season ");
                    if !is_season {
                        if start < split_idx {
                            split_idx = start;
                        }
                    }
                }
            }
        }
    }

    let raw_prefix = &filename[..split_idx];
    let mut cleaned_prefix = raw_prefix.trim();
    while cleaned_prefix.starts_with('[') {
        if let Some(end_idx) = cleaned_prefix.find(']') {
            cleaned_prefix = cleaned_prefix[end_idx + 1..].trim();
        } else {
            break;
        }
    }

    // Clean leading/trailing non-alphanumeric chars
    let base_title = cleaned_prefix
        .trim_matches(|c: char| !c.is_alphanumeric())
        .replace('.', " ")
        .replace('_', " ")
        .trim()
        .to_string();

    ParsedFilename {
        base_title,
        seasons,
        episodes,
        year,
        resolution,
        is_pack,
    }
}

pub fn to_compact_title(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

pub fn clean_title(title: &str) -> String {
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

fn is_allowed_extra_word(w: &str) -> bool {
    if w.len() <= 1 {
        return true;
    }
    // Check if it's numeric
    if w.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }

    // Check if it's an ordinal (e.g. 1st, 2nd, 3rd, 4th)
    if w.ends_with("st") || w.ends_with("nd") || w.ends_with("rd") || w.ends_with("th") {
        let prefix = &w[..w.len() - 2];
        if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }

    // Check roman numerals
    let roman_numerals = ["i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x"];
    if roman_numerals.contains(&w) {
        return true;
    }

    let allowed = [
        // Media format/release
        "season",
        "seasons",
        "series",
        "complete",
        "pack",
        "boxset",
        "box",
        "set",
        "collection",
        "anthology",
        "volume",
        "vol",
        "part",
        "pt",
        "book",
        "chapters",
        "chapter",
        "saga",
        "arc",
        "cour",
        "show",
        "tv",
        "movie",
        "film",
        "ova",
        "ona",
        "special",
        "specials",
        "bonus",
        "extras",
        "extra",
        "recap",
        "trailer",
        "teaser",
        "episode",
        "episodes",
        // Version/edition
        "edition",
        "versions",
        "version",
        "cut",
        "uncut",
        "extended",
        "remastered",
        "restored",
        "unrated",
        "rated",
        "censored",
        "uncensored",
        "directors",
        "director",
        "imax",
        "widescreen",
        "fullscreen",
        "theatrical",
        "live",
        "action",
        "animated",
        "cartoon",
        "3d",
        "2d",
        "4k",
        "uhd",
        "hd",
        "sd",
        "hdtv",
        "classic",
        "ultimate",
        "remix",
        "original",
        "digital",
        "copy",
        "remaster",
        "retail",
        // Language/audio/subtitle
        "english",
        "eng",
        "japanese",
        "jap",
        "jp",
        "sub",
        "subs",
        "subbed",
        "subtitled",
        "dub",
        "dubs",
        "dubbed",
        "multi",
        "multisubs",
        "dual",
        "audio",
        "bilingual",
        "lat",
        "latin",
        "esp",
        "espanol",
        "spanish",
        "fra",
        "french",
        "ger",
        "german",
        "ita",
        "italian",
        "rus",
        "russian",
        "kor",
        "korean",
        "chi",
        "chinese",
        "mandarin",
        "cantonese",
        "taiwanese",
        "viet",
        "vietnamese",
        "thai",
        "hindi",
        "tamil",
        "telugu",
        // Encoding/source/codec
        "rip",
        "webrip",
        "web",
        "webdl",
        "dl",
        "bluray",
        "brrip",
        "bdrip",
        "dvd",
        "dvdrip",
        "tvrip",
        "pdtv",
        "dsr",
        "sdtv",
        "ldtv",
        "h264",
        "h265",
        "x264",
        "x265",
        "hevc",
        "av1",
        "mpeg",
        "divx",
        "xvid",
        "mp4",
        "mkv",
        "avi",
        "blu",
        "ray",
        "br",
        "bd",
        "tv",
        // Audio codecs
        "aac",
        "aac2",
        "aac5",
        "ac3",
        "dd5",
        "ddp5",
        "ddp7",
        "dts",
        "dtshd",
        "truehd",
        "atmos",
        "flac",
        "mp3",
        "soundtrack",
        "ost",
        "music",
        "songs",
        // Bit depth
        "8bit",
        "10bit",
        "12bit",
        "hi10p",
        "hi10",
        // Release groups / sites / qualities
        "yts",
        "tgx",
        "galaxyrg",
        "qxr",
        "vyto",
        "rarbg",
        "ettv",
        "eztv",
        "psa",
        "meghd",
        "megapack",
        "megustas",
        "megusta",
        "ion10",
        "fgt",
        "screener",
        "scr",
        "cam",
        "telecined",
        "tc",
        "ts",
        "workprint",
        "wp",
        "hdr",
        "hdr10",
        "hdr10plus",
        "dv",
        "dolby",
        "vision",
        "hlg",
        "sdr",
        // Common noise
        "v2",
        "v3",
        "v4",
        "repack",
        "proper",
        "real",
        "readnfo",
        "nfo",
        "internal",
    ];

    allowed.contains(&w)
}

pub fn is_title_match(torrent_base: &str, meta_title: &str) -> bool {
    let compact_torrent = to_compact_title(torrent_base);
    let compact_meta = to_compact_title(meta_title);

    if compact_torrent == compact_meta {
        return true;
    }

    // Common anime abbreviations such as "Re:Zero" retain a meaningful colon
    // and appear as the literal prefix of a longer localized title. Keep this
    // narrow exception without allowing arbitrary two-word franchise prefixes.
    if torrent_base.contains(':')
        && meta_title
            .to_lowercase()
            .starts_with(&torrent_base.to_lowercase())
    {
        return true;
    }

    let get_tokens = |t: &str| -> HashSet<String> {
        t.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| {
                !w.is_empty()
                    && w != &"the"
                    && w != &"and"
                    && w != &"for"
                    && w != &"with"
                    && w != &"of"
                    && w != &"in"
                    && w != &"to"
                    && w != &"a"
                    && w != &"an"
                    && w != &"or"
            })
            .map(|s| s.to_string())
            .collect()
    };

    let w_torrent = get_tokens(torrent_base);
    if w_torrent.is_empty() {
        return false;
    }

    // Short aliases must come from a metadata provider. Deriving them by
    // chopping at ':' or '-' makes franchise names match unrelated entries.
    let variations = vec![meta_title.to_string()];

    for var in variations {
        let w_var = get_tokens(&var);
        if w_var.is_empty() {
            continue;
        }

        if w_var.is_subset(&w_torrent) {
            let extra_words: Vec<&String> = w_torrent.difference(&w_var).collect();
            let mut all_extra_allowed = true;
            for w in extra_words {
                if !is_allowed_extra_word(w) {
                    all_extra_allowed = false;
                    break;
                }
            }
            if all_extra_allowed {
                return true;
            }
        }
    }

    false
}

pub fn verify_torrent_match(
    torrent_title: &str,
    meta_title: &str,
    romaji_title: Option<&str>,
    meta_year: Option<&str>,
    target_season: Option<u32>,
    target_episode: Option<u32>,
) -> bool {
    verify_torrent_match_with_absolute(
        torrent_title,
        meta_title,
        romaji_title,
        meta_year,
        target_season,
        target_episode,
        None,
    )
}

fn verify_torrent_match_with_absolute(
    torrent_title: &str,
    meta_title: &str,
    romaji_title: Option<&str>,
    meta_year: Option<&str>,
    target_season: Option<u32>,
    target_episode: Option<u32>,
    absolute_episode: Option<u32>,
) -> bool {
    let mut hints = vec![meta_title];
    if let Some(romaji) = romaji_title {
        hints.push(romaji);
    }
    let parsed = parse_filename(torrent_title, &hints);

    // 1. Year Match
    if let Some(meta_y_str) = meta_year {
        let clean_y = meta_y_str
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        if let Ok(my) = clean_y.parse::<u32>() {
            if let Some(ty) = parsed.year {
                if (my as i32 - ty as i32).abs() > 1 {
                    return false; // Mismatched year
                }
            }
        }
    }

    // 2. Title Match (try English and Romaji)
    let mut matched = is_title_match(&parsed.base_title, meta_title);
    if !matched {
        if let Some(romaji) = romaji_title {
            matched = is_title_match(&parsed.base_title, romaji);
        }
    }
    if !matched {
        return false;
    }

    // A matching base title is not enough for a movie result. General-purpose
    // indexes also return soundtracks, trailers, commentary and TV collections.
    if target_season.is_none() {
        let tokenize = |value: &str| -> HashSet<String> {
            value
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|token| !token.is_empty())
                .map(str::to_string)
                .collect()
        };
        let torrent_tokens = tokenize(torrent_title);
        let mut title_tokens = tokenize(meta_title);
        if let Some(romaji) = romaji_title {
            title_tokens.extend(tokenize(romaji));
        }
        let non_movie_keywords = [
            "soundtrack",
            "ost",
            "trailer",
            "teaser",
            "commentary",
            "sample",
            "featurette",
            "interview",
            "bloopers",
            "outtakes",
            "extras",
            "bonus",
            "collection",
            "anthology",
            "season",
            "episode",
            "series",
        ];
        if non_movie_keywords
            .iter()
            .any(|keyword| torrent_tokens.contains(*keyword) && !title_tokens.contains(*keyword))
            || !parsed.seasons.is_empty()
        {
            return false;
        }
    }

    // 3. Series Season / Episode Match
    if let Some(ts) = target_season {
        if ts > 0 {
            // Check for special/OVA/movie keywords that are not part of the target show name.
            let lower_title = torrent_title.to_lowercase();
            let lower_meta = meta_title.to_lowercase();
            let lower_romaji = romaji_title.map(|r| r.to_lowercase());

            let get_clean_tokens = |t: &str| -> HashSet<String> {
                t.split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            };

            let meta_tokens = get_clean_tokens(&lower_meta);
            let romaji_tokens = lower_romaji
                .as_ref()
                .map(|r| get_clean_tokens(r))
                .unwrap_or_default();
            let torrent_tokens = get_clean_tokens(&lower_title);

            let ignore_keywords = [
                "ova",
                "ona",
                "special",
                "specials",
                "movie",
                "film",
                "recap",
                "teaser",
                "trailer",
                "bonus",
                "extra",
                "extras",
                "nced",
                "ncop",
                "ost",
                "soundtrack",
                "preview",
                "interview",
                "commentary",
            ];

            let is_pack = parsed.is_pack;
            for &kw in &ignore_keywords {
                if is_pack
                    && (kw == "special"
                        || kw == "specials"
                        || kw == "bonus"
                        || kw == "extra"
                        || kw == "extras"
                        || kw == "ova"
                        || kw == "ona"
                        || kw == "commentary")
                {
                    continue;
                }
                if torrent_tokens.contains(kw)
                    && !meta_tokens.contains(kw)
                    && !romaji_tokens.contains(kw)
                {
                    return false;
                }
            }
        }

        if !parsed.seasons.is_empty() && !parsed.seasons.contains(&ts) {
            return false;
        }

        // Season 1 protection: standalone episodes when target_season > 1
        if ts > 1 && parsed.seasons.is_empty() && !parsed.is_pack {
            return false;
        }

        if let Some(te) = target_episode {
            let matches_episode = parsed.episodes.contains(&te)
                || absolute_episode.is_some_and(|absolute| parsed.episodes.contains(&absolute));

            if parsed.is_pack {
                // A pack may omit explicit episodes; file-level resolution is
                // responsible for proving that it contains the requested file.
                if !parsed.episodes.is_empty() && !matches_episode {
                    return false;
                }
            } else if parsed.episodes.is_empty() || !matches_episode {
                // Single-file results require positive episode evidence.
                return false;
            }
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
        "🎬 YTS: ",
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

// -----------------------------------------------------------------------------
// XML RSS Parser Helper
// -----------------------------------------------------------------------------
fn extract_xml_tag(xml: &str, tag_name: &str) -> Option<String> {
    let re_str = format!(
        r"(?i)<(?:[a-zA-Z0-9_\-]+:)?{}(?:\s+[^>]*?)?>(.*?)</(?:[a-zA-Z0-9_\-]+:)?{}>",
        tag_name, tag_name
    );
    if let Ok(re) = Regex::new(&re_str) {
        if let Some(cap) = re.captures(xml) {
            return Some(cap[1].trim().to_string());
        }
    }
    None
}

// -----------------------------------------------------------------------------
// Individual Scrapers
// -----------------------------------------------------------------------------

// 1. YTS Movie Scraper
async fn scrape_single_yts(client: reqwest::Client, url: String) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(3000));

    if let Ok(resp) = req.send().await {
        if let Ok(json_resp) = resp.json::<YtsSearchResponse>().await {
            if json_resp.status == "ok" && json_resp.data.is_some() {
                let data = json_resp.data.unwrap();
                if let Some(movies) = data.movies {
                    for movie in movies {
                        if let Some(torrents) = movie.torrents {
                            for torrent in torrents {
                                let qmeta = parse_quality_meta(&torrent.quality);
                                let detail_str = if qmeta.details.is_empty() {
                                    String::new()
                                } else {
                                    format!(" | {}", qmeta.details.join(" | "))
                                };
                                let display_quality = format!(
                                    "{} ({})",
                                    qmeta.quality,
                                    torrent.r#type.to_uppercase()
                                );

                                let peers_info = if torrent.seeds == 0 {
                                    "👥 Active (YTS Swarm)".to_string()
                                } else {
                                    format!(
                                        "👥 {} seeders | 📥 {} peers",
                                        torrent.seeds, torrent.peers
                                    )
                                };

                                let hash = normalize_info_hash(&torrent.hash);
                                if !is_valid_info_hash(&hash) {
                                    continue;
                                }
                                let sources = get_sources_for_torrent(&hash);
                                streams.push(Stream {
                                    name: format!("[Bitlab] {}{}", display_quality, detail_str),
                                    title: format!(
                                        "🎬 YTS: {}\n📦 {}\n{}\n⚡ Direct P2P Torrent Stream",
                                        movie.title, torrent.size, peers_info
                                    ),
                                    url: None,
                                    info_hash: Some(hash),
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

pub async fn scrape_yts_movies(client: &reqwest::Client, imdb_id: &str) -> Vec<Stream> {
    let urls = vec![
        format!(
            "https://movies-api.accel.li/api/v2/list_movies.json?query_term={}",
            imdb_id
        ),
        format!(
            "https://yts.mx/api/v2/list_movies.json?query_term={}",
            imdb_id
        ),
    ];

    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client_clone = client.clone();
        set.spawn(async move { scrape_single_yts(client_clone, url).await });
    }

    let mut all_streams: Vec<Stream> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(streams) = res {
            for s in streams {
                merge_stream(&mut all_streams, s);
            }
        }
    }

    all_streams
}

// 2. APIBay Scraper
#[derive(Deserialize, Debug)]
pub struct ApibayTorrent {
    pub name: Option<String>,
    pub info_hash: Option<String>,
    pub size: Option<serde_json::Value>,
    pub seeders: Option<serde_json::Value>,
    pub leechers: Option<serde_json::Value>,
}

pub async fn scrape_apibay(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let encoded_query = urlencoding::encode(query);
    // Restrict the general index to video results. Content-specific matching is
    // still performed centrally after scraping.
    let url = format!("https://apibay.org/q.php?q={}&cat=200", encoded_query);

    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(3000));

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
        if !is_apibay_result_valid(&name, &info_hash) {
            continue;
        }

        let hash = normalize_info_hash(&info_hash);
        let qmeta = parse_quality_meta(&name);
        let detail_str = if qmeta.details.is_empty() {
            String::new()
        } else {
            format!(" | {}", qmeta.details.join(" | "))
        };

        let size_str = torrent
            .size
            .map(|v| get_json_string(&v))
            .unwrap_or_default();
        let size_formatted = format_size(&size_str);

        let seeds_str = torrent
            .seeders
            .map(|v| get_json_string(&v))
            .unwrap_or_default();
        let seeds = seeds_str.parse::<u32>().unwrap_or(0);

        let peers_str = torrent
            .leechers
            .map(|v| get_json_string(&v))
            .unwrap_or_default();
        let peers = peers_str.parse::<u32>().unwrap_or(0);

        let sources = get_sources_for_torrent(&hash);
        streams.push(Stream {
            name: format!("[Bitlab] {}{}", qmeta.quality, detail_str),
            title: format!(
                "🎬 {}: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Direct P2P Torrent Stream",
                provider_label, name, size_formatted, seeds, peers
            ),
            url: None,
            info_hash: Some(hash),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
    }

    streams
}

// 3. TPB HTML Scraper (Fallback for APIBay)
async fn scrape_single_tpb(
    client: reqwest::Client,
    url: String,
    provider_label: String,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(3000));

    let html_text = match req.send().await {
        Ok(resp) => match resp.text().await {
            Ok(t) => t,
            Err(_) => return streams,
        },
        Err(_) => return streams,
    };

    let document = scraper::Html::parse_document(&html_text);

    let row_selector = scraper::Selector::parse("#searchResult tr").unwrap();
    let col2_link_selector = scraper::Selector::parse("td:nth-child(2) a").unwrap();
    let magnet_selector = scraper::Selector::parse("a[href^=\"magnet:?\"]").unwrap();
    let col5_selector = scraper::Selector::parse("td:nth-child(5)").unwrap();
    let col6_selector = scraper::Selector::parse("td:nth-child(6)").unwrap();
    let col7_selector = scraper::Selector::parse("td:nth-child(7)").unwrap();

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
        if !is_valid_info_hash(&info_hash) {
            continue;
        }

        let name = match row.select(&col2_link_selector).next() {
            Some(el) => el.text().collect::<Vec<_>>().concat(),
            None => "Unknown Torrent".to_string(),
        };

        let size = match row.select(&col5_selector).next() {
            Some(el) => el
                .text()
                .collect::<Vec<_>>()
                .concat()
                .replace("&nbsp;", " "),
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

        let qmeta = parse_quality_meta(&name);
        let detail_str = if qmeta.details.is_empty() {
            String::new()
        } else {
            format!(" | {}", qmeta.details.join(" | "))
        };

        let sources = extract_trackers_from_magnet(magnet, &info_hash);
        streams.push(Stream {
            name: format!("[Bitlab] {}{}", qmeta.quality, detail_str),
            title: format!(
                "🎬 {}: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Direct P2P Torrent Stream",
                provider_label, name, size, seeds, leechers
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

pub async fn scrape_tpb_html(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let encoded_query = urlencoding::encode(query);
    let urls = vec![
        format!("https://tpb.party/search/{}/1/99/200", encoded_query),
        format!(
            "https://thepiratebay10.org/search/{}/1/99/200",
            encoded_query
        ),
        format!(
            "https://thepiratebay0.org/search/{}/1/99/200",
            encoded_query
        ),
    ];

    let mut urls = urls.into_iter();
    if let Some(primary_url) = urls.next() {
        let primary =
            scrape_single_tpb(client.clone(), primary_url, provider_label.to_string()).await;
        if !primary.is_empty() {
            return primary;
        }
    }

    // Only race mirrors when the primary is unavailable or empty.
    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client_clone = client.clone();
        let label_clone = provider_label.to_string();
        set.spawn(async move { scrape_single_tpb(client_clone, url, label_clone).await });
    }
    while let Some(Ok(streams)) = set.join_next().await {
        if !streams.is_empty() {
            return streams;
        }
    }
    Vec::new()
}

// 4. Bitsearch Scraper
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

#[derive(Clone, Copy)]
pub enum MediaCategory {
    Movie,
    Series,
    Anime,
}

impl MediaCategory {
    fn bitsearch_id(self) -> u8 {
        match self {
            Self::Movie => 2,
            Self::Series => 3,
            Self::Anime => 4,
        }
    }
}

pub async fn scrape_bitsearch(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
    category: MediaCategory,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let encoded_query = urlencoding::encode(query);
    let url = format!(
        "https://bitsearch.to/api/v1/search?q={}&category={}&limit=50&sort=seeders",
        encoded_query,
        category.bitsearch_id()
    );

    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(3000));

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

            let hash = normalize_info_hash(&infohash);
            if !is_valid_info_hash(&hash) {
                continue;
            }
            let qmeta = parse_quality_meta(&title);
            let detail_str = if qmeta.details.is_empty() {
                String::new()
            } else {
                format!(" | {}", qmeta.details.join(" | "))
            };

            let size_bytes = t.size.unwrap_or(0);
            let size_formatted = format_size(&size_bytes.to_string());

            let seeds = t.seeders.unwrap_or(0);
            let peers = t.leechers.unwrap_or(0);

            let sources = get_sources_for_torrent(&hash);
            streams.push(Stream {
                name: format!("[Bitlab] {}{}", qmeta.quality, detail_str),
                title: format!(
                    "🎬 {}: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Direct P2P Torrent Stream",
                    provider_label, title, size_formatted, seeds, peers
                ),
                url: None,
                info_hash: Some(hash),
                file_idx: None,
                sources: Some(sources),
                behavior_hints: None,
            });
        }
    }

    streams
}

// 5. SolidTorrents Scraper
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

async fn scrape_single_solidtorrent(
    client: reqwest::Client,
    url: String,
    provider_label: String,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(3000));

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
                        if !is_valid_info_hash(&info_hash) {
                            continue;
                        }

                        let size_bytes = t.size.unwrap_or(0);
                        let size_formatted = format_size(&size_bytes.to_string());

                        let swarm = t.swarm.unwrap_or(SolidSwarm {
                            seeders: Some(0),
                            leechers: Some(0),
                        });
                        let seeds = swarm.seeders.unwrap_or(0);
                        let leechers = swarm.leechers.unwrap_or(0);

                        let qmeta = parse_quality_meta(&title);
                        let detail_str = if qmeta.details.is_empty() {
                            String::new()
                        } else {
                            format!(" | {}", qmeta.details.join(" | "))
                        };

                        let sources = extract_trackers_from_magnet(&magnet, &info_hash);
                        streams.push(Stream {
                            name: format!("[Bitlab] {}{}", qmeta.quality, detail_str),
                            title: format!(
                                "🎬 {}: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Direct P2P Torrent Stream",
                                provider_label,
                                title,
                                size_formatted,
                                seeds,
                                leechers
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

pub async fn scrape_solidtorrents(
    client: &reqwest::Client,
    query: &str,
    provider_label: &str,
) -> Vec<Stream> {
    let encoded_query = urlencoding::encode(query);
    let urls = vec![
        format!(
            "https://solidtorrents.to/api/v1/search?q={}&category=video&sort=seeders",
            encoded_query
        ),
        format!(
            "https://solidtorrents.net/api/v1/search?q={}&category=video&sort=seeders",
            encoded_query
        ),
    ];

    for url in urls {
        let streams =
            scrape_single_solidtorrent(client.clone(), url, provider_label.to_string()).await;
        if !streams.is_empty() {
            return streams;
        }
    }
    Vec::new()
}

// 6. Nyaa Anime Scraper
async fn scrape_single_nyaa(client: reqwest::Client, url: String) -> Vec<Stream> {
    let mut streams = Vec::new();
    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(3000));

    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            if let Ok(xml_text) = resp.text().await {
                let items: Vec<&str> = xml_text.split("<item>").skip(1).collect();
                for item_xml in items {
                    let item_content = item_xml.split("</item>").next().unwrap_or("");
                    let raw_title = extract_xml_tag(item_content, "title").unwrap_or_default();
                    let title = decode_html_entities(&raw_title);
                    let hash = extract_xml_tag(item_content, "infoHash").unwrap_or_default();
                    let size = extract_xml_tag(item_content, "size").unwrap_or_default();
                    let seeders = extract_xml_tag(item_content, "seeders")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    let leechers = extract_xml_tag(item_content, "leechers")
                        .and_then(|l| l.parse::<u32>().ok())
                        .unwrap_or(0);

                    if hash.is_empty() || title.is_empty() {
                        continue;
                    }

                    let qmeta = parse_quality_meta(&title);
                    let detail_str = if qmeta.details.is_empty() {
                        String::new()
                    } else {
                        format!(" | {}", qmeta.details.join(" | "))
                    };
                    let hash = normalize_info_hash(&hash);
                    if !is_valid_info_hash(&hash) {
                        continue;
                    }
                    let sources = get_sources_for_torrent(&hash);
                    streams.push(Stream {
                        name: format!("[Bitlab] {}{}", qmeta.quality, detail_str),
                        title: format!(
                            "🌸 Nyaa: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Direct P2P Torrent Stream",
                            title,
                            size,
                            seeders,
                            leechers
                        ),
                        url: None,
                        info_hash: Some(hash),
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

pub async fn scrape_nyaa(client: &reqwest::Client, query: &str) -> Vec<Stream> {
    let encoded_query = urlencoding::encode(query);
    let urls = vec![
        format!("https://nyaa.si/?page=rss&c=1_2&q={}", encoded_query),
        format!("https://nyaa.land/?page=rss&c=1_2&q={}", encoded_query),
    ];

    for url in urls {
        let streams = scrape_single_nyaa(client.clone(), url).await;
        if !streams.is_empty() {
            return streams;
        }
    }
    Vec::new()
}

// 7. EZTV Series Scraper
async fn scrape_single_eztv(
    client: reqwest::Client,
    domain: String,
    imdb_id: String,
    target_season: u32,
    target_episode: u32,
) -> Vec<Stream> {
    let mut streams = Vec::new();
    let clean_imdb_id = imdb_id.strip_prefix("tt").unwrap_or(&imdb_id);

    // Page 1 is usually enough to find all recent encodes of the episode
    let url = format!(
        "{}/api/get-torrents?imdb_id={}&limit=50&page=1",
        domain, clean_imdb_id
    );

    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(3000));

    let resp_text = match req.send().await {
        Ok(resp) => match resp.text().await {
            Ok(t) => t,
            Err(_) => return streams,
        },
        Err(_) => return streams,
    };

    let json_val: serde_json::Value = match serde_json::from_str(&resp_text) {
        Ok(v) => v,
        Err(_) => return streams,
    };

    let torrents = match json_val.get("torrents") {
        Some(t) => match t.as_array() {
            Some(arr) => arr,
            None => return streams,
        },
        None => return streams,
    };

    for item in torrents {
        let hash = get_json_string(item.get("hash").unwrap_or(&serde_json::Value::Null));
        let title = get_json_string(item.get("title").unwrap_or(&serde_json::Value::Null));
        let season_str = get_json_string(item.get("season").unwrap_or(&serde_json::Value::Null));
        let episode_str = get_json_string(item.get("episode").unwrap_or(&serde_json::Value::Null));
        let seeds = get_json_u32(item.get("seeds").unwrap_or(&serde_json::Value::Null), 0);
        let peers = get_json_u32(item.get("peers").unwrap_or(&serde_json::Value::Null), 0);
        let size_bytes_str =
            get_json_string(item.get("size_bytes").unwrap_or(&serde_json::Value::Null));

        if hash.is_empty() || title.is_empty() {
            continue;
        }

        let season = season_str.parse::<u32>().unwrap_or(0);
        let episode = episode_str.parse::<u32>().unwrap_or(0);

        if season != target_season || episode != target_episode {
            continue;
        }

        let size_formatted = format_size(&size_bytes_str);
        let qmeta = parse_quality_meta(&title);
        let detail_str = if qmeta.details.is_empty() {
            String::new()
        } else {
            format!(" | {}", qmeta.details.join(" | "))
        };

        let hash = normalize_info_hash(&hash);
        if !is_valid_info_hash(&hash) {
            continue;
        }
        let sources = get_sources_for_torrent(&hash);
        streams.push(Stream {
            name: format!("[Bitlab] {}{}", qmeta.quality, detail_str),
            title: format!(
                "📺 EZTV: {}\n📦 {}\n👥 {} seeders | 📥 {} peers\n⚡ Direct P2P Torrent Stream",
                title, size_formatted, seeds, peers
            ),
            url: None,
            info_hash: Some(hash),
            file_idx: None,
            sources: Some(sources),
            behavior_hints: None,
        });
    }

    streams
}

pub async fn scrape_eztv(
    client: &reqwest::Client,
    imdb_id: &str,
    target_season: u32,
    target_episode: u32,
) -> Vec<Stream> {
    let primary = scrape_single_eztv(
        client.clone(),
        "https://eztvx.to".to_string(),
        imdb_id.to_string(),
        target_season,
        target_episode,
    )
    .await;
    if !primary.is_empty() {
        return primary;
    }

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
            scrape_single_eztv(
                client_clone,
                domain,
                imdb_clone,
                target_season,
                target_episode,
            )
            .await
        });
    }

    while let Some(Ok(streams)) = set.join_next().await {
        if !streams.is_empty() {
            return streams;
        }
    }
    Vec::new()
}

// -----------------------------------------------------------------------------
// Backup Metadata Providers
// -----------------------------------------------------------------------------
fn parse_kitsu_meta(json: &serde_json::Value) -> Option<(String, Option<String>, Option<String>)> {
    let attributes = json.get("data")?.get("attributes")?;
    let titles = attributes.get("titles");
    let name = titles
        .and_then(|value| value.get("en_us"))
        .and_then(|value| value.as_str())
        .or_else(|| {
            titles
                .and_then(|value| value.get("en"))
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            attributes
                .get("canonicalTitle")
                .and_then(|value| value.as_str())
        })?
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    let year = attributes
        .get("startDate")
        .and_then(|value| value.as_str())
        .and_then(|date| date.split('-').next())
        .filter(|year| year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string);
    let romaji = titles
        .and_then(|value| value.get("en_jp"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|title| !title.is_empty() && clean_title(title) != clean_title(&name))
        .map(str::to_string);
    Some((name, year, romaji))
}

async fn fetch_kitsu_meta(
    client: &reqwest::Client,
    kitsu_id: &str,
) -> Option<(String, Option<String>, Option<String>)> {
    let url = format!("https://kitsu.io/api/edge/anime/{}", kitsu_id);
    let response = client
        .get(url)
        .header("Accept", "application/vnd.api+json")
        .timeout(std::time::Duration::from_millis(1500))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let json = response.json::<serde_json::Value>().await.ok()?;
    parse_kitsu_meta(&json)
}

pub async fn fetch_meta_cached(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>,
    >,
    r#type: &str,
    imdb_id: &str,
) -> Option<(String, Option<String>)> {
    {
        let cache = meta_cache.read().await;
        if let Some(meta) = cache.get(imdb_id) {
            return Some(meta.clone());
        }
    }

    // Kitsu IDs are not IMDb IDs and are not served by Cinemeta/TVmaze's IMDb
    // lookup. Resolve them directly before entering the IMDb fallback chain.
    if let Some(kitsu_id) = imdb_id.strip_prefix("kitsu:") {
        if let Some((name, year, _)) = fetch_kitsu_meta(client, kitsu_id).await {
            let meta = (name, year);
            let mut cache = meta_cache.write().await;
            cache.insert(imdb_id.to_string(), meta.clone());
            return Some(meta);
        }
        return None;
    }

    // 1. Try Cinemeta (Main Stremio Metadata Provider)
    let cinemeta_fut = fetch_meta(client, r#type, imdb_id);
    let cinemeta_res =
        tokio::time::timeout(std::time::Duration::from_millis(1500), cinemeta_fut).await;

    if let Ok(Some(meta)) = cinemeta_res {
        let mut cache = meta_cache.write().await;
        let val = (meta.name, meta.year);
        cache.insert(imdb_id.to_string(), val.clone());
        return Some(val);
    }

    // 2. Fallback to TVmaze (for series) - 100% free, no API key required
    if r#type == "series" {
        println!(
            "[INFO] Cinemeta timed out. Trying TVmaze fallback for: {}",
            imdb_id
        );
        let url = format!("https://api.tvmaze.com/lookup/shows?imdb={}", imdb_id);
        let req = client
            .get(&url)
            .timeout(std::time::Duration::from_millis(1500));
        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                #[derive(Deserialize)]
                struct TvMazeShow {
                    name: String,
                    premiered: Option<String>,
                }
                if let Ok(show) = resp.json::<TvMazeShow>().await {
                    let year = show
                        .premiered
                        .and_then(|p| p.split('-').next().map(|s| s.to_string()));
                    let mut cache = meta_cache.write().await;
                    let val = (show.name, year);
                    cache.insert(imdb_id.to_string(), val.clone());
                    return Some(val);
                }
            }
        }
    }

    // 3. Fallback to Community TMDb Stremio Addon
    println!("[INFO] Trying TMDb Addon fallback for: {}", imdb_id);
    let url = format!(
        "https://94c8cb97ae04-tmdb-addon.baby-beamup.club/meta/{}/{}.json",
        r#type, imdb_id
    );
    let req = client
        .get(&url)
        .timeout(std::time::Duration::from_millis(1500));
    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            #[derive(Deserialize)]
            struct TmdbMeta {
                name: String,
                release_date: Option<String>,
                first_air_date: Option<String>,
            }
            #[derive(Deserialize)]
            struct TmdbResponse {
                meta: Option<TmdbMeta>,
            }
            if let Ok(data) = resp.json::<TmdbResponse>().await {
                if let Some(meta) = data.meta {
                    let date = meta.release_date.or(meta.first_air_date);
                    let year = date.and_then(|d| d.split('-').next().map(|s| s.to_string()));
                    let mut cache = meta_cache.write().await;
                    let val = (meta.name, year);
                    cache.insert(imdb_id.to_string(), val.clone());
                    return Some(val);
                }
            }
        }
    }

    None
}

async fn check_if_anime_and_get_romaji(
    client: &reqwest::Client,
    english_title: &str,
    target_year: Option<&str>,
) -> (bool, Option<String>) {
    let encoded = urlencoding::encode(english_title);
    let url = format!("https://kitsu.io/api/edge/anime?filter[text]={}", encoded);
    let req = client
        .get(&url)
        .header("Accept", "application/vnd.api+json")
        .header("Content-Type", "application/vnd.api+json")
        .timeout(std::time::Duration::from_millis(1500));

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

                            if let Some(canonical) =
                                attributes.get("canonicalTitle").and_then(|t| t.as_str())
                            {
                                if clean_title(&canonical.to_lowercase()) == clean_target {
                                    is_match = true;
                                }
                            }

                            if let Some(titles) = attributes.get("titles") {
                                if let Some(en) = titles.get("en").and_then(|t| t.as_str()) {
                                    if clean_title(&en.to_lowercase()) == clean_target {
                                        is_match = true;
                                    }
                                }
                                if let Some(en_us) = titles.get("en_us").and_then(|t| t.as_str()) {
                                    if clean_title(&en_us.to_lowercase()) == clean_target {
                                        is_match = true;
                                    }
                                }
                                if let Some(en_jp) = titles.get("en_jp").and_then(|t| t.as_str()) {
                                    romaji_title = Some(en_jp.to_string());
                                    if clean_title(&en_jp.to_lowercase()) == clean_target {
                                        is_match = true;
                                    }
                                }
                            }

                            if is_match {
                                if let Some(t_year_str) = target_year {
                                    let clean_t = t_year_str
                                        .chars()
                                        .take_while(|c| c.is_ascii_digit())
                                        .collect::<String>();
                                    if let Ok(t_year) = clean_t.parse::<i32>() {
                                        if let Some(start_date) =
                                            attributes.get("startDate").and_then(|d| d.as_str())
                                        {
                                            if let Some(year_str) = start_date.split('-').next() {
                                                if let Ok(k_year) = year_str.parse::<i32>() {
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

enum ScraperTaskResult {
    Streams(Vec<Stream>),
    MovieMeta(Option<(String, Option<String>, bool, Option<String>)>),
    SeriesMeta(Option<(String, Option<String>, bool, Option<String>, Option<u32>)>),
}

// -----------------------------------------------------------------------------
// Movie Stream Resolution Coordinating Function
// -----------------------------------------------------------------------------
pub async fn get_movie_streams(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>,
    >,
    stream_cache: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, (Vec<Stream>, std::time::Instant)>>,
    >,
    imdb_id: &str,
) -> Vec<Stream> {
    {
        let cache = stream_cache.read().await;
        if let Some((streams, timestamp)) = cache.get(imdb_id) {
            if timestamp.elapsed().as_secs() < STREAM_CACHE_TTL_SECS {
                println!("[INFO] Returning cached streams for movie: {}", imdb_id);
                return streams.clone();
            }
        }
    }

    println!("[INFO] Resolving streams for movie: {}", imdb_id);
    let start_time = std::time::Instant::now();
    let mut set: tokio::task::JoinSet<ScraperTaskResult> = tokio::task::JoinSet::new();

    // 1. Spawn ID-based searches immediately
    let client_yts = client.clone();
    let imdb_id_clone = imdb_id.to_string();
    set.spawn(async move {
        ScraperTaskResult::Streams(scrape_yts_movies(&client_yts, &imdb_id_clone).await)
    });

    let client_apibay_id = client.clone();
    let imdb_id_clone2 = imdb_id.to_string();
    set.spawn(async move {
        ScraperTaskResult::Streams(
            scrape_apibay(&client_apibay_id, &imdb_id_clone2, "APIBay").await,
        )
    });

    // 2. Fetch metadata (Cinemeta / TVmaze / TMDb fallbacks) and check if anime
    let client_meta = client.clone();
    let meta_cache_clone = meta_cache.clone();
    let imdb_id_clone3 = imdb_id.to_string();
    set.spawn(async move {
        if let Some((name, year)) =
            fetch_meta_cached(&client_meta, &meta_cache_clone, "movie", &imdb_id_clone3).await
        {
            let (is_anime, romaji) =
                check_if_anime_and_get_romaji_cached(&client_meta, &name, year.as_deref()).await;
            ScraperTaskResult::MovieMeta(Some((name, year, is_anime, romaji)))
        } else {
            ScraperTaskResult::MovieMeta(None)
        }
    });

    let mut all_streams: Vec<Stream> = Vec::new();
    let mut resolved_show_name: Option<String> = None;
    let mut resolved_romaji_name: Option<String> = None;
    let mut resolved_year: Option<String> = None;
    let mut meta_resolved = false;
    let timeout_dur = std::time::Duration::from_millis(6000);

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
                                if !verify_torrent_match(
                                    &torrent_title,
                                    show_name,
                                    resolved_romaji_name.as_deref(),
                                    resolved_year.as_deref(),
                                    None,
                                    None,
                                ) {
                                    continue;
                                }
                            }
                            merge_stream(&mut all_streams, s);
                        }
                    }
                    ScraperTaskResult::MovieMeta(meta_res) => {
                        if !meta_resolved {
                            meta_resolved = true;
                            if let Some((name, year, is_anime, romaji_opt)) = meta_res {
                                resolved_show_name = Some(name.clone());
                                resolved_year = year.clone();
                                resolved_romaji_name = romaji_opt.clone();

                                // Search queries: English Title and Romaji Title (if Anime)
                                let mut queries = vec![name.clone()];
                                if let Some(romaji) = &romaji_opt {
                                    queries.push(romaji.clone());
                                }

                                for q in queries {
                                    let cleaned_q = clean_title(&q);
                                    let query_with_year = if let Some(yr) = &year {
                                        if let Some(start_yr) = extract_start_year(yr) {
                                            format!("{} {}", cleaned_q, start_yr)
                                        } else {
                                            cleaned_q.clone()
                                        }
                                    } else {
                                        cleaned_q.clone()
                                    };

                                    // Spawn SolidTorrents search
                                    let c_solid = client.clone();
                                    let q_solid = query_with_year.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(
                                            scrape_solidtorrents(
                                                &c_solid,
                                                &q_solid,
                                                "SolidTorrents",
                                            )
                                            .await,
                                        )
                                    });

                                    // Spawn APIBay search
                                    let c_api = client.clone();
                                    let q_api = query_with_year.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(
                                            scrape_apibay(&c_api, &q_api, "APIBay").await,
                                        )
                                    });

                                    // Spawn TPB HTML search
                                    let c_tpb = client.clone();
                                    let q_tpb = query_with_year.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(
                                            scrape_tpb_html(&c_tpb, &q_tpb, "TPB").await,
                                        )
                                    });

                                    // Spawn Bitsearch search
                                    let c_bit = client.clone();
                                    let q_bit = query_with_year.clone();
                                    set.spawn(async move {
                                        ScraperTaskResult::Streams(
                                            scrape_bitsearch(
                                                &c_bit,
                                                &q_bit,
                                                "Bitsearch",
                                                MediaCategory::Movie,
                                            )
                                            .await,
                                        )
                                    });

                                    // Spawn Nyaa (Anime) search
                                    if is_anime {
                                        let c_nyaa = client.clone();
                                        let q_nyaa = query_with_year.clone();
                                        set.spawn(async move {
                                            ScraperTaskResult::Streams(
                                                scrape_nyaa(&c_nyaa, &q_nyaa).await,
                                            )
                                        });
                                        // Also search Nyaa without year since anime RSS is often yearless
                                        let c_nyaa2 = client.clone();
                                        let q_nyaa2 = cleaned_q.clone();
                                        set.spawn(async move {
                                            ScraperTaskResult::Streams(
                                                scrape_nyaa(&c_nyaa2, &q_nyaa2).await,
                                            )
                                        });
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Some(Err(_))) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }

    // ID-based tasks may finish before metadata. Re-run movie validation after
    // all tasks so completion order cannot bypass title/content checks.
    if let Some(show_name) = &resolved_show_name {
        all_streams.retain(|stream| {
            verify_torrent_match(
                &extract_torrent_title(&stream.title),
                show_name,
                resolved_romaji_name.as_deref(),
                resolved_year.as_deref(),
                None,
                None,
            )
        });
    } else {
        // When metadata is unavailable, retain only YTS's IMDb-bound response;
        // a general index search for an IMDb string is not sufficient evidence.
        all_streams.retain(|stream| {
            stream
                .title
                .lines()
                .next()
                .is_some_and(|title| title.contains("YTS: "))
        });
    }

    // Sort by seeders descending
    all_streams.sort_by(|a, b| {
        let a_seeds = extract_seeds(&a.title);
        let b_seeds = extract_seeds(&b.title);
        b_seeds.cmp(&a_seeds)
    });

    // Save to cache
    if meta_resolved && !all_streams.is_empty() {
        let mut cache = stream_cache.write().await;
        cache.insert(
            imdb_id.to_string(),
            (all_streams.clone(), std::time::Instant::now()),
        );
    }

    println!(
        "[INFO] Movie streams resolved: {} results",
        all_streams.len()
    );
    all_streams
}

// -----------------------------------------------------------------------------
// Series Stream Resolution Coordinating Function
// -----------------------------------------------------------------------------
pub async fn get_series_streams(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>,
    >,
    stream_cache: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, (Vec<Stream>, std::time::Instant)>>,
    >,
    torrent_files_cache: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, Vec<TorrentFile>>>,
    >,
    imdb_id: &str,
    season: u32,
    episode: u32,
) -> Vec<Stream> {
    let cache_key = format!("{}:{}:{}", imdb_id, season, episode);
    {
        let cache = stream_cache.read().await;
        if let Some((streams, timestamp)) = cache.get(&cache_key) {
            if timestamp.elapsed().as_secs() < STREAM_CACHE_TTL_SECS {
                println!("[INFO] Returning cached streams for series: {}", cache_key);
                return streams.clone();
            }
        }
    }

    println!(
        "[INFO] Resolving streams for series: {} S{:02}E{:02}",
        imdb_id, season, episode
    );
    let start_time = std::time::Instant::now();
    let mut set: tokio::task::JoinSet<ScraperTaskResult> = tokio::task::JoinSet::new();

    // 1. Spawn EZTV ID search immediately for actual IMDb IDs. Passing a
    // Kitsu identifier to EZTV's imdb_id parameter can never produce a match.
    if imdb_id.starts_with("tt") {
        let client_eztv = client.clone();
        let imdb_id_eztv = imdb_id.to_string();
        set.spawn(async move {
            ScraperTaskResult::Streams(
                scrape_eztv(&client_eztv, &imdb_id_eztv, season, episode).await,
            )
        });
    }

    // 2. Fetch metadata and check anime & absolute episode
    let client_meta = client.clone();
    let meta_cache_clone = meta_cache.clone();
    let imdb_id_clone = imdb_id.to_string();
    set.spawn(async move {
        if let Some(kitsu_id) = imdb_id_clone.strip_prefix("kitsu:") {
            let meta_fut = fetch_kitsu_meta(&client_meta, kitsu_id);
            let anizip_fut =
                fetch_anizip_absolute_episode_cached(&client_meta, &imdb_id_clone, episode);
            let (meta_res, absolute_episode) = tokio::join!(meta_fut, anizip_fut);
            return if let Some((name, year, romaji)) = meta_res {
                {
                    let mut cache = meta_cache_clone.write().await;
                    cache.insert(imdb_id_clone.clone(), (name.clone(), year.clone()));
                }
                ScraperTaskResult::SeriesMeta(Some((name, year, true, romaji, absolute_episode)))
            } else {
                ScraperTaskResult::SeriesMeta(None)
            };
        }

        if let Some((name, year)) =
            fetch_meta_cached(&client_meta, &meta_cache_clone, "series", &imdb_id_clone).await
        {
            // Start AniZip speculatively, but do not make every live-action
            // series wait for an anime-only provider. If Kitsu says this is
            // not anime, abort the lookup before returning metadata so the
            // title searches can begin immediately.
            let anizip_client = client_meta.clone();
            let anizip_id = imdb_id_clone.clone();
            let anizip_task = tokio::spawn(async move {
                fetch_anizip_absolute_episode_cached(&anizip_client, &anizip_id, episode).await
            });
            let (detected_anime, romaji) =
                check_if_anime_and_get_romaji_cached(&client_meta, &name, year.as_deref()).await;
            let absolute_episode = if detected_anime {
                anizip_task.await.ok().flatten()
            } else {
                anizip_task.abort();
                None
            };
            ScraperTaskResult::SeriesMeta(Some((
                name,
                year,
                detected_anime,
                romaji,
                absolute_episode,
            )))
        } else {
            ScraperTaskResult::SeriesMeta(None)
        }
    });

    let mut all_streams: Vec<Stream> = Vec::new();
    let mut resolved_show_name: Option<String> = None;
    let mut resolved_romaji_name: Option<String> = None;
    let mut resolved_year: Option<String> = None;
    let mut resolved_absolute_episode: Option<u32> = None;
    let mut resolved_is_anime = imdb_id.starts_with("kitsu:");
    let mut meta_resolved = false;
    let timeout_dur = std::time::Duration::from_millis(6000);

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
                                if !verify_torrent_match_with_absolute(
                                    &torrent_title,
                                    show_name,
                                    resolved_romaji_name.as_deref(),
                                    resolved_year.as_deref(),
                                    Some(season),
                                    Some(episode),
                                    resolved_absolute_episode,
                                ) {
                                    continue;
                                }
                            }
                            merge_stream(&mut all_streams, s);
                        }
                    }
                    ScraperTaskResult::SeriesMeta(meta_res) => {
                        if !meta_resolved {
                            meta_resolved = true;
                            if let Some((name, year, is_anime, romaji_opt, absolute_episode)) =
                                meta_res
                            {
                                resolved_show_name = Some(name.clone());
                                resolved_year = year.clone();
                                resolved_romaji_name = romaji_opt.clone();
                                resolved_absolute_episode = absolute_episode;
                                resolved_is_anime = is_anime;

                                // Search queries: English Title and Romaji Title (if Anime)
                                let mut queries = vec![name.clone()];
                                if let Some(romaji) = &romaji_opt {
                                    queries.push(romaji.clone());
                                }

                                for q in queries {
                                    let cleaned_q = clean_title(&q);

                                    // 1. Query for exact episode: "Show Name S01E01"
                                    let query_exact =
                                        format!("{} S{:02}E{:02}", cleaned_q, season, episode);
                                    // 2. Query for season pack: "Show Name S01"
                                    let query_season = format!("{} S{:02}", cleaned_q, season);
                                    // 3. Bare-title query catches torrents that don't use the SxxExx / Sxx convention (common for older shows). The season/episode filter narrows results.
                                    let query_bare = cleaned_q.clone();
                                    // 4. Year-qualified query disambiguates shows that share a name and matches torrents that include the premiere year.
                                    let mut search_queries =
                                        vec![query_exact, query_season, query_bare];
                                    if let Some(yr) = &year {
                                        // Cinemeta may return a range like "2005-2008"; use only the
                                        // start year so the query matches indexes that list the premiere year.
                                        if let Some(start_yr) = extract_start_year(yr) {
                                            search_queries
                                                .push(format!("{} {}", cleaned_q, start_yr));
                                        }
                                    }

                                    for sq in search_queries {
                                        // Spawn SolidTorrents search
                                        let c_solid = client.clone();
                                        let q_solid = sq.clone();
                                        set.spawn(async move {
                                            ScraperTaskResult::Streams(
                                                scrape_solidtorrents(
                                                    &c_solid,
                                                    &q_solid,
                                                    "SolidTorrents",
                                                )
                                                .await,
                                            )
                                        });

                                        // Spawn APIBay search
                                        let c_api = client.clone();
                                        let q_api = sq.clone();
                                        set.spawn(async move {
                                            ScraperTaskResult::Streams(
                                                scrape_apibay(&c_api, &q_api, "APIBay").await,
                                            )
                                        });

                                        // Spawn TPB HTML search
                                        let c_tpb = client.clone();
                                        let q_tpb = sq.clone();
                                        set.spawn(async move {
                                            ScraperTaskResult::Streams(
                                                scrape_tpb_html(&c_tpb, &q_tpb, "TPB").await,
                                            )
                                        });

                                        // Bitsearch has a low anonymous daily
                                        // request allowance. Use it for exact
                                        // and season queries, not broad title/year
                                        // fallbacks already covered elsewhere.
                                        if sq.contains(&format!(" S{:02}", season)) {
                                            let c_bit = client.clone();
                                            let q_bit = sq.clone();
                                            let category = if is_anime {
                                                MediaCategory::Anime
                                            } else {
                                                MediaCategory::Series
                                            };
                                            set.spawn(async move {
                                                ScraperTaskResult::Streams(
                                                    scrape_bitsearch(
                                                        &c_bit,
                                                        &q_bit,
                                                        "Bitsearch",
                                                        category,
                                                    )
                                                    .await,
                                                )
                                            });
                                        }
                                    }

                                    // Nyaa Anime search
                                    if is_anime {
                                        let mut nyaa_queries = vec![
                                            format!("{} S{:02}E{:02}", cleaned_q, season, episode),
                                            format!("{} S{:02}", cleaned_q, season),
                                        ];

                                        // Add absolute episode search for Anime
                                        if let Some(abs_ep) = absolute_episode {
                                            nyaa_queries
                                                .push(format!("{} {:02}", cleaned_q, abs_ep));
                                        } else {
                                            nyaa_queries
                                                .push(format!("{} {:02}", cleaned_q, episode));
                                        }

                                        for nq in nyaa_queries {
                                            let c_nyaa = client.clone();
                                            set.spawn(async move {
                                                ScraperTaskResult::Streams(
                                                    scrape_nyaa(&c_nyaa, &nq).await,
                                                )
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
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

    let absolute_episode = if resolved_absolute_episode.is_some() {
        resolved_absolute_episode
    } else if resolved_is_anime {
        fetch_anizip_absolute_episode_cached(client, imdb_id, episode).await
    } else {
        None
    };

    // Resolve file indices for multi-file torrents (Season packs / Complete series)
    resolve_file_indices(
        client,
        torrent_files_cache,
        &mut all_streams,
        season,
        episode,
        absolute_episode,
        resolved_show_name.clone(),
    )
    .await;

    // Filter out matches that don't match the requested episode
    all_streams.retain(|s| {
        let torrent_title = extract_torrent_title(&s.title);
        if let Some(show_name) = &resolved_show_name {
            if !verify_torrent_match_with_absolute(
                &torrent_title,
                show_name,
                resolved_romaji_name.as_deref(),
                resolved_year.as_deref(),
                Some(season),
                Some(episode),
                absolute_episode,
            ) {
                return false;
            }
        }

        if s.file_idx.is_some() {
            return true;
        }
        let mut hints = Vec::new();
        if let Some(show_name) = &resolved_show_name {
            hints.push(show_name.as_str());
        }
        if let Some(romaji) = &resolved_romaji_name {
            hints.push(romaji.as_str());
        }
        let parsed = parse_filename(&torrent_title, &hints);

        // If it's a pack and we couldn't resolve the exact file index, drop it
        if parsed.is_pack {
            return false;
        }

        if !parsed.episodes.is_empty() {
            let matches_relative = parsed.episodes.contains(&episode);
            let matches_absolute =
                absolute_episode.map_or(false, |abs| parsed.episodes.contains(&abs));
            if !matches_relative && !matches_absolute {
                return false;
            }
        }

        if !parsed.seasons.is_empty() && !parsed.seasons.contains(&season) {
            return false;
        }

        true
    });

    // Save to cache
    if meta_resolved && !all_streams.is_empty() {
        let mut cache = stream_cache.write().await;
        cache.insert(cache_key, (all_streams.clone(), std::time::Instant::now()));
    }

    println!(
        "[INFO] Series streams resolved: {} results",
        all_streams.len()
    );
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

fn merge_stream(streams: &mut Vec<Stream>, mut incoming: Stream) {
    let Some(incoming_hash) = incoming.info_hash.as_deref() else {
        streams.push(incoming);
        return;
    };

    let Some(existing) = streams
        .iter_mut()
        .find(|stream| stream.info_hash.as_deref() == Some(incoming_hash))
    else {
        streams.push(incoming);
        return;
    };

    let mut merged_sources = existing.sources.take().unwrap_or_default();
    for source in incoming.sources.take().unwrap_or_default() {
        if !merged_sources.contains(&source) {
            merged_sources.push(source);
        }
    }

    // Keep the provider presentation with the strongest observed swarm while
    // retaining trackers learned from every duplicate result.
    if extract_seeds(&incoming.title) > extract_seeds(&existing.title) {
        existing.name = incoming.name;
        existing.title = incoming.title;
    }
    existing.sources = Some(merged_sources);
}

// -----------------------------------------------------------------------------
// Torrent File Index Resolver & Torrent File Parser
// -----------------------------------------------------------------------------
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct TorrentFile {
    pub path: String,
    pub size: u64,
    pub index: u32,
}

#[derive(Deserialize)]
struct TorBoxTorrentInfoResponse {
    success: bool,
    data: Option<TorBoxTorrentInfo>,
}

#[derive(Deserialize)]
struct TorBoxTorrentInfo {
    hash: String,
    files: Vec<TorBoxTorrentFile>,
}

#[derive(Deserialize)]
struct TorBoxTorrentFile {
    id: u32,
    name: String,
    size: u64,
}

fn parse_torbox_torrent_files(body: &str, expected_info_hash: &str) -> Option<Vec<TorrentFile>> {
    let response = serde_json::from_str::<TorBoxTorrentInfoResponse>(body).ok()?;
    let data = response.data?;
    if !response.success
        || normalize_info_hash(&data.hash) != normalize_info_hash(expected_info_hash)
    {
        return None;
    }

    let files = data
        .files
        .into_iter()
        .filter(|file| !file.name.trim().is_empty())
        .map(|file| TorrentFile {
            path: file.name,
            size: file.size,
            index: file.id,
        })
        .collect::<Vec<_>>();
    (!files.is_empty()).then_some(files)
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

    // Torrent cache mirrors occasionally serve a truncated or otherwise
    // malformed .torrent file. TorBox exposes the cached metadata as JSON,
    // including the original zero-based file IDs Stremio needs as fileIdx.
    // Validate the returned hash before trusting that mapping.
    let torbox_client = client.clone();
    let torbox_hash = info_hash.to_string();
    set.spawn(async move {
        let encoded_hash = urlencoding::encode(&torbox_hash);
        let url = format!(
            "https://api.torbox.app/v1/api/torrents/torrentinfo?hash={}&timeout=2&use_cache_lookup=true",
            encoded_hash
        );
        let response = torbox_client
            .get(url)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_millis(2500))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = response.text().await.ok()?;
        parse_torbox_torrent_files(&body, &torbox_hash)
    });

    for url in urls {
        let client_clone = client.clone();
        set.spawn(async move {
            let req = client_clone.get(&url)
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                .timeout(std::time::Duration::from_millis(3000));
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

#[cfg(test)]
fn is_file_match(
    file_path: &str,
    target_season: u32,
    target_episode: u32,
    show_name: Option<&str>,
) -> bool {
    is_file_match_with_absolute(file_path, target_season, target_episode, None, show_name)
}

fn is_file_match_with_absolute(
    file_path: &str,
    target_season: u32,
    target_episode: u32,
    absolute_episode: Option<u32>,
    show_name: Option<&str>,
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

    if lower_path.contains("sample") {
        return false;
    }

    if target_season > 0 {
        let ignore_keywords = [
            "nced",
            "ncop",
            "ost",
            "soundtrack",
            "bonus",
            "extras",
            "extra",
            "special",
            "ova",
            "preview",
            "trailer",
            "recap",
            "interview",
            "commentary",
            "featurette",
            "making of",
            "behind the scenes",
            "bloopers",
            "gag reel",
            "deleted scene",
            "outtakes",
        ];
        for kw in ignore_keywords {
            if lower_path.contains(kw) {
                return false;
            }
        }
    }

    let filename = lower_path
        .split(|c| c == '/' || c == '\\')
        .last()
        .unwrap_or(&lower_path);
    let mut hints = Vec::new();
    if let Some(name) = show_name {
        hints.push(name);
    }
    let (filename_seasons, episodes, _) = parse_seasons_episodes(filename, &hints);

    if episodes.contains(&target_episode)
        || absolute_episode.is_some_and(|absolute| episodes.contains(&absolute))
    {
        let season = filename_seasons
            .first()
            .copied()
            .or_else(|| parse_season_from_path(&lower_path));
        match season {
            Some(s) => {
                return s == target_season;
            }
            None => {
                return target_season == 1; // Default to season 1 if not specified in folder
            }
        }
    }
    false
}

pub async fn resolve_file_indices(
    client: &reqwest::Client,
    torrent_files_cache: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, Vec<TorrentFile>>>,
    >,
    streams: &mut [Stream],
    season: u32,
    episode: u32,
    absolute_episode: Option<u32>,
    show_name: Option<String>,
) {
    let mut set = tokio::task::JoinSet::new();
    let fetch_limit = std::sync::Arc::new(tokio::sync::Semaphore::new(4));

    // Resolve only the top 15 seeders torrents to keep it fast
    for (idx, stream) in streams.iter().enumerate().take(15) {
        let torrent_title = extract_torrent_title(&stream.title);
        let mut hints = Vec::new();
        if let Some(ref name) = show_name {
            hints.push(name.as_str());
        }
        let parsed = parse_filename(&torrent_title, &hints);
        // Only resolve multi-file torrents (packs) to save HTTP requests and CPU time
        let is_multi_file = parsed.is_pack || parsed.episodes.len() > 1;
        if !is_multi_file {
            continue;
        }

        if let Some(ref hash) = stream.info_hash {
            let client = client.clone();
            let cache = torrent_files_cache.clone();
            let hash = hash.clone();
            let show_name_clone = show_name.clone();
            let fetch_limit = fetch_limit.clone();
            set.spawn(async move {
                let cached_files = {
                    let cache_read = cache.read().await;
                    cache_read.get(&hash).cloned()
                };

                let files = match cached_files {
                    Some(f) => Some(f),
                    None => {
                        let Ok(_permit) = fetch_limit.acquire_owned().await else {
                            return (idx, None, None, hash);
                        };
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
                        if is_file_match_with_absolute(
                            &file.path,
                            season,
                            episode,
                            absolute_episode,
                            show_name_clone.as_deref(),
                        ) {
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

    let resolve_timeout = std::time::Duration::from_millis(2000);
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
    })
    .await;
}

pub async fn fetch_anizip_absolute_episode(
    client: &reqwest::Client,
    imdb_or_kitsu: &str,
    episode: u32,
) -> Option<u32> {
    let url = if imdb_or_kitsu.starts_with("kitsu:") {
        let kitsu_id = imdb_or_kitsu.trim_start_matches("kitsu:");
        format!("https://api.ani.zip/mappings?kitsu_id={}", kitsu_id)
    } else {
        format!("https://api.ani.zip/mappings?imdb_id={}", imdb_or_kitsu)
    };

    let req = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .timeout(std::time::Duration::from_millis(3000));

    if let Ok(resp) = req.send().await {
        if let Ok(data) = resp.json::<AniZipResponse>().await {
            if let Some(ep) = data.episodes.get(&episode.to_string()) {
                return ep.absolute_episode_number;
            }
        }
    }
    None
}

// -----------------------------------------------------------------------------
// Unit Tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_torrent_match() {
        let show = "Re:Zero kara Hajimeru Isekai Seikatsu";

        // Correct season and episode
        assert!(verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));
        assert!(!verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 12 [1080p].mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));

        // Explicit season
        assert!(verify_torrent_match(
            "[Erai-raws] Re:Zero S1 - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));
        assert!(!verify_torrent_match(
            "[Erai-raws] Re:Zero S2 - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));

        // Batch/Pack verification
        assert!(verify_torrent_match(
            "[SubsPlease] Re:Zero S1 Complete [1080p]",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));
        assert!(!verify_torrent_match(
            "[SubsPlease] Re:Zero S2 Complete [1080p]",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));

        // Versioning
        assert!(verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02v2 [1080p].mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));

        // Ordinal seasons
        assert!(verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 2nd Season - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(2),
            Some(2)
        ));
        assert!(!verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 2nd Season - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));

        // Season 1 protection: S1 Episode 2 should not match S2 Episode 2
        assert!(!verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(2),
            Some(2)
        ));

        // The Chosen vs The Chosen One
        assert!(!verify_torrent_match(
            "The Chosen One S01E01 1080p",
            "The Chosen",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(!verify_torrent_match(
            "The Chosen S01E01 1080p",
            "The Chosen One",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "The Chosen S01E01 1080p",
            "The Chosen",
            None,
            None,
            Some(1),
            Some(1)
        ));

        // Shows with numbers in their name
        assert!(verify_torrent_match(
            "Zoey 101 S01E01 1080p",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey.101.S01.NTSC.DVDR-P2P",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Mob Psycho 100 S01E01 1080p",
            "Mob Psycho 100",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "9-1-1 S01E01 1080p",
            "9-1-1",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "100 Humans S01E01 1080p",
            "100 Humans",
            None,
            None,
            Some(1),
            Some(1)
        ));

        // Bitsearch search result titles regression tests
        assert!(verify_torrent_match(
            "Zoey.101.2005.S01.1080p.DVD.UPSCALED.Opus.2.0.x265-edge2020",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101 (2005) Season 1-4 S01-04 (480p AMZN WEBRIP x265 HEVC 10bit DDP 2.0 EDGE2020)",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101 2005.S01-S04+Bonus Content.AI Remaster.Complete.Series.1080p.AAC2.0.English.German.Dutch.French-Zero00",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101.S01.2005.720p.H265-Zero00",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101 2005.S01-S04.720p.H265.10bit.EAC2.0-Zero00",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101 S01 e01-13 Ita Eng by thegatto",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101.2005.S01-S04.720p.Ai Upscale.H265.10bit-Zero00",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101 s01 ENG - (traitant by mosilon)",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101 S01",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));
        assert!(verify_torrent_match(
            "Zoey 101.S01-S04.2005.576p.H265-Zero00",
            "Zoey 101",
            None,
            None,
            Some(1),
            Some(1)
        ));

        // OVA/Special exclusion for regular seasons
        assert!(!verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - OVA - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));
        assert!(!verify_torrent_match(
            "[SubsPlease] Re:Zero kara Hajimeru Isekai Seikatsu - Memory Snow (OVA) (1080p).mkv",
            show,
            None,
            None,
            Some(1),
            Some(2)
        ));

        // OVA/Special allowed for Season 0 (specials) or None (movies/general)
        assert!(verify_torrent_match(
            "[Erai-raws] Re:Zero kara Hajimeru Isekai Seikatsu - OVA - 02 [1080p].mkv",
            show,
            None,
            None,
            Some(0),
            Some(2)
        ));
    }

    #[test]
    fn test_clean_title() {
        assert_eq!(clean_title("Clarkson's Farm"), "Clarksons Farm");
        assert_eq!(clean_title("Grey's Anatomy"), "Greys Anatomy");
        assert_eq!(
            clean_title("It’s Always Sunny in Philadelphia"),
            "Its Always Sunny in Philadelphia"
        );
        assert_eq!(clean_title("Spider-Man"), "Spider Man");
        assert_eq!(clean_title("S.W.A.T."), "S W A T");
        assert_eq!(
            clean_title("Marvel's Agents of S.H.I.E.L.D."),
            "Marvels Agents of S H I E L D"
        );
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
        assert_eq!(
            decode_html_entities("Frieren&#39;s Journey"),
            "Frieren's Journey"
        );
        assert_eq!(
            decode_html_entities("Frieren&#x27;s Journey"),
            "Frieren's Journey"
        );
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
    fn test_quality_metadata_uses_token_like_markers() {
        assert_eq!(parse_quality_meta("Show.720p.WEB-DL").quality, "720p");
        assert_eq!(parse_quality_meta("Show.HDTV.x264").quality, "720p");
        assert_eq!(parse_quality_meta("TheHDClub.Release").quality, "SD");
        assert!(
            parse_quality_meta("Movie.TrueHD.Atmos.AC3")
                .details
                .contains(&"7.1".to_string())
        );
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
            extract_hash_from_magnet(
                "magnet:?xt=urn:btih:1588987db4c7d98f74fb436ad8fede1cbe9f1f63&dn=Test"
            ),
            Some("1588987db4c7d98f74fb436ad8fede1cbe9f1f63".to_string())
        );
        assert_eq!(
            extract_hash_from_magnet(
                "magnet:?xt=urn:btih:WRN7ZT6NKMA6SSXYKAFRUGDDIFJUNKI2&dn=Test"
            ),
            Some("b45bfccfcd5301e94af8500b1a1863415346a91a".to_string())
        );
    }

    #[test]
    fn test_parse_seasons_episodes() {
        assert_eq!(
            parse_seasons_episodes("Bocchi the Rock! - S01E01.mkv", &[]),
            (vec![1], vec![1], false)
        );
        assert_eq!(
            parse_seasons_episodes("02.mp4", &[]),
            (vec![], vec![2], false)
        );
        assert_eq!(
            parse_seasons_episodes("[SubsPlease] Bocchi the Rock! - 12 (1080p).mkv", &[]),
            (vec![], vec![12], false)
        );
        assert_eq!(
            parse_seasons_episodes("Bocchi the Rock! - Ep 05.mkv", &[]),
            (vec![], vec![5], false)
        );
        assert_eq!(
            parse_seasons_episodes("2x03.mkv", &[]),
            (vec![2], vec![3], false)
        );
        assert_eq!(
            parse_seasons_episodes("Clarksons Farm Season 1 Episode 2.mkv", &[]),
            (vec![1], vec![2], false)
        );
        assert_eq!(
            parse_seasons_episodes("Clarksons Farm Season 4 Episode 05.mkv", &[]),
            (vec![4], vec![5], false)
        );
        assert_eq!(
            parse_seasons_episodes("Season 1 - 02.mkv", &[]),
            (vec![1], vec![2], false)
        );

        // Multi-episode range matches
        let (s, e, pack) = parse_seasons_episodes("Bocchi the Rock! - S01E01-E10.mkv", &[]);
        assert_eq!(s, vec![1]);
        assert_eq!(e, (1..=10).collect::<Vec<u32>>());
        assert!(pack);

        // Long episode index for anime (>1000)
        let (_, e_long, _) =
            parse_seasons_episodes("[SubsPlease] One Piece - 1050 (1080p).mkv", &[]);
        assert_eq!(e_long, vec![1050]);

        // Codec and audio channel exclusion testing
        assert_eq!(
            parse_seasons_episodes("The.Chosen.S01.1080p.WEBRip.DDP5.1.Atmos.x264", &[]),
            (vec![1], vec![], true)
        );
    }

    #[test]
    fn test_parse_filename() {
        let res = parse_filename("[SubsPlease] Bocchi the Rock! - 12 (1080p).mkv", &[]);
        assert_eq!(res.base_title, "Bocchi the Rock");
        assert_eq!(res.episodes, vec![12]);
        assert_eq!(res.resolution, Some("1080p".to_string()));

        let res2 = parse_filename("Clarkson's Farm S01 Complete 1080p", &[]);
        assert_eq!(res2.base_title, "Clarkson's Farm");
        assert_eq!(res2.seasons, vec![1]);
        assert!(res2.is_pack);

        let res3 = parse_filename("Zoey.101.S01.NTSC.DVDR-P2P", &["Zoey 101"]);
        assert_eq!(res3.base_title, "Zoey 101");
        assert_eq!(res3.seasons, vec![1]);
        assert!(res3.is_pack);
    }

    #[test]
    fn test_parse_season_from_path() {
        assert_eq!(parse_season_from_path("Season 2/01.mkv"), Some(2));
        assert_eq!(parse_season_from_path("S3/01.mkv"), Some(3));
        assert_eq!(parse_season_from_path("2nd Season/01.mkv"), Some(2));
        assert_eq!(parse_season_from_path("Bocchi the Rock/01.mkv"), None);
    }
    // ---- Issue 1: older shows returning no results ---------------------------

    // Mirrors the search-query construction used in get_series_streams so we
    // can assert that a bare-title query and a year-qualified query are emitted.
    fn build_series_search_queries(
        cleaned_q: &str,
        year: Option<&str>,
        season: u32,
        episode: u32,
    ) -> Vec<String> {
        let query_exact = format!("{} S{:02}E{:02}", cleaned_q, season, episode);
        let query_season = format!("{} S{:02}", cleaned_q, season);
        let query_bare = cleaned_q.to_string();
        let mut search_queries = vec![query_exact, query_season, query_bare];
        if let Some(yr) = year {
            // Cinemeta may return a range like "2005-2008"; use only the start year.
            if let Some(start_yr) = extract_start_year(yr) {
                search_queries.push(format!("{} {}", cleaned_q, start_yr));
            }
        }
        search_queries
    }

    #[test]
    fn test_series_queries_include_bare_title_and_year() {
        let queries = build_series_search_queries("Drake and Josh", Some("2004"), 1, 1);
        assert!(
            queries.iter().any(|q| q == "Drake and Josh"),
            "bare title query missing: {:?}",
            queries
        );
        assert!(
            queries.iter().any(|q| q == "Drake and Josh 2004"),
            "year-qualified query missing: {:?}",
            queries
        );
        assert!(
            queries.iter().any(|q| q == "Drake and Josh S01E01"),
            "exact query missing: {:?}",
            queries
        );
        assert!(
            queries.iter().any(|q| q == "Drake and Josh S01"),
            "season query missing: {:?}",
            queries
        );
    }

    #[test]
    fn test_series_queries_without_year_still_has_bare_title() {
        let queries = build_series_search_queries("Zoey 101", None, 2, 5);
        assert!(
            queries.iter().any(|q| q == "Zoey 101"),
            "bare title query missing: {:?}",
            queries
        );
        assert!(
            queries.iter().any(|q| q == "Zoey 101 S02E05"),
            "exact query missing: {:?}",
            queries
        );
        assert!(
            !queries.iter().any(|q| q == "Zoey 101 2005"),
            "unexpected year query when year is None: {:?}",
            queries
        );
    }

    // A torrent named with the 1x01 convention (no SxxExx) should still verify
    // against the requested season/episode so bare-title search results are kept.
    #[test]
    fn test_verify_match_accepts_xtorrent_for_bare_query() {
        let torrent_title = "Drake and Josh 1x01 HDTV XviD-LOL";
        let accepted = verify_torrent_match(
            torrent_title,
            "Drake and Josh",
            None,
            Some("2004"),
            Some(1),
            Some(1),
        );
        assert!(accepted, "1x01 torrent should match S1E1, but was rejected");
    }

    // ---- Issue 2: commentary clip playing instead of the episode -----------

    #[test]
    fn test_is_file_match_rejects_commentary() {
        let show = "SpongeBob SquarePants";
        let commentary = "Season 01/S01E01 Commentary by the Cast.mkv";
        assert!(
            !is_file_match(commentary, 1, 1, Some(show)),
            "commentary file must NOT be selected as the episode"
        );
    }

    #[test]
    fn test_is_file_match_rejects_other_bonus_files() {
        let show = "SpongeBob SquarePants";
        for path in [
            "Season 01/Behind the Scenes.mkv",
            "Season 01/Bloopers.mkv",
            "Season 01/Featurette.mkv",
            "Season 01/Making of.mkv",
            "Season 01/Gag Reel.mkv",
            "Season 01/Outtakes.mkv",
            "Season 01/Deleted Scene.mkv",
        ] {
            assert!(
                !is_file_match(path, 1, 1, Some(show)),
                "bonus file {:?} must NOT be selected as the episode",
                path
            );
        }
    }

    #[test]
    fn test_is_file_match_accepts_real_episode() {
        let show = "SpongeBob SquarePants";
        let ep = "Season 01/SpongeBob SquarePants S01E01 Help Wanted.mkv";
        assert!(
            is_file_match(ep, 1, 1, Some(show)),
            "real episode file should be selected"
        );
    }

    #[test]
    fn test_is_file_match_rejects_sample_and_non_video() {
        let show = "SpongeBob SquarePants";
        assert!(
            !is_file_match("Season 01/Sample.mkv", 1, 1, Some(show)),
            "sample file must be rejected"
        );
        assert!(
            !is_file_match("Season 01/S01E01.nfo", 1, 1, Some(show)),
            "non-video file must be rejected"
        );
    }

    #[test]
    fn test_verify_match_rejects_single_commentary_release() {
        let torrent_title = "SpongeBob SquarePants S01E01 Commentary 1080p WEB";
        let accepted = verify_torrent_match(
            torrent_title,
            "SpongeBob SquarePants",
            None,
            None,
            Some(1),
            Some(1),
        );
        assert!(
            !accepted,
            "single-episode commentary release should be rejected"
        );
    }

    #[test]
    fn test_verify_match_accepts_pack_that_mentions_commentary() {
        // A season pack whose title happens to mention "commentary" (e.g. it
        // includes commentary tracks among real episodes) should still be
        // accepted so resolve_file_indices can pick the right file.
        let torrent_title = "SpongeBob SquarePants Season 01 1-12 Complete With Commentary 1080p";
        let accepted = verify_torrent_match(
            torrent_title,
            "SpongeBob SquarePants",
            None,
            None,
            Some(1),
            Some(1),
        );
        assert!(
            accepted,
            "pack mentioning commentary should be accepted (file-level filter handles it)"
        );
    }

    // ---- Issue 3: year-range query should use start year only --------------

    #[test]
    fn test_extract_start_year_single_year() {
        assert_eq!(extract_start_year("2005"), Some("2005".to_string()));
        assert_eq!(extract_start_year("2023"), Some("2023".to_string()));
    }

    #[test]
    fn test_extract_start_year_range_with_hyphen() {
        assert_eq!(extract_start_year("2005-2008"), Some("2005".to_string()));
    }

    #[test]
    fn test_extract_start_year_range_with_en_dash() {
        assert_eq!(extract_start_year("2005–2008"), Some("2005".to_string()));
    }

    #[test]
    fn test_extract_start_year_no_year() {
        assert_eq!(extract_start_year(""), None);
        assert_eq!(extract_start_year("N/A"), None);
    }

    #[test]
    fn test_series_queries_year_range_uses_start_year() {
        let queries = build_series_search_queries("Zoey 101", Some("2005-2008"), 1, 1);
        assert!(
            queries.iter().any(|q| q == "Zoey 101 2005"),
            "year-qualified query should use start year only: {:?}",
            queries
        );
        assert!(
            !queries.iter().any(|q| q == "Zoey 101 2005-2008"),
            "raw range query must not be emitted: {:?}",
            queries
        );
        assert!(
            queries.iter().any(|q| q == "Zoey 101"),
            "bare title query missing: {:?}",
            queries
        );
    }

    // ---- Issue 4: APIBay "no results" sentinel must be filtered -------------

    #[test]
    fn test_is_zero_info_hash() {
        assert!(is_zero_info_hash(
            "0000000000000000000000000000000000000000"
        ));
        assert!(is_zero_info_hash(
            "  0000000000000000000000000000000000000000  "
        ));
        assert!(!is_zero_info_hash(
            "4fbfc100705fed2fc483da3e11d1b4bc5ba97264"
        ));
        assert!(!is_zero_info_hash(""));
        assert!(is_valid_info_hash(
            "4fbfc100705fed2fc483da3e11d1b4bc5ba97264"
        ));
        assert!(!is_valid_info_hash("not-a-real-info-hash"));
        assert!(!is_valid_info_hash(
            "0000000000000000000000000000000000000000"
        ));
    }

    #[test]
    fn test_is_apibay_result_valid_rejects_sentinels() {
        assert!(!is_apibay_result_valid(
            "No results returned",
            "0000000000000000000000000000000000000000"
        ));
        assert!(!is_apibay_result_valid(
            "No results found",
            "0000000000000000000000000000000000000000"
        ));
        assert!(!is_apibay_result_valid(
            "Some Real Release",
            "0000000000000000000000000000000000000000"
        ));
        assert!(!is_apibay_result_valid(
            "",
            "4fbfc100705fed2fc483da3e11d1b4bc5ba97264"
        ));
        assert!(!is_apibay_result_valid("Some Real Release", ""));
    }

    #[test]
    fn test_is_apibay_result_valid_accepts_real_torrent() {
        assert!(is_apibay_result_valid(
            "Zoey 101 Season 1 Complete WEB-DL x264 [i_c]",
            "8FA30FAFE88B8516A545113E9B732FEE17D4CB06"
        ));
    }

    #[test]
    fn test_parse_torbox_files_preserves_file_ids_and_validates_hash() {
        let hash = "8FA30FAFE88B8516A545113E9B732FEE17D4CB06";
        let body = r#"{
            "success": true,
            "data": {
                "hash": "8fa30fafe88b8516a545113e9b732fee17d4cb06",
                "files": [
                    {"id": 1, "name": "Zoey 101 S01E01 Welcome To PCA.mkv", "size": 236847582},
                    {"id": 3, "name": "Zoey 101 S01E02 New Roomies.mkv", "size": 186201824}
                ]
            }
        }"#;

        let files = parse_torbox_torrent_files(body, hash).expect("valid torrent metadata");
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].index, 3);
        assert_eq!(files[1].path, "Zoey 101 S01E02 New Roomies.mkv");
        assert!(
            parse_torbox_torrent_files(body, "1111111111111111111111111111111111111111").is_none()
        );
    }

    #[test]
    fn test_series_match_requires_episode_or_pack_evidence() {
        assert!(!verify_torrent_match(
            "The Chosen 1080p WEBRip x265",
            "The Chosen",
            None,
            Some("2017"),
            Some(1),
            Some(5),
        ));
    }

    #[test]
    fn test_movie_match_rejects_non_movie_extras() {
        for release in [
            "Dune Soundtrack FLAC",
            "Dune Trailer 1080p",
            "Dune Commentary 1080p",
            "Dune Complete Collection 1080p",
        ] {
            assert!(
                !verify_torrent_match(release, "Dune", None, Some("2021"), None, None),
                "movie extra should be rejected: {release}"
            );
        }
    }

    #[test]
    fn test_title_match_does_not_accept_truncated_franchise_title() {
        assert!(!is_title_match("Star Wars", "Star Wars: The Clone Wars"));
        assert!(!is_title_match(
            "Mission Impossible",
            "Mission: Impossible - Fallout"
        ));
    }

    #[test]
    fn test_parse_chained_multi_episode_release() {
        let (seasons, episodes, is_pack) =
            parse_seasons_episodes("Some Show S01E01E02 1080p", &["Some Show"]);
        assert_eq!(seasons, vec![1]);
        assert_eq!(episodes, vec![1, 2]);
        assert!(is_pack);
    }

    #[test]
    fn test_absolute_episode_is_valid_matching_evidence() {
        assert!(verify_torrent_match_with_absolute(
            "One Piece - 1050 [1080p]",
            "One Piece",
            None,
            Some("1999"),
            Some(1),
            Some(159),
            Some(1050),
        ));
        assert!(!verify_torrent_match(
            "One Piece - 1050 [1080p]",
            "One Piece",
            None,
            Some("1999"),
            Some(1),
            Some(159),
        ));
    }

    #[test]
    fn test_file_match_reads_season_from_filename_and_absolute_episode() {
        assert!(is_file_match(
            "Show Pack/Some Show S02E03.mkv",
            2,
            3,
            Some("Some Show"),
        ));
        assert!(is_file_match_with_absolute(
            "Show Pack/Some Show - 1050.mkv",
            1,
            159,
            Some(1050),
            Some("Some Show"),
        ));
    }

    #[test]
    fn test_parse_kitsu_metadata_prefers_english_title() {
        let payload = serde_json::json!({
            "data": {
                "attributes": {
                    "canonicalTitle": "Sousou no Frieren",
                    "titles": {
                        "en": "Frieren: Beyond Journey's End",
                        "en_jp": "Sousou no Frieren"
                    },
                    "startDate": "2023-09-29"
                }
            }
        });
        assert_eq!(
            parse_kitsu_meta(&payload),
            Some((
                "Frieren: Beyond Journey's End".to_string(),
                Some("2023".to_string()),
                Some("Sousou no Frieren".to_string())
            ))
        );
    }

    #[test]
    fn test_duplicate_streams_merge_sources_and_keep_stronger_swarm() {
        let make_stream = |title: &str, source: &str| Stream {
            name: "[Bitlab] 1080p".to_string(),
            title: title.to_string(),
            url: None,
            info_hash: Some("4fbfc100705fed2fc483da3e11d1b4bc5ba97264".to_string()),
            file_idx: None,
            sources: Some(vec![source.to_string()]),
            behavior_hints: None,
        };
        let mut streams = vec![make_stream(
            "🎬 Source A: Show\n👥 2 seeders",
            "tracker:one",
        )];
        merge_stream(
            &mut streams,
            make_stream("🎬 Source B: Show\n👥 20 seeders", "tracker:two"),
        );
        assert_eq!(streams.len(), 1);
        assert!(streams[0].title.contains("20 seeders"));
        assert_eq!(streams[0].sources.as_ref().map(Vec::len), Some(2));
    }

    #[test]
    fn test_bitsearch_category_ids_are_content_specific() {
        assert_eq!(MediaCategory::Movie.bitsearch_id(), 2);
        assert_eq!(MediaCategory::Series.bitsearch_id(), 3);
        assert_eq!(MediaCategory::Anime.bitsearch_id(), 4);
    }
}
