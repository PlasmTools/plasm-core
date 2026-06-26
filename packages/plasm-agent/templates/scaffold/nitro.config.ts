import { defineNitroConfig } from "nitropack/config";

export default defineNitroConfig({
  compatibilityDate: "2026-06-26",
  srcDir: ".",
  ignore: ["api/**"],
  devServer: {
    port: Number(process.env.PORT ?? 3000),
    host: process.env.HOST ?? "127.0.0.1",
  },
  typescript: {
    strict: false,
  },
  externals: {
    inline: ["@plasm_lang/engine"],
  },
});
