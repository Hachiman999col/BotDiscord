use std::time::Duration;

use serde::Deserialize;

/// Lo que el bot declara al autenticarse contra Jellyfin.
const CLIENT_NAME: &str = "BotDiscord";
const DEVICE_ID: &str = "botdiscord";
const VERSION: &str = "1.0.0";
/// Jellyfin 12 rechaza las cabeceras viejas `X-Emby-*`: todo va por `Authorization`.
const AUTHORIZATION_HEADER: &str = "Authorization";

/// Cliente autenticado contra el servidor de Jellyfin definido en el `.env`.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    user_id: String,
    user_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: Option<String>,
    pub year: Option<i64>,
    pub tracks: Option<u64>,
}

impl Album {
    /// Texto que se muestra en Discord: "Nombre — Artista (2019) · 12 canciones".
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if let Some(artist) = &self.artist {
            label.push_str(&format!(" — {artist}"));
        }
        if let Some(year) = self.year {
            label.push_str(&format!(" ({year})"));
        }
        if let Some(tracks) = self.tracks {
            label.push_str(&format!(" · {tracks} canciones"));
        }
        label
    }

    /// Acepta el nombre del álbum, el artista, o "artista - álbum".
    fn matches(&self, needle: &str) -> bool {
        let name = self.name.to_lowercase();
        let artist = self.artist.as_deref().unwrap_or_default().to_lowercase();

        if name.contains(needle) || (!artist.is_empty() && artist.contains(needle)) {
            return true;
        }

        match needle.split_once(" - ") {
            Some((needle_artist, needle_album)) => {
                let needle_artist = needle_artist.trim();
                let needle_album = needle_album.trim();
                !needle_album.is_empty()
                    && name.contains(needle_album)
                    && (artist.contains(needle_artist) || needle_artist.is_empty())
            }
            None => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub duration: Option<String>,
}

impl Client {
    /// Lee `URL_JELLYFIN`, `JELLYFIN_USER` y `JELLYFIN_PASSWORD` del entorno y abre sesión.
    pub async fn connect_from_env() -> Result<Self, String> {
        let base = std::env::var("URL_JELLYFIN")
            .map_err(|_| "falta URL_JELLYFIN en el .env".to_owned())?;
        let user = std::env::var("JELLYFIN_USER")
            .map_err(|_| "falta JELLYFIN_USER en el .env".to_owned())?;
        let password = std::env::var("JELLYFIN_PASSWORD")
            .map_err(|_| "falta JELLYFIN_PASSWORD en el .env".to_owned())?;

        let base = base.trim().trim_end_matches('/').to_owned();
        if base.is_empty() {
            return Err("URL_JELLYFIN está vacía en el .env".to_owned());
        }

        let http = reqwest::Client::builder()
            .user_agent("BotDiscord/1.0.0")
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|err| format!("no se pudo crear el cliente HTTP: {err}"))?;

        // Jellyfin acepta "Pw" en las versiones nuevas y "Password" en las viejas.
        let body = serde_json::json!({
            "Username": user,
            "Pw": password,
            "Password": password,
        });

        let response = http
            .post(format!("{base}/Users/AuthenticateByName"))
            .header(AUTHORIZATION_HEADER, media_browser_header())
            .json(&body)
            .send()
            .await
            .map_err(|err| format!("no se pudo conectar a {base}: {err}"))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("login rechazado ({status}): {}", short_body(&text)));
        }

        let auth: AuthResponse = serde_json::from_str(&text)
            .map_err(|err| format!("respuesta inesperada al iniciar sesión: {err}"))?;

        Ok(Self {
            http,
            base,
            token: auth.access_token,
            user_id: auth.user.id,
            user_name: auth.user.name,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn user_name(&self) -> &str {
        &self.user_name
    }

    pub async fn albums(&self) -> Result<Vec<Album>, String> {
        let response: ItemsResponse<RawAlbum> = self
            .get(
                "/Items",
                &[
                    ("IncludeItemTypes", "MusicAlbum".to_owned()),
                    ("Recursive", "true".to_owned()),
                    ("SortBy", "SortName".to_owned()),
                    ("UserId", self.user_id.clone()),
                    (
                        "Fields",
                        "AlbumArtist,ChildCount,RecursiveItemCount".to_owned(),
                    ),
                    ("Limit", "5000".to_owned()),
                ],
            )
            .await?;

        let mut albums: Vec<Album> = response.items.into_iter().map(Album::from).collect();
        albums.sort_by_key(|album| album.name.to_lowercase());
        Ok(albums)
    }

    /// Álbumes que coinciden con el id de `list`, el nombre o el artista; devuelve su número de lista.
    pub async fn find_albums(&self, query: &str) -> Result<Vec<(usize, Album)>, String> {
        let albums = self.albums().await?;

        Ok(match_albums(&albums, query)
            .into_iter()
            .map(|index| (index + 1, albums[index].clone()))
            .collect())
    }

    pub async fn tracks(&self, album_id: &str) -> Result<Vec<Track>, String> {
        let response: ItemsResponse<RawTrack> = self
            .get(
                "/Items",
                &[
                    ("ParentId", album_id.to_owned()),
                    ("IncludeItemTypes", "Audio".to_owned()),
                    ("Recursive", "true".to_owned()),
                    (
                        "SortBy",
                        "ParentIndexNumber,IndexNumber,SortName".to_owned(),
                    ),
                    ("UserId", self.user_id.clone()),
                    ("Fields", "RunTimeTicks".to_owned()),
                ],
            )
            .await?;

        Ok(response.items.into_iter().map(Track::from).collect())
    }

    /// URL y cabeceras para que songbird reciba el audio del tema ya transcodificado a mp3.
    pub fn stream_request(&self, track_id: &str) -> (String, Vec<(String, String)>) {
        let url = format!(
            "{}/Audio/{track_id}/stream.mp3?static=false&DeviceId={DEVICE_ID}",
            self.base
        );
        (
            url,
            vec![
                (AUTHORIZATION_HEADER.to_owned(), token_header(&self.token)),
                ("User-Agent".to_owned(), "BotDiscord/1.0.0".to_owned()),
            ],
        )
    }

    async fn get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, String> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header(AUTHORIZATION_HEADER, token_header(&self.token))
            .query(query)
            .send()
            .await
            .map_err(|err| format!("Jellyfin no respondió en {path}: {err}"))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Jellyfin respondió {status} en {path}: {}",
                short_body(&body)
            ));
        }

        serde_json::from_str(&body)
            .map_err(|err| format!("respuesta inesperada de Jellyfin en {path}: {err}"))
    }
}

fn media_browser_header() -> String {
    format!(
        "MediaBrowser Client=\"{CLIENT_NAME}\", Device=\"{CLIENT_NAME}\", DeviceId=\"{DEVICE_ID}\", Version=\"{VERSION}\""
    )
}

fn token_header(token: &str) -> String {
    format!("MediaBrowser Token=\"{token}\"")
}

fn short_body(body: &str) -> String {
    body.trim().chars().take(200).collect()
}

#[derive(Deserialize)]
struct AuthResponse {
    #[serde(rename = "AccessToken")]
    access_token: String,
    #[serde(rename = "User")]
    user: AuthUser,
}

#[derive(Deserialize)]
struct AuthUser {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name", default)]
    name: String,
}

#[derive(Deserialize)]
struct ItemsResponse<T> {
    #[serde(rename = "Items", default = "Vec::new")]
    items: Vec<T>,
}

#[derive(Deserialize)]
struct RawAlbum {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "AlbumArtist", default)]
    album_artist: Option<String>,
    #[serde(rename = "Artists", default)]
    artists: Vec<String>,
    #[serde(rename = "ProductionYear", default)]
    year: Option<i64>,
    #[serde(rename = "ChildCount", default)]
    tracks: Option<u64>,
    #[serde(rename = "RecursiveItemCount", default)]
    recursive_tracks: Option<u64>,
}

impl From<RawAlbum> for Album {
    fn from(raw: RawAlbum) -> Self {
        let artist = raw
            .album_artist
            .filter(|artist| !artist.trim().is_empty())
            .or_else(|| raw.artists.into_iter().find(|artist| !artist.is_empty()));

        Self {
            id: raw.id,
            name: raw.name,
            artist,
            year: raw.year,
            tracks: raw.recursive_tracks.or(raw.tracks),
        }
    }
}

/// Índices (0-based) de los álbumes que coinciden, priorizando el número de la lista.
fn match_albums(albums: &[Album], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();

    if let Ok(position) = needle.parse::<usize>() {
        if (1..=albums.len()).contains(&position) {
            return vec![position - 1];
        }
    }

    let exact: Vec<usize> = albums
        .iter()
        .enumerate()
        .filter(|(_, album)| album.name.to_lowercase() == needle)
        .map(|(index, _)| index)
        .collect();
    if !exact.is_empty() {
        return exact;
    }

    albums
        .iter()
        .enumerate()
        .filter(|(_, album)| album.matches(&needle))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Deserialize)]
struct RawTrack {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "RunTimeTicks", default)]
    run_time_ticks: Option<i64>,
}

impl From<RawTrack> for Track {
    fn from(raw: RawTrack) -> Self {
        Self {
            id: raw.id,
            title: raw.name,
            duration: format_ticks(raw.run_time_ticks),
        }
    }
}

/// Jellyfin mide la duración en "ticks" de 100 ns.
fn format_ticks(ticks: Option<i64>) -> Option<String> {
    let ticks = ticks.filter(|ticks| *ticks > 0)?;
    let seconds = ticks / 10_000_000;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);

    Some(if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(name: &str, artist: &str) -> Album {
        Album {
            id: "id".to_owned(),
            name: name.to_owned(),
            artist: Some(artist.to_owned()),
            year: Some(2019),
            tracks: Some(12),
        }
    }

    #[test]
    fn arma_la_linea_del_album() {
        assert_eq!(
            album("Discovery", "Daft Punk").label(),
            "Discovery — Daft Punk (2019) · 12 canciones"
        );
    }

    #[test]
    fn busca_por_album_artista_o_ambos() {
        let discovery = album("Discovery", "Daft Punk");
        assert!(discovery.matches("disco"));
        assert!(discovery.matches("daft"));
        assert!(discovery.matches("daft punk - discovery"));
        assert!(!discovery.matches("random access memories"));
    }

    #[test]
    fn busca_albumes_por_numero_nombre_o_artista() {
        let albums = vec![
            album("Discovery", "Daft Punk"),
            album("Random Access Memories", "Daft Punk"),
        ];

        assert_eq!(match_albums(&albums, "2"), vec![1]);
        assert_eq!(match_albums(&albums, " discovery "), vec![0]);
        assert_eq!(match_albums(&albums, "daft punk - random"), vec![1]);
        assert_eq!(match_albums(&albums, "daft"), vec![0, 1]);
        assert!(match_albums(&albums, "inexistente").is_empty());
        assert!(match_albums(&albums, "99").is_empty());
    }

    #[test]
    fn formatea_la_duracion() {
        assert_eq!(format_ticks(Some(2_130_000_000)), Some("3:33".to_owned()));
        assert_eq!(
            format_ticks(Some(37_230_000_000)),
            Some("1:02:03".to_owned())
        );
        assert_eq!(format_ticks(None), None);
        assert_eq!(format_ticks(Some(0)), None);
    }
}
