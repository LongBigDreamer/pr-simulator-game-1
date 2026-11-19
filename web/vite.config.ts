import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import { verifiedAssets } from "../tools/asset-check/vite-plugin.js";
import { nonEmpty, record } from "../tools/shared/validation.js";

const repositoryRoot = fileURLToPath(new URL("..", import.meta.url));
let revision = (process.env.BPG_NATIVE_SOURCE_SHA ?? process.env.GITHUB_SHA)?.slice(0, 8);
if (revision === undefined) {
  try {
    revision = execFileSync("git", ["rev-parse", "--short=8", "HEAD"], {
      cwd: repositoryRoot,
      encoding: "utf8"
    }).trim();
  } catch {
    revision = "unknown";
  }
}
const packageVersion = nonEmpty(record(JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"))).version);
const buildLabel = `v${packageVersion} · ${revision} · ${new Date().toISOString().slice(0, 16)}Z`;

export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  base: "./",
  publicDir: false,
  define: { __APP_BUILD_LABEL__: JSON.stringify(buildLabel) },
  plugins: [verifiedAssets(fileURLToPath(new URL("..", import.meta.url)))],
  build: {
    outDir: "dist", emptyOutDir: true
  }
});
