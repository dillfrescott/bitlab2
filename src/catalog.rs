use crate::stremio::MetaPreview;
use crate::cinemeta::fetch_meta;
use std::collections::HashSet;
use regex::Regex;

// Fallback Movies if scraping fails
const FALLBACK_MOVIES: &[(&str, &str)] = &[
    ("tt0468569", "The Dark Knight"),
    ("tt1375666", "Inception"),
    ("tt0816692", "Interstellar"),
    ("tt15239678", "Dune: Part Two"),
    ("tt15398716", "Oppenheimer"),
    ("tt0111161", "The Shawshank Redemption"),
    ("tt0068646", "The Godfather"),
    ("tt0110912", "Pulp Fiction"),
    ("tt0133093", "The Matrix"),
    ("tt0137523", "Fight Club"),
    ("tt0109830", "Forrest Gump"),
    ("tt0172495", "Gladiator"),
    ("tt9362722", "Spider-Man: Across the Spider-Verse"),
    ("tt1160419", "Dune"),
    ("tt1517268", "Barbie"),
];

const FALLBACK_SHOWS: &[(&str, &str)] = &[
    ("tt0944947", "Game of Thrones"),
    ("tt0903747", "Breaking Bad"),
    ("tt1856010", "House of the Dragon"),
    ("tt3011894", "Wild Wild Country"),
    ("tt2861424", "Rick and Morty"),
    ("tt4574334", "Stranger Things"),
    ("tt6751668", "Succession"),
    ("tt1190634", "The Boys"),
    ("tt8111088", "The Mandalorian"),
    ("tt2356777", "True Detective"),
    ("tt0475778", "The Office"),
    ("tt0118301", "Friends"),
    ("tt3502216", "Peaky Blinders"),
    ("tt0303497", "Arrested Development"),
    ("tt0944835", "Modern Family"),
];

/// Get popular movies using the IMDb moviemeter scrape & Cinemeta
pub async fn get_popular_movies(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>>,
) -> Vec<MetaPreview> {
    let mut imdb_ids = Vec::new();
    
    // Attempt to scrape IMDb moviemeter
    let url = "https://www.imdb.com/chart/moviemeter/";
    let req = client.get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.9");
        
    if let Ok(resp) = req.send().await {
        if let Ok(html) = resp.text().await {
            // Find all /title/ttXXXXXXX in the HTML
            let re = Regex::new(r"/title/(tt\d{7,10})").unwrap();
            let mut seen = HashSet::new();
            for cap in re.captures_iter(&html) {
                let id = cap[1].to_string();
                if seen.insert(id.clone()) {
                    imdb_ids.push(id);
                    if imdb_ids.len() >= 25 {
                        break;
                    }
                }
            }
        }
    }

    // If scraping failed, use fallback movies
    if imdb_ids.is_empty() {
        for &(id, _) in FALLBACK_MOVIES {
            imdb_ids.push(id.to_string());
        }
    }

    // Resolve posters and details via Cinemeta in parallel
    let mut metas = Vec::new();
    let mut futures = Vec::new();

    for id in imdb_ids {
        let client_clone = client.clone();
        let meta_cache_clone = meta_cache.clone();
        futures.push(tokio::spawn(async move {
            let fetch_fut = fetch_meta(&client_clone, "movie", &id);
            if let Ok(Some(meta)) = tokio::time::timeout(std::time::Duration::from_millis(2500), fetch_fut).await {
                // Populate the meta cache for future streaming requests
                {
                    let mut cache = meta_cache_clone.write().await;
                    cache.insert(id.clone(), (meta.name.clone(), meta.year.clone()));
                }
                return Some(MetaPreview {
                    id: id.clone(),
                    r#type: "movie".to_string(),
                    name: meta.name,
                    poster: meta.poster,
                    poster_shape: Some("poster".to_string()),
                    release_info: meta.year,
                    imdb_rating: meta.imdb_rating,
                    description: meta.description,
                });
            }
            None
        }));
    }

    // Await all futures
    for fut in futures {
        if let Ok(Some(meta)) = fut.await {
            metas.push(meta);
        }
    }

    // Fallback basic info if Cinemeta failed completely
    if metas.is_empty() {
        for &(id, name) in FALLBACK_MOVIES {
            metas.push(MetaPreview {
                id: id.to_string(),
                r#type: "movie".to_string(),
                name: name.to_string(),
                poster: None,
                poster_shape: Some("poster".to_string()),
                release_info: None,
                imdb_rating: None,
                description: Some("Curated popular movie.".to_string()),
            });
        }
    }

    metas
}

/// Scrapes popular TV shows from IMDb tvmeter or falls back to a curated list,
/// then fetches their metadata from Cinemeta to populate details
pub async fn get_popular_series(
    client: &reqwest::Client,
    meta_cache: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, (String, Option<String>)>>>,
) -> Vec<MetaPreview> {
    let mut imdb_ids = Vec::new();
    
    // Attempt to scrape IMDb tvmeter
    let url = "https://www.imdb.com/chart/tvmeter/";
    let req = client.get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.9");
        
    if let Ok(resp) = req.send().await {
        if let Ok(html) = resp.text().await {
            // Find all /title/ttXXXXXXX in the HTML
            let re = Regex::new(r"/title/(tt\d{7,10})").unwrap();
            let mut seen = HashSet::new();
            for cap in re.captures_iter(&html) {
                let id = cap[1].to_string();
                if seen.insert(id.clone()) {
                    imdb_ids.push(id);
                    if imdb_ids.len() >= 25 {
                        break;
                    }
                }
            }
        }
    }

    // If scraping failed or returned nothing, use our fallback curated list
    if imdb_ids.is_empty() {
        for &(id, _) in FALLBACK_SHOWS {
            imdb_ids.push(id.to_string());
        }
    }

    // Resolve posters and details via Cinemeta in parallel
    let mut metas = Vec::new();
    let mut futures = Vec::new();

    for id in imdb_ids {
        let client_clone = client.clone();
        let meta_cache_clone = meta_cache.clone();
        futures.push(tokio::spawn(async move {
            let fetch_fut = fetch_meta(&client_clone, "series", &id);
            if let Ok(Some(meta)) = tokio::time::timeout(std::time::Duration::from_millis(2500), fetch_fut).await {
                // Populate the meta cache for future streaming requests
                {
                    let mut cache = meta_cache_clone.write().await;
                    cache.insert(id.clone(), (meta.name.clone(), meta.year.clone()));
                }
                return Some(MetaPreview {
                    id: id.clone(),
                    r#type: "series".to_string(),
                    name: meta.name,
                    poster: meta.poster,
                    poster_shape: Some("poster".to_string()),
                    release_info: meta.year,
                    imdb_rating: meta.imdb_rating,
                    description: meta.description,
                });
            }
            None
        }));
    }

    // Await all futures
    for fut in futures {
        if let Ok(Some(meta)) = fut.await {
            metas.push(meta);
        }
    }

    // If meta fetching failed entirely (e.g. no internet/Cinemeta down), return basic info
    if metas.is_empty() {
        for &(id, name) in FALLBACK_SHOWS {
            metas.push(MetaPreview {
                id: id.to_string(),
                r#type: "series".to_string(),
                name: name.to_string(),
                poster: None,
                poster_shape: Some("poster".to_string()),
                release_info: None,
                imdb_rating: None,
                description: Some("Curated popular television series.".to_string()),
            });
        }
    }

    metas
}
