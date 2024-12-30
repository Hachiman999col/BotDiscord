import { FILES_POKEMON } from "@contants/files";

import {
  Client,
  Events,
  Message,
  OmitPartialGroupDMChannel,
  AttachmentBuilder,
} from "discord.js";

function validIdPokemon(str: string): number {
  let num = 0;
  const split = str.split(" ");
  const id = Number(split[1]);
  num = !isNaN(id) ? id : 0;
  return num;
}

function searchLocalImagePokemon(
  id: number,
  emit: OmitPartialGroupDMChannel<Message<boolean>>
) {
  const imagePath = `${FILES_POKEMON.PHOTOS}${id}${FILES_POKEMON.PHOTOS_EXTENSION}`; // Ruta de la imagen

  const attachment = new AttachmentBuilder(imagePath);

  emit.reply({ files: [attachment] });
}

export function commmandsPokemon(client: Client<boolean>) {
  client.on(Events.MessageCreate, (message) => {
    const msj = message.content;

    if (msj.includes("pkimg")) {
      const id = validIdPokemon(msj);

      if (id > 0 && id <= 898) {
        searchLocalImagePokemon(id, message);
      } else {
        message.reply("El id del pokemon no fue encontrado");
      }
      return;
    }
  });
}
