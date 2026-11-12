import { build } from "esbuild";
await build({
  entryPoints: ["entry.js"],
  bundle: true,
  format: "esm",
  platform: "browser",
  target: "es2020",
  minify: true,
  outfile: "../../frontend/vendor/wallet.js",
  define: { "process.env.NODE_ENV": '"production"', global: "globalThis" },
  loader: { ".svg": "dataurl", ".png": "dataurl" },
  alias: { buffer: "buffer/", util: "util/" },
  inject: ["./shims.js"],
  logLevel: "warning",
});
