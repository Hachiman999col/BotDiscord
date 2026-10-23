use std::path::PathBuf;

use serenity::{
    all::{
        Channel, ChannelId, CommandDataOption, CommandDataOptionValue, CommandInteraction,
        CommandOptionType, Context, CreateAttachment, CreateCommand, CreateCommandOption,
        CreateInteractionResponse, CreateInteractionResponseFollowup,
        CreateInteractionResponseMessage, CreateMessage, EventHandler, GatewayIntents, Interaction,
        Message, Ready,
    },
    async_trait, Client,
};
use songbird::serenity::SerenityInit;

mod jellyfin;
mod music;

/// El bot solo puede hablar en el canal de texto con este nombre.
const BOT_CHANNEL_NAME: &str = "bot";
const POKEMON_PHOTOS_DIR: &str = "assets";
const POKEMON_MAX_ID: u32 = 898;
/// Discord corta los mensajes en 2000 caracteres.
const MESSAGE_LIMIT: usize = 1900;

/// Texto del comando `/commands`: la lista completa de lo que sabe hacer el bot.
const COMMANDS_TEXT: &str = "**Comandos disponibles**\n\
`/hi` — el bot te saluda\n\
`/pkimg id:<1-898>` — imagen de un Pokémon\n\
`/yt play url:<link>` — reproduce el link o lo agrega a la cola\n\
`/yt pause` — pausa o reanuda la canción actual\n\
`/yt next` — pasa a la siguiente canción (si no quedan, desconecta)\n\
`/yt queue` — muestra el banner con la cola\n\
`/yt delete [posicion]` — quita una canción de la cola\n\
`/yt exit` — corta la música y desconecta al bot\n\
`/yt help` — ayuda de yt\n\
`/jmusic list` — lista todos los álbumes de Jellyfin con su id\n\
`/jmusic play album:<nombre|id>` — reproduce un álbum de Jellyfin\n\
`/jmusic pause` — pausa o reanuda la canción actual\n\
`/jmusic next` — pasa a la siguiente canción (si no quedan, desconecta)\n\
`/jmusic exit` — corta la música y desconecta al bot\n\
`/commands` — muestra esta lista\n\n\
Los mismos comandos de `yt` y `jmusic` funcionan como mensaje: `yt <url>`, `yt pause`, `jmusic list`, etc. Todo se usa en el canal de texto `#bot`.";

struct Handler {
    queues: music::Queues,
    /// Resultado de la conexión con Jellyfin al arrancar; el bot funciona igual si falla.
    jellyfin: Result<jellyfin::Client, String>,
}

impl Handler {
    fn new(jellyfin: Result<jellyfin::Client, String>) -> Self {
        Self {
            queues: music::new_queues(),
            jellyfin,
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("Logged in as {}", ready.user.name);

        // Se registran por servidor para que aparezcan al instante al escribir "/".
        let commands = slash_commands();
        for guild in &ready.guilds {
            match ctx.http.create_guild_commands(guild.id, &commands).await {
                Ok(registered) => println!(
                    "Comandos de barra registrados en {} ({})",
                    guild.id,
                    registered.len()
                ),
                Err(err) => eprintln!(
                    "No se pudieron registrar los comandos de barra en {}: {err}",
                    guild.id
                ),
            }
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        // Ignora mensajes de cualquier canal que no se llame "bot" (incluye DMs e hilos).
        if !is_bot_channel(&ctx, msg.channel_id).await {
            return;
        }

        if msg.content == "hi" {
            if let Err(err) = msg.reply(&ctx, "Hola putita").await {
                eprintln!("No se pudo responder el saludo: {err}");
            }
            return;
        }

        if msg.content == "yt" || msg.content.starts_with("yt ") {
            let caller = music::MusicCaller {
                guild_id: msg.guild_id,
                user_id: msg.author.id,
            };
            let command = music::parse_command(&msg.content);
            let response = music::run(&ctx, caller, command, &self.queues).await;
            send_music_reply(&ctx, &msg, response, &self.queues).await;
            return;
        }

        if msg.content == "jmusic" || msg.content.starts_with("jmusic ") {
            let caller = music::MusicCaller {
                guild_id: msg.guild_id,
                user_id: msg.author.id,
            };
            let command = music::parse_jmusic_command(&msg.content);
            let response =
                music::run_jmusic(&ctx, caller, command, &self.jellyfin, &self.queues).await;
            send_music_reply(&ctx, &msg, response, &self.queues).await;
            return;
        }

        if msg.content.contains("pkimg") {
            let id = pokemon_id(&msg.content);
            if id > 0 && id <= POKEMON_MAX_ID {
                send_pokemon_image(&ctx, msg.channel_id, id).await;
            } else if let Err(err) = msg.reply(&ctx, "El id del pokemon no fue encontrado").await {
                eprintln!("No se pudo responder el error del pokemon: {err}");
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };

        if !is_bot_channel(&ctx, command.channel_id).await {
            let response = CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Solo respondo en el canal de texto #bot")
                    .ephemeral(true),
            );
            if let Err(err) = command.create_response(&ctx.http, response).await {
                eprintln!("No se pudo responder la interacción: {err}");
            }
            return;
        }

        match command.data.name.as_str() {
            "hi" => send_slash_text(&ctx, &command, "Hola putita").await,
            "pkimg" => slash_pkimg(&ctx, &command).await,
            "yt" => slash_yt(&ctx, &command, &self.queues).await,
            "jmusic" => slash_jmusic(&ctx, &command, &self.jellyfin, &self.queues).await,
            "commands" => send_slash_text(&ctx, &command, COMMANDS_TEXT).await,
            other => eprintln!("Comando de barra desconocido: {other}"),
        }
    }
}

/// Definición de los comandos que aparecen al escribir "/" en Discord.
fn slash_commands() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new("hi").description("El bot te saluda"),
        CreateCommand::new("pkimg")
            .description("Envía la imagen de un Pokémon")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::Integer,
                    "id",
                    "Id del Pokémon (1-898)",
                )
                .required(true)
                .min_int_value(1)
                .max_int_value(POKEMON_MAX_ID as u64),
            ),
        CreateCommand::new("yt")
            .description("Música de YouTube en tu canal de voz")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "play",
                    "Reproduce un link de YouTube o lo agrega a la cola",
                )
                .add_sub_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "url",
                        "Link del video de YouTube",
                    )
                    .required(true),
                ),
            )
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "pause",
                "Pausa o reanuda la canción actual",
            ))
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "next",
                "Pasa a la siguiente canción o desconecta si no hay más",
            ))
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "queue",
                "Muestra la cola de reproducción actual",
            ))
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "delete",
                    "Quita una canción de la cola",
                )
                .add_sub_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "posicion",
                        "Posición en la cola (1 = la próxima)",
                    )
                    .min_int_value(1),
                ),
            )
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "exit",
                "Corta la música y desconecta al bot",
            ))
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "help",
                "Muestra todos los comandos de yt",
            )),
        CreateCommand::new("jmusic")
            .description("Música de Jellyfin en tu canal de voz")
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "list",
                "Muestra todos los álbumes de Jellyfin",
            ))
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "play",
                    "Reproduce un álbum de Jellyfin",
                )
                .add_sub_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "album",
                        "Nombre del álbum (o artista - álbum)",
                    )
                    .required(true),
                ),
            )
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "pause",
                "Pausa o reanuda la canción actual",
            ))
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "next",
                "Pasa a la siguiente canción o desconecta si no hay más",
            ))
            .add_option(CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "exit",
                "Corta la música y desconecta al bot",
            )),
        CreateCommand::new("commands").description("Muestra todos los comandos disponibles"),
    ]
}

async fn is_bot_channel(ctx: &Context, channel_id: ChannelId) -> bool {
    match channel_id.to_channel(ctx).await {
        Ok(Channel::Guild(channel)) => channel.name == BOT_CHANNEL_NAME,
        _ => false,
    }
}

/// El id es la segunda palabra del mensaje: "pkimg 25" -> 25.
fn pokemon_id(content: &str) -> u32 {
    content
        .split_whitespace()
        .nth(1)
        .and_then(|id| id.parse().ok())
        .unwrap_or(0)
}

async fn send_pokemon_image(ctx: &Context, channel_id: ChannelId, id: u32) {
    let path = PathBuf::from(format!("{POKEMON_PHOTOS_DIR}/photo_{id}.jpg"));

    let attachment = match CreateAttachment::path(&path).await {
        Ok(attachment) => attachment,
        Err(err) => {
            eprintln!("No se pudo abrir {}: {err}", path.display());
            return;
        }
    };

    let builder = CreateMessage::new().add_files(vec![attachment]);
    if let Err(err) = channel_id.send_message(&ctx.http, builder).await {
        eprintln!("No se pudo enviar la imagen del pokemon: {err}");
    }
}

async fn send_music_reply(
    ctx: &Context,
    msg: &Message,
    response: music::MusicResponse,
    queues: &music::Queues,
) {
    match response {
        music::MusicResponse::Text(text) => {
            // El listado de álbumes puede superar el límite de Discord: se manda en partes.
            for (index, chunk) in split_message(&text).into_iter().enumerate() {
                let builder = if index == 0 {
                    CreateMessage::new().content(chunk).reference_message(msg)
                } else {
                    CreateMessage::new().content(chunk)
                };

                if let Err(err) = msg.channel_id.send_message(&ctx.http, builder).await {
                    eprintln!("[music] no se pudo responder: {err}");
                    break;
                }
            }
        }
        music::MusicResponse::Banner { content, embed } => {
            let builder = CreateMessage::new()
                .content(content)
                .embed(*embed)
                .reference_message(msg);
            match msg.channel_id.send_message(&ctx.http, builder).await {
                Ok(sent) => {
                    if let Some(guild_id) = msg.guild_id {
                        music::remember_banner(queues, guild_id, sent.channel_id, sent.id).await;
                    }
                }
                Err(err) => eprintln!("[music] no se pudo responder: {err}"),
            }
        }
    }
}

/// Parte el texto en trozos de menos de 2000 caracteres, cortando por líneas.
fn split_message(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        let needed = line.chars().count() + usize::from(!current.is_empty());
        if !current.is_empty() && current.chars().count() + needed > MESSAGE_LIMIT {
            chunks.push(std::mem::take(&mut current));
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);

        // Una línea sola más larga que el límite se corta en pedazos.
        while current.chars().count() > MESSAGE_LIMIT {
            let head: String = current.chars().take(MESSAGE_LIMIT).collect();
            chunks.push(head);
            current = current.chars().skip(MESSAGE_LIMIT).collect();
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

async fn send_slash_text(ctx: &Context, command: &CommandInteraction, text: &str) {
    let response =
        CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content(text));
    if let Err(err) = command.create_response(&ctx.http, response).await {
        eprintln!("No se pudo responder la interacción: {err}");
    }
}

async fn slash_pkimg(ctx: &Context, command: &CommandInteraction) {
    let id = command
        .data
        .options
        .iter()
        .find(|option| option.name == "id")
        .and_then(|option| match &option.value {
            CommandDataOptionValue::Integer(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(0);

    if id < 1 || id > POKEMON_MAX_ID as i64 {
        send_slash_text(ctx, command, "El id del pokemon no fue encontrado").await;
        return;
    }

    let path = PathBuf::from(format!("{POKEMON_PHOTOS_DIR}/photo_{id}.jpg"));

    match CreateAttachment::path(&path).await {
        Ok(attachment) => {
            let response = CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Pokémon {id}"))
                    .add_file(attachment),
            );
            if let Err(err) = command.create_response(&ctx.http, response).await {
                eprintln!("No se pudo enviar la imagen del pokemon: {err}");
            }
        }
        Err(err) => {
            eprintln!("No se pudo abrir {}: {err}", path.display());
            send_slash_text(ctx, command, "No pude abrir la imagen del pokemon").await;
        }
    }
}

async fn slash_yt(ctx: &Context, command: &CommandInteraction, queues: &music::Queues) {
    // Se confirma la interacción antes de conectarse al canal de voz (tarda más de 3 segundos).
    if let Err(err) = command.defer(&ctx.http).await {
        eprintln!("[yt] no se pudo confirmar la interacción: {err}");
        return;
    }

    let caller = music::MusicCaller {
        guild_id: command.guild_id,
        user_id: command.user.id,
    };
    let yt_command = yt_command_from_slash(command);
    let response = music::run(ctx, caller, yt_command, queues).await;
    send_followup(ctx, command, response, queues).await;
}

async fn slash_jmusic(
    ctx: &Context,
    command: &CommandInteraction,
    jellyfin: &Result<jellyfin::Client, String>,
    queues: &music::Queues,
) {
    if let Err(err) = command.defer(&ctx.http).await {
        eprintln!("[jmusic] no se pudo confirmar la interacción: {err}");
        return;
    }

    let caller = music::MusicCaller {
        guild_id: command.guild_id,
        user_id: command.user.id,
    };
    let jmusic_command = jmusic_command_from_slash(command);
    let response = music::run_jmusic(ctx, caller, jmusic_command, jellyfin, queues).await;
    send_followup(ctx, command, response, queues).await;
}

/// Responde la interacción ya confirmada, partiendo el texto si es muy largo.
async fn send_followup(
    ctx: &Context,
    command: &CommandInteraction,
    response: music::MusicResponse,
    queues: &music::Queues,
) {
    let is_banner = matches!(response, music::MusicResponse::Banner { .. });
    let followups: Vec<CreateInteractionResponseFollowup> = match response {
        music::MusicResponse::Text(text) => split_message(&text)
            .into_iter()
            .map(|chunk| CreateInteractionResponseFollowup::new().content(chunk))
            .collect(),
        music::MusicResponse::Banner { content, embed } => {
            vec![CreateInteractionResponseFollowup::new()
                .content(content)
                .embed(*embed)]
        }
    };

    for (index, followup) in followups.into_iter().enumerate() {
        match command.create_followup(&ctx.http, followup).await {
            Ok(sent) => {
                // Solo el último banner enviado se refresca con el progreso.
                if is_banner && index == 0 {
                    if let Some(guild_id) = command.guild_id {
                        music::remember_banner(queues, guild_id, sent.channel_id, sent.id).await;
                    }
                }
            }
            Err(err) => {
                eprintln!("[music] no se pudo responder la interacción: {err}");
                break;
            }
        }
    }
}

fn yt_command_from_slash(command: &CommandInteraction) -> music::YtCommand<'_> {
    let Some(option) = command.data.options.first() else {
        return music::YtCommand::Usage;
    };

    match option.name.as_str() {
        "play" => match sub_option_string(option, "url") {
            Some(url) => music::YtCommand::Add(url),
            None => music::YtCommand::Usage,
        },
        "pause" => music::YtCommand::Pause,
        "next" => music::YtCommand::Next,
        "queue" => music::YtCommand::ShowQueue,
        "delete" => music::YtCommand::Delete(
            sub_option_int(option, "posicion").map(|position| position.max(1) as usize),
        ),
        "exit" => music::YtCommand::Exit,
        "help" => music::YtCommand::Help,
        _ => music::YtCommand::Usage,
    }
}

fn jmusic_command_from_slash(command: &CommandInteraction) -> music::JmusicCommand<'_> {
    let Some(option) = command.data.options.first() else {
        return music::JmusicCommand::Usage;
    };

    match option.name.as_str() {
        "list" => music::JmusicCommand::List,
        "play" => sub_option_string(option, "album")
            .map(music::JmusicCommand::Play)
            .unwrap_or(music::JmusicCommand::Usage),
        "pause" => music::JmusicCommand::Pause,
        "next" => music::JmusicCommand::Next,
        "exit" => music::JmusicCommand::Exit,
        _ => music::JmusicCommand::Usage,
    }
}

fn sub_option_string<'a>(option: &'a CommandDataOption, name: &str) -> Option<&'a str> {
    let CommandDataOptionValue::SubCommand(sub_options) = &option.value else {
        return None;
    };

    sub_options
        .iter()
        .find(|sub_option| sub_option.name == name)
        .and_then(|sub_option| match &sub_option.value {
            CommandDataOptionValue::String(value) => Some(value.as_str()),
            _ => None,
        })
}

fn sub_option_int(option: &CommandDataOption, name: &str) -> Option<i64> {
    let CommandDataOptionValue::SubCommand(sub_options) = &option.value else {
        return None;
    };

    sub_options
        .iter()
        .find(|sub_option| sub_option.name == name)
        .and_then(|sub_option| match &sub_option.value {
            CommandDataOptionValue::Integer(value) => Some(*value),
            _ => None,
        })
}

#[tokio::main]
async fn main() {
    if let Err(err) = dotenvy::dotenv() {
        eprintln!("No se encontró el archivo .env: {err}");
    }

    let token = std::env::var("DISCORD_TOKEN").expect(
        "Falta DISCORD_TOKEN. Crea un .env en la raíz del proyecto con DISCORD_TOKEN=tu_token",
    );

    // Muestra el token que se va a usar antes de conectarse.
    println!("Token: {token}");

    // Si Jellyfin falla, el bot arranca igual y los comandos jmusic avisan del problema.
    let jellyfin = jellyfin::Client::connect_from_env().await;
    match &jellyfin {
        Ok(client) => println!(
            "Jellyfin conectado como {} ({})",
            client.user_name(),
            client.base()
        ),
        Err(err) => eprintln!("[jmusic] Jellyfin no disponible: {err}"),
    }

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler::new(jellyfin))
        .register_songbird()
        .await
        .expect("Error creando el cliente de Discord");

    if let Err(err) = client.start().await {
        eprintln!("Error ejecutando el bot: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_el_id_del_pokemon() {
        assert_eq!(pokemon_id("pkimg 25"), 25);
        assert_eq!(pokemon_id("pkimg"), 0);
        assert_eq!(pokemon_id("pkimg abc"), 0);
        assert_eq!(pokemon_id("otra cosa"), 0);
    }

    #[test]
    fn define_los_comandos_de_barra() {
        let commands = serenity::json::to_value(slash_commands()).expect("serializar comandos");
        let names: Vec<&str> = commands
            .as_array()
            .expect("lista de comandos")
            .iter()
            .map(|command| command["name"].as_str().expect("nombre"))
            .collect();

        assert_eq!(names, vec!["hi", "pkimg", "yt", "jmusic", "commands"]);

        let yt = commands
            .as_array()
            .expect("lista de comandos")
            .iter()
            .find(|command| command["name"] == "yt")
            .expect("comando yt");

        let subcommands: Vec<&str> = yt["options"]
            .as_array()
            .expect("subcomandos")
            .iter()
            .map(|option| option["name"].as_str().expect("nombre"))
            .collect();
        assert_eq!(
            subcommands,
            vec!["play", "pause", "next", "queue", "delete", "exit", "help"]
        );

        let url_option = &yt["options"][0]["options"][0];
        assert_eq!(url_option["name"], "url");
        assert_eq!(url_option["required"], true);

        let jmusic = commands
            .as_array()
            .expect("lista de comandos")
            .iter()
            .find(|command| command["name"] == "jmusic")
            .expect("comando jmusic");

        let jmusic_subcommands: Vec<&str> = jmusic["options"]
            .as_array()
            .expect("subcomandos")
            .iter()
            .map(|option| option["name"].as_str().expect("nombre"))
            .collect();
        assert_eq!(
            jmusic_subcommands,
            vec!["list", "play", "pause", "next", "exit"]
        );

        let album_option = &jmusic["options"][1]["options"][0];
        assert_eq!(album_option["name"], "album");
        assert_eq!(album_option["required"], true);
    }

    #[test]
    fn el_ayuda_del_comando_commands_lista_todo() {
        for command in [
            "hi",
            "pkimg",
            "/yt play",
            "/yt pause",
            "/yt next",
            "/yt queue",
            "/yt delete",
            "/yt exit",
            "/jmusic list",
            "/jmusic play",
            "/jmusic pause",
            "/jmusic next",
            "/jmusic exit",
            "/commands",
        ] {
            assert!(
                COMMANDS_TEXT.contains(command),
                "falta {command} en el texto de /commands"
            );
        }

        assert!(COMMANDS_TEXT.chars().count() <= MESSAGE_LIMIT);
    }

    #[test]
    fn parte_los_mensajes_largos() {
        assert_eq!(split_message("hola"), vec!["hola"]);
        assert_eq!(split_message(""), vec![""]);

        let muchas_lineas: String = (0..300).map(|line| format!("línea {line}\n")).collect();
        let chunks = split_message(&muchas_lineas);

        assert!(chunks.len() > 1);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.chars().count() <= MESSAGE_LIMIT));
        assert_eq!(
            chunks.concat().replace('\n', ""),
            muchas_lineas.replace('\n', "")
        );
    }
}
