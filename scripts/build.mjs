import { mkdir, rm, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import * as esbuild from "esbuild";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = resolve(root, "dist");

await rm(dist, { recursive: true, force: true });
await mkdir(resolve(dist, "assets"), { recursive: true });

await esbuild.build({
  entryPoints: [resolve(root, "src/main.tsx")],
  bundle: true,
  format: "esm",
  target: "es2022",
  sourcemap: true,
  minify: true,
  outfile: resolve(dist, "assets/main.js"),
  loader: {
    ".css": "css"
  },
  assetNames: "assets/[name]-[hash]"
});

await writeFile(
  resolve(dist, "index.html"),
  `<!doctype html>
<html lang="tr">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>BeautySaloon</title>
    <script type="module" crossorigin src="/assets/main.js"></script>
    <link rel="stylesheet" crossorigin href="/assets/main.css" />
  </head>
  <body>
    <div id="root"></div>
  </body>
</html>
`
);
await writeFile(resolve(dist, "assets/build-meta.json"), JSON.stringify({ builder: "esbuild", createdAtUtc: new Date().toISOString() }, null, 2));
