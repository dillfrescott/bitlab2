use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CinemetaMeta {
    pub name: String,
    pub year: Option<String>,
    pub poster: Option<String>,
    pub description: Option<String>,
    pub imdb_rating: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CinemetaResponse {
    pub meta: Option<CinemetaMeta>,
}

pub async fn fetch_meta(
    client: &reqwest::Client,
    r#type: &str,
    imdb_id: &str,
) -> Option<CinemetaMeta> {
    let url = format!("https://v3-cinemeta.strem.io/meta/{}/{}.json", r#type, imdb_id);
    let resp = client.get(&url).send().await.ok()?;
    let data: CinemetaResponse = resp.json().await.ok()?;
    data.meta
}
