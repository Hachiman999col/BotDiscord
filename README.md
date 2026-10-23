# BotDiscord

Bot de Discord escrito en Rust con [serenity](https://github.com/serenity-rs/serenity) y [songbird](https://github.com/serenity-rs/songbird) para el audio.

El bot **solo responde en un canal de texto llamado `bot`**. En cualquier otro canal (o por DM) ignora los mensajes.

## Comandos

Los comandos funcionan de dos formas: escribiendo el mensaje (`yt <url>`) o como **comandos de barra** (`/yt play`), que aparecen al escribir `/` en Discord.

| Mensaje | Slash | Descripción |
| --- | --- | --- |
| `hi` | `/hi` | El bot te saluda. |
| `pkimg <id>` | `/pkimg id:<id>` | Envía la imagen local del Pokémon con ese id (1–898). Ejemplo: `pkimg 25`. |
| `yt <url>` | `/yt play url:<link>` | Entra a tu canal de voz y reproduce el audio de ese link de YouTube. Si ya hay una canción sonando, el link se agrega a la cola. |
| `yt pause` | `/yt pause` | Pausa o reanuda la canción actual. |
| `yt next` | `/yt next` | Salta a la siguiente canción de la cola. Si no queda ninguna, desconecta al bot del canal de voz. |
| `yt queue` | `/yt queue` | Muestra el banner con la canción actual y la cola. |
| `yt delete [posición]` | `/yt delete [posicion]` | Quita de la cola la canción en esa posición (1 = la próxima). Sin posición, quita la última agregada. |
| `yt exit` | `/yt exit` | Corta la música y desconecta al bot del canal de voz. |
| `yt help` | `/yt help` | Muestra la lista de comandos de `yt`. Mandar `yt` sin argumentos muestra lo mismo. |
| `jmusic list` | `/jmusic list` | Muestra todos los álbumes de Jellyfin con un id, nombre, artista, año y cantidad de canciones. |
| `jmusic <id\|álbum>` | `/jmusic play album:<nombre\|id>` | Busca el álbum en Jellyfin, lista sus canciones y las reproduce. Acepta el id de `jmusic list`, el nombre del álbum, el artista o `artista - álbum`; si hay varias coincidencias, muestra la lista con sus ids para elegir. |
| `jmusic pause` | `/jmusic pause` | Pausa o reanuda la canción actual (misma cola que `yt`). |
| `jmusic next` | `/jmusic next` | Salta a la siguiente canción de la cola; si no queda ninguna, desconecta al bot. |
| `jmusic exit` | `/jmusic exit` | Corta la música y desconecta al bot del canal de voz. |
| — | `/commands` | Muestra la lista completa de comandos disponibles. |

Cada vez que se agrega o quita algo de la cola, el bot responde con un **banner** (embed) que muestra la canción actual y las próximas de la lista, con el **título y la duración** (en `yt` se piden a `yt-dlp` al agregar el link; en `jmusic` vienen de Jellyfin).

`yt` y `jmusic` comparten la misma cola por servidor: podés mezclar links de YouTube con álbumes de Jellyfin en la misma reproducción.

El banner se **refresca cada 10 segundos** mientras suena algo: muestra una barra de progreso, el tiempo transcurrido, la duración total y **cuánto falta** para que termine la canción.

La cola admite hasta **10 canciones de YouTube**. Si está llena, el bot avisa y no agrega el link nuevo (podés liberar lugar con `yt delete`). Los **álbumes de Jellyfin no cuentan** para ese tope: un álbum de 100 pistas se encola completo.

## Requisitos

- Rust (probado con 1.97). Instalación: https://rustup.rs
- **yt-dlp** en el `PATH` (lo usa songbird para resolver el audio de YouTube). Instalación del binario standalone:
  ```bash
  mkdir -p ~/.local/bin
  curl -L -o ~/.local/bin/yt-dlp https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp
  chmod +x ~/.local/bin/yt-dlp
  ```
- **libopus**, **cmake** y **pkg-config** para compilar songbird (codifica el audio a Opus). En Fedora: `sudo dnf install opus-devel cmake pkgconf-pkg-config`; en Arch: `sudo pacman -S opus cmake pkgconf`.
- Un bot creado en el [Discord Developer Portal](https://discord.com/developers/applications).
- El intent privilegiado **Message Content** activado en Developer Portal → Bot → Privileged Gateway Intents. Sin esto el contenido de los mensajes llega vacío y el bot no reconoce ningún comando.
- El bot necesita el permiso de **Conectar** en los canales de voz para los comandos de música.
- Un servidor de **Jellyfin** accesible por HTTP/HTTPS con un usuario y contraseña. El audio se pide transcodificado a **mp3** (`/Audio/<id>/stream.mp3`), así que el servidor necesita `ffmpeg` (viene incluido en la imagen oficial de Jellyfin).

## Comandos de barra

Al conectarse, el bot registra `/hi`, `/pkimg`, `/yt` (subcomandos `play`, `pause`, `next`, `queue`, `delete`, `exit` y `help`), `/jmusic` (subcomandos `list`, `play`, `pause`, `next` y `exit`) y `/commands` en cada servidor donde está, así que aparecen al escribir `/` sin esperar la propagación global de Discord.

`/commands` responde con la lista completa de comandos y para qué sirve cada uno.

Si al escribir `/` no aparece el bot, revisá que lo hayas invitado con el scope **applications.commands** además de **bot**. Podés re-invitar desde Developer Portal → OAuth2 → URL Generator marcando `bot` y `applications.commands`.

Los slash commands respetan la misma regla que los mensajes: solo responden en el canal de texto `#bot` (en otro canal el bot contesta de forma efímera que debe usarse `#bot`).

## Configuración

Creá un archivo `.env` en la raíz del proyecto:

```
DISCORD_TOKEN=tu_token_del_bot
URL_JELLYFIN=https://jellyfin.ejemplo.com
JELLYFIN_USER=usuario
JELLYFIN_PASSWORD=contraseña
```

El token se obtiene en Developer Portal → tu aplicación → Bot → Reset Token. El `.env` está en `.gitignore`, así que no se sube al repositorio.

`URL_JELLYFIN` es la base del servidor (sin `/web`). El bot inicia sesión con `POST /Users/AuthenticateByName` al arrancar e imprime `Jellyfin conectado como <usuario> (<url>)`; si falla, arranca igual y los comandos `jmusic` responden con el error (queda en consola con el prefijo `[jmusic]`).

> Jellyfin 12 deshabilitó las cabeceras viejas `X-Emby-*`: el bot usa `Authorization: MediaBrowser ...` tanto para el login como para pedir los álbumes y el audio.

## Ejecución

```bash
cargo run
```

La primera vez compila todas las dependencias (tarda alrededor de un minuto); después arranca al instante. Cuando se conecta imprime `Logged in as <nombre del bot>`.

Para producción:

```bash
cargo build --release
./target/release/botdiscord
```

Para desarrollo con recarga automática, instalá `cargo-watch` si no lo tenés y ejecutá el bot en modo watch:

```bash
cargo install cargo-watch --locked
cargo watch -x run
```

`cargo watch` a secas corre `cargo check`; con `-x run` reinicia el bot cada vez que guardás un archivo de `src/`.

> Ejecutá siempre el bot desde la raíz del proyecto: ahí busca el `.env` y la carpeta `assets/`.

## Estructura

```
Cargo.toml
.env            # token (no se versiona)
assets/         # photo_<id>.jpg, imágenes 1–898 usadas por pkimg
src/main.rs     # cliente, restricción de canal, comandos de barra, saludo y pkimg
src/music.rs    # cola compartida, banner y comandos yt/jmusic
src/jellyfin.rs # login y API de Jellyfin (álbumes, canciones y stream mp3)
```

## Pruebas en distrobox

Si usás un contenedor de distrobox (por ejemplo `rust-gui`), entrá con la ruta absoluta del proyecto porque el contenedor abre en tu home:

```bash
distrobox enter rust-gui -- bash -lc 'cd /run/media/system/assets/GITHUB/BotDiscord && cargo test'
distrobox enter rust-gui -- bash -lc 'cd /run/media/system/assets/GITHUB/BotDiscord && cargo run'
```

Para quedarte con una shell dentro del contenedor y trabajar normalmente:

```bash
distrobox enter rust-gui
cd /run/media/system/assets/GITHUB/BotDiscord
cargo run
```

## Verificación

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

## Notas

- Si el `.env` no existe o no tiene `DISCORD_TOKEN`, el bot se cierra con un mensaje de error claro.
- `yt` requiere que quien lo usa esté en un canal de voz; el bot se une y, cuando la cola queda vacía, sale solo del canal.
- El tope de 10 canciones pendientes solo cuenta los links de YouTube (la que está sonando no cuenta para el límite). Los álbumes de Jellyfin se encolan completos.
- El banner con progreso se edita sobre el último banner enviado en el servidor, así que no se llena el canal de mensajes nuevos.
- La cola y el estado de pausa viven en memoria: si reiniciás el bot se pierden.
- La salida de `yt-dlp` se deduce con los codecs de symphonia habilitados en `Cargo.toml` (`mkv`/Opus, `isomp4`/AAC y `mp3`). Si un video no suena, revisá que `yt-dlp` esté actualizado.
- Los comandos de IA, música completa y generación de imágenes que tenía la versión anterior en TypeScript fueron eliminados en el port a Rust.
