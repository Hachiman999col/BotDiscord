import axios from "axios";
import {
  AttachmentBuilder,
  Client,
  Events,
  Message,
  OmitPartialGroupDMChannel,
} from "discord.js";

async function generate(
  txt: string,
  message: OmitPartialGroupDMChannel<Message<boolean>>
) {
  const existNg = txt.includes("ng:");

  const splitData = txt.split("ng:");
  const prompt = existNg ? splitData[0] : txt;
  const ngPrompt = existNg ? splitData[1] : "";

  const payload = {
    prompt: prompt,
    negative_prompt: ngPrompt,
    width: 512,
    height: 512,
    steps: 20,
    sampler_name: "DPM++ 2M",
    cfg_scale: 7,
    restore_faces: false,
  };
  console.log({ payload });
  const response = await axios.post(
    "http://127.0.0.1:7860/sdapi/v1/txt2img",
    payload
  );

  const imageBase64 = response.data.images[0];
  const buffer = Buffer.from(imageBase64, "base64");

  const attachment = new AttachmentBuilder(buffer, { name: "imagen.png" });
  message.reply({ files: [attachment] });
}

export function generateImage(client: Client<boolean>) {
  client.on(Events.MessageCreate, (message) => {
    const txtRaw: string = message.content;
    if (!txtRaw.includes("/image")) return;
    const txt = txtRaw.replace("/image", "");
    generate(txt, message);
  });
}
/**
 * doc
 {
  "prompt": "A futuristic city with flying cars",
  "negative_prompt": "blurry, low quality",
  "steps": 50,
  "sampler_name": "DPM++ 2M Karras",
  "seed": 12345,
  "cfg_scale": 7.5,
  "width": 512,
  "height": 512,
  "restore_faces": true,
  "enable_hr": true,
  "hr_scale": 2,
  "hr_upscaler": "Latent",
  "init_images": ["data:image/png;base64,..."],
  "denoising_strength": 0.7,
  "batch_size": 2,
  "n_iter": 1,
  "override_settings": {
    "CLIP_stop_at_last_layers": 2
  }
}

Parámetros básicos

Propiedad	Tipo	Descripción

prompt	String	Texto descriptivo para generar la imagen.
negative_prompt	String	Texto para elementos a evitar en la imagen.
steps	Integer	Número de pasos de muestreo (por defecto: 20).
sampler_name	String	Método de sampling (e.g., Euler, DDIM, DPM2, etc.).
seed	Integer	Semilla para reproducibilidad.
cfg_scale	Float	Escala de orientación del prompt (mayor = más adherencia al prompt, por defecto: 7).
width	Integer	Ancho de la imagen (en píxeles, típicamente múltiplos de 64).
height	Integer	Alto de la imagen (en píxeles, típicamente múltiplos de 64).
restore_faces	Boolean	Activa la restauración de caras (true/false).
tiling	Boolean	Genera texturas en mosaico (true/false).
batch_size	Integer	Número de imágenes generadas por solicitud.
n_iter	Integer	Número de lotes a generar.

Control de imágenes iniciales

Propiedad	Tipo	Descripción

init_images	Array[String]	Lista de imágenes iniciales codificadas en base64.
denoising_strength	Float	Nivel de ruido para modificar la imagen inicial (0-1).
Control avanzado
Propiedad	Tipo	Descripción
enable_hr	Boolean	Activa la generación de alta resolución.
hr_scale	Float	Escala de ampliación para alta resolución.
hr_upscaler	String	Métodos de upscaling (e.g., Latent, ESRGAN, etc.).
hr_second_pass_steps	Integer	Pasos adicionales para la fase de alta resolución.
Control de composiciones (inpainting/outpainting)

Propiedad	Tipo	Descripción

mask	String	Máscara (codificada en base64) para definir las áreas a modificar.
mask_blur	Integer	Nivel de desenfoque aplicado a la máscara.
inpainting_fill	Integer	Método de relleno para áreas enmascaradas (e.g., 0 para solo modificar, 1 para rellenar).
inpaint_full_res	Boolean	Generar en resolución completa para inpainting.
inpaint_full_res_padding	Integer	Relleno adicional alrededor de las áreas enmascaradas.

Control de colores

Propiedad	Tipo	Descripción

color_correct	Boolean	Ajusta automáticamente los colores en la imagen generada.
Otros ajustes útiles
Propiedad	Tipo	Descripción
clip_skip	Integer	Número de capas CLIP a omitir (mejorar consistencia de estilo).
eta	Float	Parámetro de ruido aleatorio, afecta al realismo de los detalles.
s_churn	Float	Aumenta la variación en el sampling para explorar nuevas posibilidades.
override_settings	Dict	Cambios temporales a configuraciones de la UI.
script_name	String	Especifica un script adicional para ejecutar.
script_args	Array	Argumentos para el script especificado.
 */
