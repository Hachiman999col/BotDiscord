import { Client, Events } from "discord.js";

export function regards(client: Client<boolean>) {
  client.on(Events.MessageCreate, (message) => {
    console.log(message.content);
    if (message.content === "hi") {
      message.reply("Hola putita");
    }
  });
}
