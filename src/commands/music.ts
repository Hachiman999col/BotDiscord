import {
  Client,
  Events,
  Guild,
  Message,
  OmitPartialGroupDMChannel,
  VoiceBasedChannel,
} from "discord.js";
import {
  joinVoiceChannel,
  createAudioPlayer,
  createAudioResource,
  AudioPlayerStatus,
  AudioResource,
  StreamType,
} from "@discordjs/voice";
import ytdl from "ytdl-core";
import ffmpeg from "ffmpeg";

async function connectionYTDL(
  voiceChannel: VoiceBasedChannel,
  guild: Guild,
  url: string
) {
  const connection = joinVoiceChannel({
    channelId: voiceChannel.id,
    guildId: guild.id,
    adapterCreator: guild.voiceAdapterCreator,
  });
  // Crear el recurso de audio a partir de la URL de la radio
  const stream = ytdl(url, {
    filter: "audioonly",
    quality: "highestaudio",
    dlChunkSize: 0,
  });

  const resource = createAudioResource(stream, {
    inputType: StreamType.Arbitrary,
  });
  const player = createAudioPlayer();
  connection.subscribe(player);
  player.play(resource);

  player.on(AudioPlayerStatus.Playing, () => {
    console.log("Reproduciendo audio del video de YouTube...");
  });
  player.on(AudioPlayerStatus.Idle, () => {
    console.log("La transmisión ha finalizado");
    connection.destroy();
  });
}
export function commandsMusic(client: Client<boolean>) {
  client.on(Events.MessageCreate, (message) => {
    if (message.content.startsWith("!play")) {
      const url = message.content.split(" ")[1];

      if (!message.member) {
        message.reply("Debes estar en un canal de voz para reproducir música");
        return;
      }
      if (!message.guild) {
        message.reply("Debes estar en un canal de voz para reproducir música");
        return;
      }
      const voiceChannel = message.member.voice.channel;

      if (!voiceChannel) {
        message.reply("Debes estar en un canal de voz para reproducir música");
        return;
      }
      try {
        connectionYTDL(voiceChannel, message.guild, url);
      } catch (error) {
        console.error(error);
        message.reply("Error al reproducir la música");
      }
    }
  });
}
