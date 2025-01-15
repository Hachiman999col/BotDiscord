import { Client, Events, Message, OmitPartialGroupDMChannel } from "discord.js";
import ollama from "ollama";

const MASX_LENGTH = 1800;
async function call(
  txt: string,
  message: OmitPartialGroupDMChannel<Message<boolean>>
) {
  const msj = { role: "user", content: txt };

  const response = await ollama.chat({
    model: "llama3.1:8b",
    messages: [{ ...msj }],
  });
  const content: string =
    response?.message?.content ?? "No se pudo responder, sorry perro";

  const parts = [];
  for (let i = 0; i < content.length; i += MASX_LENGTH) {
    parts.push(content.slice(i, i + MASX_LENGTH));
  }
  for (const part of parts) {
    message.reply(part ?? "No se pudo responder, sorry perro");
  }
}
export function ollamaCall(client: Client<boolean>) {
  client.on(Events.MessageCreate, (message) => {
    const txtRaw: string = message.content;
    if (!txtRaw.includes("/bot")) return;

    const txt = txtRaw.replace("/bot", "");

    call(txt, message);
  });
}
