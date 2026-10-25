use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serenity::{
    all::{
        ChannelId, Context, CreateEmbed, CreateEmbedFooter, EditMessage, GuildId, MessageId, UserId,
    },
    async_trait,
};
use songbird::{
    events::{Event, EventContext, EventHandler as VoiceEventHandler, TrackEvent},
    input::{HttpRequest, YoutubeDl},
    tracks::{PlayError, PlayMode, TrackHandle},
    Songbird,
};
use tokio::sync::Mutex;

use crate::jellyfin;

const MAX_SONGS_IN_BANNER: usize = 10;
const MAX_QUEUE_LEN: usize = 10;
const BANNER_COLOR: u32 = 0x5865F2;
/// Cada cuánto se edita el banner para refrescar el progreso de la canción.
const BANNER_REFRESH: Duration = Duration::from_secs(10);
const PROGRESS_BAR_WIDTH: usize = 12;
/// Cliente de YouTube con el que se reintenta cuando YouTube pide verificación anti-bot.
const RETRY_CLIENT: &str = "web_embedded";
const FULL_QUEUE_TEXT: &str =
    "La cola ya tiene 10 canciones de YouTube. Esperá a que termine alguna o borrá una con `yt delete`.";

/// De dónde sale el audio de una canción.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    YouTube,
    Jellyfin,
}

/// Lo necesario para reproducir una canción y mostrarla en el banner.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Song {
    url: String,
    title: String,
    duration: Option<String>,
    /// Duración en segundos, para dibujar la barra de progreso.
    duration_secs: Option<u64>,
    origin: Origin,
    /// Cabeceras extra para pedir el audio (Jellyfin usa `Authorization`).
    headers: Vec<(String, String)>,
    /// Cliente de YouTube forzado en yt-dlp; solo se usa al reintentar un fallo.
    ytdlp_client: Option<String>,
}

impl Song {
    fn youtube(url: &str, title: Option<String>, duration: Option<String>) -> Self {
        let duration_secs = duration.as_deref().and_then(parse_duration);
        Self {
            url: url.to_owned(),
            title: title.unwrap_or_else(|| url.to_owned()),
            duration,
            duration_secs,
            origin: Origin::YouTube,
            headers: Vec::new(),
            ytdlp_client: None,
        }
    }

    fn jellyfin(
        url: String,
        headers: Vec<(String, String)>,
        title: String,
        duration: Option<String>,
    ) -> Self {
        let duration_secs = duration.as_deref().and_then(parse_duration);
        Self {
            url,
            title,
            duration,
            duration_secs,
            origin: Origin::Jellyfin,
            headers,
            ytdlp_client: None,
        }
    }
}

/// "3:33" -> 213 y "1:02:03" -> 3723.
fn parse_duration(text: &str) -> Option<u64> {
    let mut seconds = 0u64;
    let mut parts = 0;

    for part in text.trim().split(':') {
        let value: u64 = part.trim().parse().ok()?;
        seconds = seconds * 60 + value;
        parts += 1;
    }

    (parts > 0).then_some(seconds)
}

fn format_secs(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Barra tipo `▬▬▬🔘▭▭▭` con la posición actual.
fn progress_bar(position: u64, total: u64) -> String {
    let ratio = if total == 0 {
        0.0
    } else {
        (position as f64 / total as f64).clamp(0.0, 1.0)
    };
    let knob = ((ratio * PROGRESS_BAR_WIDTH as f64).round() as usize).min(PROGRESS_BAR_WIDTH);

    (0..PROGRESS_BAR_WIDTH)
        .map(|index| {
            if index < knob {
                '▬'
            } else if index == knob && ratio < 1.0 {
                '🔘'
            } else {
                '▭'
            }
        })
        .collect()
}

/// Runtime de JavaScript que yt-dlp usa para extraer YouTube (solo detecta `deno` por su cuenta).
/// Sin uno, la extracción queda degradada y algunos videos no se pueden bajar.
fn js_runtime() -> Option<&'static str> {
    static RUNTIME: OnceLock<Option<&'static str>> = OnceLock::new();

    *RUNTIME.get_or_init(|| {
        let dirs: Vec<_> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
        ["deno", "node", "bun"]
            .into_iter()
            .find(|program| dirs.iter().any(|dir| dir.join(program).is_file()))
    })
}

/// Argumentos extra para yt-dlp: runtime de JavaScript y cookies, si están configurados.
fn ytdlp_args() -> Vec<String> {
    let mut args = js_runtime()
        .map(|runtime| vec!["--js-runtimes".to_owned(), runtime.to_owned()])
        .unwrap_or_default();
    args.extend(cookie_args());
    args
}

/// Cookies de YouTube para yt-dlp, leídas del `.env` (`YTDLP_COOKIES` o
/// `YTDLP_COOKIES_FROM_BROWSER`). Ayudan cuando YouTube pide iniciar sesión.
fn cookie_args() -> Vec<String> {
    cookie_args_for(
        std::env::var("YTDLP_COOKIES").ok().as_deref(),
        std::env::var("YTDLP_COOKIES_FROM_BROWSER").ok().as_deref(),
    )
}

/// El archivo de cookies tiene prioridad: es el que no depende del navegador del host.
fn cookie_args_for(cookies_file: Option<&str>, from_browser: Option<&str>) -> Vec<String> {
    let file = cookies_file
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let browser = from_browser
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(file) = file {
        vec!["--cookies".to_owned(), file.to_owned()]
    } else if let Some(browser) = browser {
        vec!["--cookies-from-browser".to_owned(), browser.to_owned()]
    } else {
        Vec::new()
    }
}

/// Fuerza el cliente de YouTube de yt-dlp (se usa al reintentar un fallo anti-bot).
fn client_args(song: &Song) -> Vec<String> {
    song.ytdlp_client
        .as_ref()
        .map(|client| {
            vec![
                "--extractor-args".to_owned(),
                format!("youtube:player_client={client}"),
            ]
        })
        .unwrap_or_default()
}

/// Construye la entrada de audio según el origen: yt-dlp o el stream HTTP de Jellyfin.
fn input_for(song: &Song) -> songbird::input::Input {
    match song.origin {
        Origin::YouTube => {
            let mut args = ytdlp_args();
            args.extend(client_args(song));
            YoutubeDl::new(http_client(), song.url.clone())
                .user_args(args)
                .into()
        }
        Origin::Jellyfin => jellyfin_input(song.url.clone(), song.headers.clone()),
    }
}

/// Entrada HTTP con las cabeceras de Jellyfin.
fn jellyfin_input(url: String, headers: Vec<(String, String)>) -> songbird::input::Input {
    let mut map = HeaderMap::new();
    for (name, value) in &headers {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(name), Ok(value)) => {
                map.insert(name, value);
            }
            _ => eprintln!("[jmusic] cabecera inválida al pedir el audio: {name}"),
        }
    }

    HttpRequest::new_with_headers(http_client(), url, map).into()
}

fn song_label(song: &Song) -> String {
    match &song.duration {
        Some(duration) => format!("{} · {duration}", song.title),
        None => song.title.clone(),
    }
}

/// Qué se está reproduciendo en un servidor y qué queda en la cola.
#[derive(Default)]
pub struct GuildQueue {
    current: Option<Song>,
    pending: VecDeque<Song>,
    current_handle: Option<TrackHandle>,
    paused: bool,
    /// Cuándo empezó la canción actual y cuánto estuvo pausada, para el progreso.
    started_at: Option<Instant>,
    paused_at: Option<Instant>,
    paused_total: Duration,
    /// Último banner enviado: se edita para mostrar el progreso.
    banner: Option<(ChannelId, MessageId)>,
    /// Motivo del último fallo de reproducción, para mostrarlo en el banner.
    failure: Option<String>,
    /// Identifica la reproducción en curso; el actualizador del banner se detiene al cambiar.
    session: u64,
}

impl GuildQueue {
    fn position_secs(&self) -> u64 {
        let Some(started) = self.started_at else {
            return 0;
        };

        let reference = self.paused_at.unwrap_or_else(Instant::now);
        reference
            .saturating_duration_since(started)
            .saturating_sub(self.paused_total)
            .as_secs()
    }

    fn reset_timing(&mut self) {
        self.started_at = Some(Instant::now());
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
        self.paused = false;
    }

    /// Canciones de YouTube en la cola: son las únicas que cuentan para el tope.
    fn youtube_pending(&self) -> usize {
        self.pending
            .iter()
            .filter(|song| song.origin == Origin::YouTube)
            .count()
    }
}

/// Guarda el banner que se va a refrescar con el progreso de la canción.
pub async fn remember_banner(
    queues: &Queues,
    guild_id: GuildId,
    channel_id: ChannelId,
    message_id: MessageId,
) {
    if let Some(state) = queues.lock().await.get_mut(&guild_id) {
        state.banner = Some((channel_id, message_id));
    }
}

pub type Queues = Arc<Mutex<HashMap<GuildId, GuildQueue>>>;

pub fn new_queues() -> Queues {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Cliente HTTP compartido por todas las descargas de audio.
fn http_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new).clone()
}

#[derive(Default)]
struct SongMeta {
    title: Option<String>,
    duration: Option<String>,
}

/// Pide a yt-dlp el título y la duración del video. Si falla, el banner muestra la URL.
async fn resolve_metadata(url: &str) -> SongMeta {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new("yt-dlp")
            .args(ytdlp_args())
            .args([
                "--no-playlist",
                "--print",
                "%(title)s\t%(duration_string)s",
                url,
            ])
            .output(),
    )
    .await;

    let output = match output {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(Ok(output)) => {
            eprintln!(
                "[yt] no se pudo obtener el título de {url}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return SongMeta::default();
        }
        Ok(Err(err)) => {
            eprintln!("[yt] no se pudo ejecutar yt-dlp para el título: {err}");
            return SongMeta::default();
        }
        Err(err) => {
            eprintln!("[yt] yt-dlp tardó demasiado en devolver los datos de {url}: {err}");
            return SongMeta::default();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or_default();
    let (title, duration) = line.split_once('\t').unwrap_or((line, ""));

    let clean = |value: &str| {
        let value = value.trim();
        (!value.is_empty() && value != "NA").then(|| value.to_owned())
    };

    SongMeta {
        title: clean(title),
        duration: clean(duration),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum YtCommand<'a> {
    Add(&'a str),
    Pause,
    Next,
    ShowQueue,
    Delete(Option<usize>),
    Exit,
    Help,
    Usage,
}

const HELP_TEXT: &str = "**Comandos de yt**\n\
`yt <url>` — reproduce el link o lo agrega a la cola\n\
`yt pause` — pausa o reanuda la canción actual\n\
`yt next` — pasa a la siguiente canción (si no quedan, desconecta)\n\
`yt queue` — muestra la cola de reproducción actual\n\
`yt delete [posición]` — quita una canción de la cola\n\
`yt exit` — corta la música y desconecta el bot\n\
`yt help` — muestra esta lista\n\n\
La cola admite hasta 10 canciones de YouTube (los álbumes de Jellyfin no cuentan para el tope).\n\n\
En Discord son los mismos comandos con barra: `/yt play`, `/yt pause`, `/yt next`, `/yt queue`, `/yt delete`, `/yt exit`, `/yt help`.";

/// Interpreta el mensaje completo: "yt <url>", "yt pause", "yt delete 2", "yt exit".
pub fn parse_command(content: &str) -> YtCommand<'_> {
    let args = content.strip_prefix("yt").unwrap_or_default().trim();
    if args.is_empty() {
        return YtCommand::Usage;
    }

    let (sub, rest) = match args.split_once(char::is_whitespace) {
        Some((sub, rest)) => (sub, rest.trim()),
        None => (args, ""),
    };

    match sub {
        "pause" => YtCommand::Pause,
        "next" => YtCommand::Next,
        "queue" => YtCommand::ShowQueue,
        "help" => YtCommand::Help,
        "delete" => YtCommand::Delete(rest.parse().ok()),
        "exit" => YtCommand::Exit,
        url if url.starts_with("http://") || url.starts_with("https://") => YtCommand::Add(url),
        _ => YtCommand::Usage,
    }
}

/// Quién pide el comando: sirve igual para mensajes (`yt ...`) y para slash commands (`/yt`).
pub struct MusicCaller {
    pub guild_id: Option<GuildId>,
    pub user_id: UserId,
}

/// Comandos de los servicios de música de Jellyfin: `jmusic list`, `jmusic <álbum>`, etc.
#[derive(Debug, PartialEq, Eq)]
pub enum JmusicCommand<'a> {
    List,
    Play(&'a str),
    Pause,
    Next,
    Exit,
    Usage,
}

const JMUSIC_HELP: &str = "**Comandos de jmusic**\n\
`jmusic list` — muestra todos los álbumes de Jellyfin con su id\n\
`jmusic <id|álbum>` — busca el álbum por id o nombre y reproduce sus canciones\n\
`jmusic pause` — pausa o reanuda la canción actual\n\
`jmusic next` — pasa a la siguiente canción (si no quedan, desconecta)\n\
`jmusic exit` — corta la música y desconecta el bot\n\n\
La cola admite hasta 10 canciones de YouTube (los álbumes de Jellyfin no cuentan para el tope).\n\n\
En Discord son los mismos comandos con barra: `/jmusic list`, `/jmusic play`, `/jmusic pause`, `/jmusic next`, `/jmusic exit`.";

/// Interpreta el mensaje completo: "jmusic list", "jmusic Discovery", "jmusic pause".
pub fn parse_jmusic_command(content: &str) -> JmusicCommand<'_> {
    let args = content.strip_prefix("jmusic").unwrap_or_default().trim();
    if args.is_empty() {
        return JmusicCommand::Usage;
    }

    if args.eq_ignore_ascii_case("list") {
        JmusicCommand::List
    } else if args.eq_ignore_ascii_case("pause") {
        JmusicCommand::Pause
    } else if args.eq_ignore_ascii_case("next") {
        JmusicCommand::Next
    } else if args.eq_ignore_ascii_case("exit") {
        JmusicCommand::Exit
    } else {
        JmusicCommand::Play(args)
    }
}

fn jellyfin_or_error(
    jellyfin: &Result<jellyfin::Client, String>,
) -> Result<&jellyfin::Client, MusicResponse> {
    jellyfin.as_ref().map_err(|err| {
        eprintln!("[jmusic] Jellyfin no está disponible: {err}");
        MusicResponse::text(format!("Jellyfin no está configurado: {err}"))
    })
}

pub async fn run_jmusic(
    ctx: &Context,
    caller: MusicCaller,
    command: JmusicCommand<'_>,
    jellyfin: &Result<jellyfin::Client, String>,
    queues: &Queues,
) -> MusicResponse {
    match command {
        JmusicCommand::Pause => pause(&caller, queues).await,
        JmusicCommand::Next => next(&caller, queues).await,
        JmusicCommand::Exit => exit(ctx, &caller, queues).await,
        JmusicCommand::Usage => MusicResponse::text(JMUSIC_HELP),
        JmusicCommand::List => match jellyfin_or_error(jellyfin) {
            Ok(client) => list_albums(client).await,
            Err(response) => response,
        },
        JmusicCommand::Play(query) => match jellyfin_or_error(jellyfin) {
            Ok(client) => play_album(ctx, &caller, query, client, queues).await,
            Err(response) => response,
        },
    }
}

/// Lo que el bot tiene que responder, sin importar si vino de un mensaje o de una interacción.
pub enum MusicResponse {
    Text(String),
    Banner {
        content: String,
        embed: Box<CreateEmbed>,
    },
}

impl MusicResponse {
    fn text(content: impl Into<String>) -> Self {
        Self::Text(content.into())
    }

    fn banner(content: impl Into<String>, state: &GuildQueue) -> Self {
        Self::Banner {
            content: content.into(),
            embed: Box::new(banner(state)),
        }
    }
}

pub async fn run(
    ctx: &Context,
    caller: MusicCaller,
    command: YtCommand<'_>,
    queues: &Queues,
) -> MusicResponse {
    match command {
        YtCommand::Add(url) => add(ctx, &caller, url, queues).await,
        YtCommand::Pause => pause(&caller, queues).await,
        YtCommand::Next => next(&caller, queues).await,
        YtCommand::ShowQueue => show_queue(&caller, queues).await,
        YtCommand::Delete(index) => delete(&caller, index, queues).await,
        YtCommand::Exit => exit(ctx, &caller, queues).await,
        YtCommand::Help | YtCommand::Usage => MusicResponse::text(HELP_TEXT),
    }
}

async fn add(ctx: &Context, caller: &MusicCaller, url: &str, queues: &Queues) -> MusicResponse {
    println!("[yt] comando recibido: {url}");

    let Some(guild_id) = caller.guild_id else {
        return MusicResponse::text("Los comandos de música solo funcionan dentro de un servidor");
    };

    // Se revisa antes de pedir metadatos para no gastar una consulta a yt-dlp.
    if let Some(response) = full_queue_response(queues, guild_id).await {
        println!("[yt] cola llena ({MAX_QUEUE_LEN} canciones), no se agrega: {url}");
        return response;
    }

    let meta = resolve_metadata(url).await;
    println!(
        "[yt] título: {} | duración: {}",
        meta.title.as_deref().unwrap_or("<no disponible>"),
        meta.duration.as_deref().unwrap_or("<no disponible>")
    );

    let song = Song::youtube(url, meta.title, meta.duration);
    let label = song.title.clone();
    play_songs(ctx, caller, vec![song], label, true, queues).await
}

/// Aviso con el banner cuando la cola ya no acepta más links de YouTube.
async fn full_queue_response(queues: &Queues, guild_id: GuildId) -> Option<MusicResponse> {
    let queues_guard = queues.lock().await;
    let state = queues_guard.get(&guild_id)?;
    (state.current.is_some() && state.youtube_pending() >= MAX_QUEUE_LEN)
        .then(|| MusicResponse::banner(FULL_QUEUE_TEXT, state))
}

/// Canal de voz del usuario: primero el cache y, si está atrasado, la API.
async fn voice_channel_for(
    ctx: &Context,
    caller: &MusicCaller,
    guild_id: GuildId,
) -> Option<ChannelId> {
    let guild_in_cache = ctx.cache.guild(guild_id).is_some();
    let cached_channel = ctx.cache.guild(guild_id).and_then(|guild| {
        guild
            .voice_states
            .get(&caller.user_id)
            .and_then(|state| state.channel_id)
    });
    println!("[music] servidor en cache: {guild_in_cache}, canal en cache: {cached_channel:?}");

    if let Some(channel) = cached_channel {
        return Some(channel);
    }

    match ctx
        .http
        .get_user_voice_state(guild_id, caller.user_id)
        .await
    {
        Ok(state) => {
            println!("[music] canal según la API: {:?}", state.channel_id);
            state.channel_id
        }
        Err(err) => {
            eprintln!("[music] no se pudo consultar el estado de voz en la API: {err}");
            None
        }
    }
}

/// Se une al canal de voz y arranca la canción actual de la cola.
async fn start_current(
    ctx: &Context,
    guild_id: GuildId,
    voice_channel: ChannelId,
    queues: &Queues,
) -> Result<(), String> {
    let Some(manager) = songbird::get(ctx).await else {
        eprintln!("[music] songbird no está registrado en el cliente");
        return Err("El sistema de voz no está inicializado en el bot".to_owned());
    };

    let song = {
        let queues_guard = queues.lock().await;
        queues_guard
            .get(&guild_id)
            .and_then(|state| state.current.clone())
    }
    .ok_or_else(|| "No se pudo guardar el estado de la cola".to_owned())?;

    let call = manager.join(guild_id, voice_channel).await.map_err(|err| {
        eprintln!("[music] no se pudo unir al canal de voz {voice_channel}: {err}");
        format!("No me pude conectar al canal de voz: {err}")
    })?;
    println!("[music] conectado al canal de voz {voice_channel}");

    let handle = {
        let mut call = call.lock().await;
        let handle = call.play_input(input_for(&song));

        call.add_global_event(
            Event::Track(TrackEvent::End),
            TrackEnd {
                ctx: ctx.clone(),
                manager,
                guild_id,
                queues: queues.clone(),
            },
        );

        handle
    };

    let session = {
        let mut queues_guard = queues.lock().await;
        let Some(state) = queues_guard.get_mut(&guild_id) else {
            return Err("No se pudo guardar el estado de la cola".to_owned());
        };

        state.current_handle = Some(handle);
        state.reset_timing();
        state.session += 1;
        state.session
    };

    spawn_banner_updater(ctx.clone(), guild_id, queues.clone(), session);

    println!("[music] reproduciendo: {} ({:?})", song.title, song.origin);
    Ok(())
}

/// Edita el último banner cada `BANNER_REFRESH` para mostrar el progreso de la canción.
fn spawn_banner_updater(ctx: Context, guild_id: GuildId, queues: Queues, session: u64) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(BANNER_REFRESH).await;

            let (channel_id, message_id, embed) = {
                let queues_guard = queues.lock().await;
                let Some(state) = queues_guard.get(&guild_id) else {
                    break;
                };
                if state.session != session {
                    break;
                }
                let Some((channel_id, message_id)) = state.banner else {
                    continue;
                };

                (channel_id, message_id, banner(state))
            };

            let edit = EditMessage::new()
                .content("🎵 Cola de reproducción")
                .embed(embed);
            if let Err(err) = channel_id.edit_message(&ctx.http, message_id, edit).await {
                eprintln!("[music] no se pudo actualizar el banner: {err}");
                break;
            }
        }
    });
}

/// Reproduce la primera canción y encola el resto (YouTube o Jellyfin).
/// `respect_limit` solo aplica a los links de YouTube: los álbumes de Jellyfin no tienen tope.
async fn play_songs(
    ctx: &Context,
    caller: &MusicCaller,
    songs: Vec<Song>,
    label: String,
    respect_limit: bool,
    queues: &Queues,
) -> MusicResponse {
    let Some(guild_id) = caller.guild_id else {
        return MusicResponse::text("Los comandos de música solo funcionan dentro de un servidor");
    };

    if songs.is_empty() {
        return MusicResponse::text("No hay canciones para reproducir");
    }

    // Ya hay algo sonando en este servidor: las canciones se suman a la cola.
    {
        let mut queues_guard = queues.lock().await;

        if let Some(state) = queues_guard.get_mut(&guild_id) {
            if state.current.is_some() {
                // Un comando nuevo descarta el aviso del fallo anterior.
                state.failure = None;

                let space = if respect_limit {
                    MAX_QUEUE_LEN.saturating_sub(state.youtube_pending())
                } else {
                    usize::MAX
                };
                if space == 0 {
                    println!("[music] cola llena ({MAX_QUEUE_LEN} canciones de YouTube): {label}");
                    return MusicResponse::banner(FULL_QUEUE_TEXT, state);
                }

                let total = songs.len();
                let first_position = state.pending.len() + 1;
                for song in songs.into_iter().take(space) {
                    state.pending.push_back(song);
                }
                let added = state.pending.len() + 1 - first_position;

                println!(
                    "[music] encoladas {added} canciones (posición {first_position}): {label}"
                );

                let mut content = if added == 1 {
                    format!("Agregado a la cola en la posición {first_position}: {label}")
                } else {
                    format!(
                        "Agregadas {added} canciones a la cola (posiciones {first_position}-{}): {label}",
                        state.pending.len()
                    )
                };
                if added < total {
                    content.push_str(&leftover_note(total - added));
                }
                return MusicResponse::banner(content, state);
            }
        }
    }

    let Some(voice_channel) = voice_channel_for(ctx, caller, guild_id).await else {
        return MusicResponse::text(
            "No veo que estés en un canal de voz. Entrá a uno y volvé a mandar el comando.",
        );
    };

    let total = songs.len();
    let mut songs = songs.into_iter();
    let Some(current) = songs.next() else {
        return MusicResponse::text("No hay canciones para reproducir");
    };
    let pending: VecDeque<Song> = if respect_limit {
        songs.take(MAX_QUEUE_LEN).collect()
    } else {
        songs.collect()
    };
    let played = 1 + pending.len();

    queues.lock().await.insert(
        guild_id,
        GuildQueue {
            current: Some(current),
            pending,
            ..Default::default()
        },
    );

    if let Err(message) = start_current(ctx, guild_id, voice_channel, queues).await {
        queues.lock().await.remove(&guild_id);
        return MusicResponse::text(message);
    }

    let queues_guard = queues.lock().await;
    let Some(state) = queues_guard.get(&guild_id) else {
        return MusicResponse::text("No se pudo guardar el estado de la cola");
    };

    let mut content = format!("Reproduciendo: {label}");
    if played < total {
        content.push_str(&leftover_note(total - played));
    }
    MusicResponse::banner(content, state)
}

fn leftover_note(left: usize) -> String {
    format!("\nLa cola admite {MAX_QUEUE_LEN} canciones de YouTube: {left} quedaron afuera.")
}

async fn list_albums(client: &jellyfin::Client) -> MusicResponse {
    let albums = match client.albums().await {
        Ok(albums) => albums,
        Err(err) => {
            eprintln!("[jmusic] no se pudieron listar los álbumes: {err}");
            return MusicResponse::text(format!("No pude pedirle los álbumes a Jellyfin: {err}"));
        }
    };

    if albums.is_empty() {
        return MusicResponse::text("Jellyfin no tiene álbumes cargados");
    }

    println!("[jmusic] list: {} álbumes", albums.len());

    let mut content = format!("**Álbumes en Jellyfin ({})**\n", albums.len());
    for (index, album) in albums.iter().enumerate() {
        content.push_str(&format!("`{}`. {}\n", index + 1, album.label()));
    }
    content.push_str("\nUsá `jmusic <id>` o `jmusic <nombre>` para reproducir uno.");

    MusicResponse::text(content)
}

async fn play_album(
    ctx: &Context,
    caller: &MusicCaller,
    query: &str,
    client: &jellyfin::Client,
    queues: &Queues,
) -> MusicResponse {
    println!("[jmusic] buscando álbum: {query}");

    let albums = match client.find_albums(query).await {
        Ok(albums) => albums,
        Err(err) => {
            eprintln!("[jmusic] no se pudo buscar el álbum «{query}»: {err}");
            return MusicResponse::text(format!("No pude buscar el álbum en Jellyfin: {err}"));
        }
    };

    let album = match albums.as_slice() {
        [] => {
            println!("[jmusic] sin resultados para «{query}»");
            return MusicResponse::text(format!(
                "No encontré ningún álbum que coincida con «{query}». Probá con `jmusic list`."
            ));
        }
        [(_, album)] => album.clone(),
        varios => {
            println!("[jmusic] {} álbumes coinciden con «{query}»", varios.len());
            let mut content = format!(
                "Hay {} álbumes que coinciden con «{query}»:\n",
                varios.len()
            );
            for (index, album) in varios.iter().take(15) {
                content.push_str(&format!("`{index}`. {}\n", album.label()));
            }
            content.push_str("\nEscribí `jmusic <id>` o un nombre más exacto.");
            return MusicResponse::text(content);
        }
    };

    let tracks = match client.tracks(&album.id).await {
        Ok(tracks) => tracks,
        Err(err) => {
            eprintln!(
                "[jmusic] no se pudieron leer las canciones de «{}»: {err}",
                album.name
            );
            return MusicResponse::text(format!("No pude leer las canciones de ese álbum: {err}"));
        }
    };

    if tracks.is_empty() {
        return MusicResponse::text(format!("El álbum «{}» no tiene canciones", album.name));
    }

    let songs: Vec<Song> = tracks
        .into_iter()
        .map(|track| {
            let (url, headers) = client.stream_request(&track.id);
            Song::jellyfin(url, headers, track.title, track.duration)
        })
        .collect();

    println!(
        "[jmusic] álbum «{}»: {} canciones",
        album.label(),
        songs.len()
    );

    let label = match &album.artist {
        Some(artist) => format!("{} — {artist}", album.name),
        None => album.name.clone(),
    };

    play_songs(ctx, caller, songs, label, false, queues).await
}

async fn pause(caller: &MusicCaller, queues: &Queues) -> MusicResponse {
    let Some(guild_id) = caller.guild_id else {
        return MusicResponse::text("Los comandos de música solo funcionan dentro de un servidor");
    };

    let (handle, paused) = {
        let mut queues_guard = queues.lock().await;
        let Some(state) = queues_guard.get_mut(&guild_id) else {
            eprintln!("[yt] pause: no hay nada sonando en el servidor {guild_id}");
            return MusicResponse::text("No hay nada sonando");
        };

        let Some(handle) = state.current_handle.clone() else {
            eprintln!("[yt] pause: no hay pista actual en el servidor {guild_id}");
            return MusicResponse::text("No hay nada sonando");
        };

        state.paused = !state.paused;
        if state.paused {
            state.paused_at = Some(Instant::now());
        } else if let Some(paused_at) = state.paused_at.take() {
            state.paused_total += paused_at.elapsed();
        }

        (handle, state.paused)
    };

    println!("[yt] pause: pausado={paused}");

    let result = if paused {
        handle.pause()
    } else {
        handle.play()
    };

    match result {
        Ok(_) => MusicResponse::text(if paused {
            "⏸ Canción pausada"
        } else {
            "▶️ Canción reanudada"
        }),
        Err(err) => {
            eprintln!("[yt] pause: no se pudo cambiar el estado de la canción: {err}");
            MusicResponse::text("No pude pausar o reanudar la canción")
        }
    }
}

async fn show_queue(caller: &MusicCaller, queues: &Queues) -> MusicResponse {
    let Some(guild_id) = caller.guild_id else {
        return MusicResponse::text("Los comandos de música solo funcionan dentro de un servidor");
    };

    let queues_guard = queues.lock().await;
    let Some(state) = queues_guard.get(&guild_id) else {
        eprintln!("[yt] queue: no hay cola en el servidor {guild_id}");
        return MusicResponse::text("No hay nada en la cola");
    };

    MusicResponse::banner("🎵 Cola actual", state)
}

async fn next(caller: &MusicCaller, queues: &Queues) -> MusicResponse {
    let Some(guild_id) = caller.guild_id else {
        return MusicResponse::text("Los comandos de música solo funcionan dentro de un servidor");
    };

    let (handle, remaining) = {
        let queues_guard = queues.lock().await;
        let Some(state) = queues_guard.get(&guild_id) else {
            eprintln!("[yt] next: no hay nada sonando en el servidor {guild_id}");
            return MusicResponse::text("No hay nada sonando");
        };

        let Some(handle) = state.current_handle.clone() else {
            eprintln!("[yt] next: no hay pista actual en el servidor {guild_id}");
            return MusicResponse::text("No hay nada sonando");
        };

        (handle, state.pending.len())
    };

    // Al detener la pista se dispara el evento de fin, que pasa a la siguiente o desconecta.
    if let Err(err) = handle.stop() {
        eprintln!("[yt] next: no se pudo detener la pista actual: {err}");
    }

    if remaining == 0 {
        println!("[yt] next: no quedan canciones en la cola, desconectando");
        MusicResponse::text("No quedan canciones en la cola, me desconecto")
    } else {
        println!("[yt] next: saltando a la siguiente canción ({remaining} en cola)");
        MusicResponse::text("⏭ Pasando a la siguiente canción")
    }
}

async fn delete(caller: &MusicCaller, index: Option<usize>, queues: &Queues) -> MusicResponse {
    let Some(guild_id) = caller.guild_id else {
        return MusicResponse::text("Los comandos de música solo funcionan dentro de un servidor");
    };

    let mut queues_guard = queues.lock().await;
    let Some(state) = queues_guard.get_mut(&guild_id) else {
        eprintln!("[yt] delete: no hay cola en el servidor {guild_id}");
        return MusicResponse::text("No hay nada en la cola");
    };

    match remove_pending(&mut state.pending, index) {
        Some(song) => {
            println!(
                "[yt] delete: eliminada de la cola: {} ({})",
                song.title, song.url
            );
            MusicResponse::banner(
                format!("Eliminado de la cola: {}", song_label(&song)),
                state,
            )
        }
        None => {
            eprintln!("[yt] delete: la cola está vacía");
            MusicResponse::text("La cola está vacía")
        }
    }
}

async fn exit(ctx: &Context, caller: &MusicCaller, queues: &Queues) -> MusicResponse {
    let Some(guild_id) = caller.guild_id else {
        return MusicResponse::text("Los comandos de música solo funcionan dentro de un servidor");
    };

    // Primero se olvida el estado para que el evento de fin no intente seguir con la cola.
    let state = queues.lock().await.remove(&guild_id);

    println!("[yt] exit: saliendo del servidor {guild_id}");

    if let Some(state) = state {
        if let Some(handle) = state.current_handle {
            if let Err(err) = handle.stop() {
                eprintln!("[yt] exit: no se pudo detener la pista: {err}");
            }
        }
    }

    if let Some(manager) = songbird::get(ctx).await {
        if let Err(err) = manager.remove(guild_id).await {
            eprintln!("[yt] exit: no se pudo salir del canal de voz: {err}");
        }
    }

    MusicResponse::text("Listo, corté la música y me desconecté")
}

/// Quita la canción indicada (1 = próxima) o la última si no se indica posición.
fn remove_pending(pending: &mut VecDeque<Song>, index: Option<usize>) -> Option<Song> {
    match index {
        Some(position) if position >= 1 => pending.remove(position - 1),
        _ => pending.pop_back(),
    }
}

/// Banner con lo que suena y lo que sigue en la cola.
fn banner(state: &GuildQueue) -> CreateEmbed {
    CreateEmbed::new()
        .title("🎵 Cola de reproducción")
        .description(banner_description(state))
        .colour(BANNER_COLOR)
        .footer(CreateEmbedFooter::new(format!(
            "{} en cola · {}",
            state.pending.len(),
            if state.paused {
                "pausado"
            } else {
                "reproduciendo"
            }
        )))
}

fn banner_description(state: &GuildQueue) -> String {
    let mut description = String::new();

    match &state.current {
        Some(current) => {
            let prefix = if state.paused { "⏸" } else { "▶️" };
            description.push_str(&format!("**{prefix} Sonando:**\n{}\n", song_label(current)));

            // Progreso y tiempo restante de la canción actual.
            if let Some(total) = current.duration_secs {
                let position = state.position_secs().min(total);
                description.push_str(&format!(
                    "`{}` {} / {} · faltan {}\n",
                    progress_bar(position, total),
                    format_secs(position),
                    format_secs(total),
                    format_secs(total.saturating_sub(position))
                ));
            }
        }
        None => description.push_str("**Nada sonando**\n"),
    }

    if let Some(failure) = &state.failure {
        description.push_str(&format!("\n⚠️ {failure}\n"));
    }

    if state.pending.is_empty() {
        description.push_str("\n_La cola está vacía._");
    } else {
        description.push_str("\n**En cola:**\n");
        for (index, song) in state.pending.iter().take(MAX_SONGS_IN_BANNER).enumerate() {
            description.push_str(&format!("`{}.` {}\n", index + 1, song_label(song)));
        }
        if state.pending.len() > MAX_SONGS_IN_BANNER {
            let restantes = state.pending.len() - MAX_SONGS_IN_BANNER;
            description.push_str(&format!("_...y {restantes} más._"));
        }
    }

    description
}

/// Al terminar una canción pasa a la siguiente; si no quedan, el bot se va.
struct TrackEnd {
    ctx: Context,
    manager: Arc<Songbird>,
    guild_id: GuildId,
    queues: Queues,
}

/// Motivo legible de un fallo de reproducción, a partir del error crudo de songbird.
fn failure_reason(err: &PlayError) -> String {
    failure_from_text(&err.to_string())
}

/// ¿El fallo es el chequeo anti-bot de YouTube? Ese caso se puede reintentar con otro cliente.
fn bot_check_failure(raw: &str) -> bool {
    let raw = raw.to_lowercase();
    raw.contains("sign in to confirm") || raw.contains("confirm you're not a bot")
}

fn failure_from_text(raw: &str) -> String {
    if raw.contains("could not find executable 'yt-dlp'") {
        return "falta `yt-dlp` en el `PATH` del bot (ver los requisitos del README)".to_owned();
    }

    // "runtime error while playing track: input creation [failed to create audio: <motivo>]"
    let detail = raw
        .split_once("failed to create audio: ")
        .map(|(_, detail)| detail.trim_end_matches(']').trim())
        .unwrap_or(raw);

    // yt-dlp mezcla warnings con el error real: sirve la última línea `ERROR:`.
    let message = detail
        .lines()
        .rev()
        .find_map(|line| line.split_once("ERROR: ").map(|(_, rest)| rest.trim()))
        .unwrap_or(detail);

    match hint_for(message) {
        Some(hint) => hint.to_owned(),
        None => shorten(message, 200),
    }
}

/// Traduce los fallos típicos de YouTube a un motivo accionable.
fn hint_for(message: &str) -> Option<&'static str> {
    let message = message.to_lowercase();

    if message.contains("sign in to confirm") {
        Some(
            "YouTube pidió confirmar que no soy un bot: configurá `YTDLP_COOKIES` en el `.env` (ver el README)",
        )
    } else if message.contains("http error 429") {
        Some("YouTube limitó las peticiones (HTTP 429): esperá unos minutos y reintentá")
    } else {
        None
    }
}

/// Recorta el texto para que el motivo quepa en el mensaje de Discord.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }

    let mut short: String = text.chars().take(max - 1).collect();
    short.push('…');
    short
}

impl TrackEnd {
    /// Refresca el banner con el motivo del fallo y lo manda como mensaje al canal.
    async fn notify_failure(
        &self,
        channel_id: ChannelId,
        message_id: MessageId,
        embed: CreateEmbed,
        message: String,
    ) {
        let edit = EditMessage::new()
            .content("🎵 Cola de reproducción")
            .embed(embed);
        if let Err(err) = channel_id
            .edit_message(&self.ctx.http, message_id, edit)
            .await
        {
            eprintln!("[music] no se pudo actualizar el banner con el fallo: {err}");
        }

        if let Err(err) = channel_id.say(&self.ctx.http, message).await {
            eprintln!("[music] no se pudo avisar del fallo en el canal {channel_id}: {err}");
        }
    }
}

#[async_trait]
impl VoiceEventHandler for TrackEnd {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        // Los fallos de audio (por ejemplo, yt-dlp no disponible) solo se ven acá.
        let error = match ctx {
            EventContext::Track(tracks) => {
                tracks.iter().find_map(|(state, _)| match &state.playing {
                    PlayMode::Errored(err) => {
                        eprintln!("[yt] la pista falló al reproducirse: {err:?}");
                        Some(err)
                    }
                    _ => None,
                })
            }
            _ => None,
        };

        let mut queues_guard = self.queues.lock().await;
        let state = queues_guard.get_mut(&self.guild_id)?;

        // El chequeo anti-bot de YouTube se reintenta una vez con otro cliente antes de avisar.
        let retry = error.is_some_and(|err| bot_check_failure(&err.to_string()))
            && state
                .current
                .as_ref()
                .is_some_and(|song| song.ytdlp_client.is_none());

        // El motivo queda en el banner y además se avisa en el mismo canal del banner.
        let notice = if retry {
            None
        } else {
            error.and_then(|err| {
                let reason = failure_reason(err);
                let label = state
                    .current
                    .as_ref()
                    .map(song_label)
                    .unwrap_or_else(|| "la canción actual".to_owned());
                let failure = format!("No pude reproducir {label}: {reason}");
                state.failure = Some(failure.clone());
                state.banner.map(|(channel_id, message_id)| {
                    (channel_id, message_id, banner(state), failure)
                })
            })
        };

        let next = if retry {
            state.current.take().map(|mut song| {
                song.ytdlp_client = Some(RETRY_CLIENT.to_owned());
                song
            })
        } else {
            state.pending.pop_front()
        };

        let Some(next) = next else {
            queues_guard.remove(&self.guild_id);
            drop(queues_guard);
            println!("[music] cola vacía, saliendo del canal de voz");
            if let Some((channel_id, message_id, embed, failure)) = notice {
                self.notify_failure(channel_id, message_id, embed, failure)
                    .await;
            }
            if let Err(err) = self.manager.remove(self.guild_id).await {
                eprintln!("[yt] no se pudo salir del canal de voz: {err}");
            }
            return None;
        };

        if next.ytdlp_client.is_some() {
            println!(
                "[yt] YouTube pidió verificación; reintento {} con el cliente {RETRY_CLIENT}",
                next.url
            );
        } else {
            println!(
                "[music] pista terminada, sigue: {} ({})",
                next.title, next.url
            );
        }
        state.current = Some(next.clone());
        state.reset_timing();

        let Some(call) = self.manager.get(self.guild_id) else {
            queues_guard.remove(&self.guild_id);
            return None;
        };
        drop(queues_guard);

        let handle = {
            let mut call = call.lock().await;
            call.play_input(input_for(&next))
        };

        if let Some(state) = self.queues.lock().await.get_mut(&self.guild_id) {
            state.current_handle = Some(handle);
        }

        if let Some((channel_id, message_id, embed, failure)) = notice {
            self.notify_failure(channel_id, message_id, embed, failure)
                .await;
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpreta_los_subcomandos() {
        assert_eq!(
            parse_command("yt https://youtu.be/dQw4w9WgXcQ"),
            YtCommand::Add("https://youtu.be/dQw4w9WgXcQ")
        );
        assert_eq!(parse_command("yt pause"), YtCommand::Pause);
        assert_eq!(parse_command("yt next"), YtCommand::Next);
        assert_eq!(parse_command("yt queue"), YtCommand::ShowQueue);
        assert_eq!(parse_command("yt help"), YtCommand::Help);
        assert_eq!(parse_command("yt exit"), YtCommand::Exit);
        assert_eq!(parse_command("yt delete"), YtCommand::Delete(None));
        assert_eq!(parse_command("yt delete 3"), YtCommand::Delete(Some(3)));
        assert_eq!(parse_command("yt delete tres"), YtCommand::Delete(None));
        assert_eq!(parse_command("yt"), YtCommand::Usage);
        assert_eq!(parse_command("yt no-es-un-link"), YtCommand::Usage);
    }
    fn song(title: &str) -> Song {
        Song::youtube(
            &format!("https://example.com/{title}"),
            Some(title.to_owned()),
            None,
        )
    }

    #[test]
    fn interpreta_los_comandos_de_jmusic() {
        assert_eq!(parse_jmusic_command("jmusic list"), JmusicCommand::List);
        assert_eq!(parse_jmusic_command("jmusic LIST"), JmusicCommand::List);
        assert_eq!(parse_jmusic_command("jmusic pause"), JmusicCommand::Pause);
        assert_eq!(parse_jmusic_command("jmusic next"), JmusicCommand::Next);
        assert_eq!(parse_jmusic_command("jmusic exit"), JmusicCommand::Exit);
        assert_eq!(parse_jmusic_command("jmusic"), JmusicCommand::Usage);
        assert_eq!(
            parse_jmusic_command("jmusic Discovery"),
            JmusicCommand::Play("Discovery")
        );
        assert_eq!(
            parse_jmusic_command("jmusic Daft Punk - Discovery"),
            JmusicCommand::Play("Daft Punk - Discovery")
        );
    }

    #[test]
    fn interpreta_la_duracion_y_dibuja_el_progreso() {
        assert_eq!(parse_duration("3:33"), Some(213));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("0:07"), Some(7));
        assert_eq!(parse_duration("NA"), None);
        assert_eq!(parse_duration(""), None);

        assert_eq!(format_secs(213), "3:33");
        assert_eq!(format_secs(3723), "1:02:03");

        let bar = progress_bar(0, 200);
        assert_eq!(bar.chars().count(), PROGRESS_BAR_WIDTH);
        assert!(bar.starts_with('🔘'));
        assert!(progress_bar(200, 200).chars().all(|cell| cell == '▬'));
    }

    #[test]
    fn el_banner_muestra_progreso_y_tiempo_restante() {
        let mut state = GuildQueue::default();
        let mut current = song("Tema");
        current.duration = Some("3:20".to_owned());
        current.duration_secs = Some(200);
        state.current = Some(current);
        state.started_at = Some(Instant::now() - Duration::from_secs(30));

        let description = banner_description(&state);
        assert!(description.contains("0:30 / 3:20"), "{description}");
        assert!(description.contains("faltan 2:50"), "{description}");
        assert!(description.contains('🔘'), "{description}");
    }

    #[test]
    fn el_banner_muestra_el_motivo_del_fallo() {
        let state = GuildQueue {
            current: Some(song("Tema")),
            failure: Some("No pude reproducir Tema: falta `yt-dlp`".to_owned()),
            ..Default::default()
        };

        let description = banner_description(&state);
        assert!(
            description.contains("⚠️ No pude reproducir Tema: falta `yt-dlp`"),
            "{description}"
        );
    }

    #[test]
    fn traduce_el_fallo_de_ytdlp_a_un_motivo_legible() {
        assert_eq!(
            failure_from_text(
                "runtime error while playing track: input creation [failed to create audio: could not find executable 'yt-dlp' on path]"
            ),
            "falta `yt-dlp` en el `PATH` del bot (ver los requisitos del README)"
        );
        assert_eq!(
            failure_from_text(
                "runtime error while playing track: input creation [failed to create audio: yt-dlp failed with non-zero status code: ERROR: Video unavailable]"
            ),
            "Video unavailable"
        );
        assert_eq!(
            failure_from_text("runtime error while playing track: parsing formats/codecs [rot]"),
            "runtime error while playing track: parsing formats/codecs [rot]"
        );
    }

    #[test]
    fn muestra_el_error_real_y_no_los_warnings_de_ytdlp() {
        // Tal cual lo escupe yt-dlp cuando YouTube frena la extracción.
        let raw = "runtime error while playing track: input creation [failed to create audio: yt-dlp failed with non-zero status code: WARNING: [youtube] jidurZRdbSg: Unable to download webpage: HTTP Error 429: Too Many Requests\nWARNING: [youtube] No supported JavaScript runtime could be found. Only deno is enabled by default\nERROR: [youtube] jidurZRdbSg: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the authentication.]";

        let motivo = failure_from_text(raw);
        assert!(
            motivo.starts_with("YouTube pidió confirmar que no soy un bot"),
            "{motivo}"
        );
        assert!(!motivo.contains("WARNING"), "{motivo}");

        let limitado = failure_from_text(
            "runtime error while playing track: input creation [failed to create audio: yt-dlp failed with non-zero status code: ERROR: Unable to download webpage: HTTP Error 429: Too Many Requests]",
        );
        assert!(limitado.contains("HTTP 429"), "{limitado}");
    }

    #[test]
    fn arma_los_argumentos_de_cookies_del_env() {
        let expected = |parts: [&str; 2]| vec![parts[0].to_owned(), parts[1].to_owned()];

        assert_eq!(
            cookie_args_for(Some("/ruta/cookies.txt"), None),
            expected(["--cookies", "/ruta/cookies.txt"])
        );
        assert_eq!(
            cookie_args_for(None, Some("firefox")),
            expected(["--cookies-from-browser", "firefox"])
        );
        // El archivo gana cuando están las dos variables.
        assert_eq!(
            cookie_args_for(Some("/ruta/cookies.txt"), Some("firefox")),
            expected(["--cookies", "/ruta/cookies.txt"])
        );
        assert!(cookie_args_for(Some("  "), None).is_empty());
        assert!(cookie_args_for(None, None).is_empty());
    }

    #[test]
    fn el_reintento_fuerza_otro_cliente_de_youtube() {
        let mut tema = song("Tema");
        assert!(client_args(&tema).is_empty());

        tema.ytdlp_client = Some(RETRY_CLIENT.to_owned());
        assert_eq!(
            client_args(&tema),
            vec![
                "--extractor-args".to_owned(),
                format!("youtube:player_client={RETRY_CLIENT}")
            ]
        );
    }

    #[test]
    fn reconoce_el_fallo_anti_bot_de_youtube() {
        assert!(bot_check_failure(
            "yt-dlp failed with non-zero status code: ERROR: [youtube] jidurZRdbSg: Sign in to confirm you’re not a bot."
        ));
        assert!(!bot_check_failure(
            "yt-dlp failed with non-zero status code: ERROR: Video unavailable"
        ));
    }

    #[test]
    fn recorta_los_motivos_muy_largos() {
        let largo = shorten(&"a".repeat(500), 200);
        assert_eq!(largo.chars().count(), 200);
        assert!(largo.ends_with('…'));
        assert_eq!(shorten("corto", 200), "corto");
    }

    #[test]
    fn solo_los_links_de_youtube_cuentan_para_el_tope() {
        let mut state = GuildQueue::default();
        state
            .pending
            .extend(["a", "b"].iter().map(|title| song(title)));
        state.pending.push_back(Song::jellyfin(
            "http://jellyfin/Audio/1/stream.mp3".to_owned(),
            Vec::new(),
            "tema".to_owned(),
            None,
        ));

        assert_eq!(state.youtube_pending(), 2);
        assert_eq!(state.pending.len(), 3);
    }

    #[test]
    fn muestra_la_duracion_en_el_banner() {
        let mut con_duracion = song("Cancion");
        con_duracion.duration = Some("3:33".to_owned());

        assert_eq!(song_label(&con_duracion), "Cancion · 3:33");
        assert_eq!(song_label(&song("Otra")), "Otra");
    }

    fn titles(pending: &VecDeque<Song>) -> Vec<String> {
        pending.iter().map(|song| song.title.clone()).collect()
    }

    #[test]
    fn quita_la_cancion_indicada_o_la_ultima() {
        let mut pending: VecDeque<Song> = ["a", "b", "c"].iter().map(|t| song(t)).collect();

        assert_eq!(
            remove_pending(&mut pending, Some(1)).map(|song| song.title),
            Some("a".to_string())
        );
        assert_eq!(titles(&pending), vec!["b", "c"]);

        assert_eq!(
            remove_pending(&mut pending, None).map(|song| song.title),
            Some("c".to_string())
        );
        assert_eq!(remove_pending(&mut pending, Some(9)), None);
        assert_eq!(
            remove_pending(&mut pending, None).map(|song| song.title),
            Some("b".to_string())
        );
        assert_eq!(remove_pending(&mut pending, None), None);
    }
}
