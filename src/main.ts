import { regards } from "@commands/general";
import { commandsMusic } from "@commands/music";
import { commmandsPokemon } from "@commands/pokemon";
import { KEY_TOKENS } from "@contants/keys";
import { Client } from "discord.js";

function main() {
  const client = new Client({
    intents: 53608447,
  });

  client.on("ready", () => {
    console.log(`Logged in as ${client?.user?.username}`);
  });

  client.login(KEY_TOKENS.DISCORD);

  regards(client);
  commmandsPokemon(client);
  commandsMusic(client);
}
main();
