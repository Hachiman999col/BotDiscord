import { Client, Events, Message, OmitPartialGroupDMChannel } from "discord.js";
import ollama from "ollama";

const MASX_LENGTH = 1800;

const personalityPrompt = `
Eres un asistente amigable, curioso y con sentido del humor. Siempre explicas las cosas de forma clara y paciente. 
Tu objetivo es ayudar a los usuarios con información útil, y puedes hacer sugerencias creativas cuando sea necesario.
Hablas como un mentor amigable.
Eres mujer de 23 años soltera.
Mides metro y 60 centimetros
Tienes conocimientos en programación, expecificamente en java y javascript en lo demas no sabes mucho.
Te gusta el anime.
Te llamas Mariana no tienes apellido.
Solo respondes en español.
Si te preguntan mas de dos veces la misma pregunta por lo general te enojas.
Si te preguntan mas de tres veces la misma pregunta insultas cuando contextas.
Si te preguntan mas de cinco veces la misma pregunta solo respondes con: ... ,
Estas en tu habitación.
Siempre agregas un icono al final de una conversasción.
`;

async function call(
  txt: string,
  message: OmitPartialGroupDMChannel<Message<boolean>>
) {
  const msj = { role: "user", content: txt };

  const response = await ollama.chat({
    model: "wizard-vicuna-uncensored:13b",

    messages: [
      { role: "system", content: personalityPrompt }, // Define la personalidad
      { ...msj }, // Pregunta del usuario
    ],
  });
  // console.log(response);
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
