import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const required = ["PLUGIN_ID", "VERSION", "SHA256", "SIZE", "SOURCE_REPOSITORY"];
for (const key of required) {
  if (!process.env[key]) throw new Error(`Missing required environment variable: ${key}`);
}
if (!process.env.MEDIARY_STORE_TOKEN) {
  console.log("MEDIARY_STORE_TOKEN is not configured; skipping Mediary catalog PR.");
  process.exit(0);
}

const pluginId = process.env.PLUGIN_ID;
const version = process.env.VERSION;
const sourceRepository = process.env.SOURCE_REPOSITORY;
const pluginDir = join("plugins", pluginId);
const source = JSON.parse(readFileSync(join(pluginDir, "plugin-source.json"), "utf8"));
const manifest = JSON.parse(readFileSync(join(pluginDir, "plugin.json"), "utf8"));
if (manifest.id !== pluginId || manifest.version !== version) {
  throw new Error("plugin.json ID or version does not match the release inputs");
}

const run = (command, args, options = {}) => execFileSync(command, args, {
  stdio: "inherit",
  ...options,
});
const branch = `automation/publish-${pluginId}-v${version}`;
const archive = `${pluginId}-${version}-linux-amd64.tar.gz`;
const releaseUrl = `https://github.com/${sourceRepository}/releases/download/${pluginId}-v${version}/${archive}`;
const temporary = mkdtempSync(join(tmpdir(), "mediary-catalog-"));
const store = join(temporary, "store");

try {
  const token = encodeURIComponent(process.env.MEDIARY_STORE_TOKEN);
  run("git", ["clone", `https://x-access-token:${token}@github.com/Ladavian/Mediary-Plugins.git`, store]);
  run("git", ["config", "user.name", "Mediary plugin automation"], { cwd: store });
  run("git", ["config", "user.email", "noreply@github.com"], { cwd: store });
  run("git", ["fetch", "origin", "main"], { cwd: store });
  run("git", ["switch", "-C", branch, "origin/main"], { cwd: store });
  const entry = {
    id: pluginId,
    name: manifest.name,
    version,
    description: source.description,
    author: source.author,
    homepage: `https://github.com/${sourceRepository}`,
    source: `https://github.com/${sourceRepository}`,
    api_version: manifest.api_version ?? 1,
    min_mediary_version: source.min_mediary_version,
    permissions: source.permissions ?? [],
    artifacts: {
      "linux-amd64": {
        url: releaseUrl,
        sha256: process.env.SHA256,
        size: Number(process.env.SIZE),
      },
    },
  };
  writeFileSync(join(store, "plugins", `${pluginId}.json`), `${JSON.stringify(entry, null, 2)}\n`);
  run("node", ["scripts/build-catalog.mjs"], { cwd: store });
  run("git", ["add", "catalog.json", `plugins/${pluginId}.json`], { cwd: store });
  run("git", ["commit", "-m", `Publish ${pluginId} ${version}`], { cwd: store });
  run("git", ["push", "-u", "origin", branch], { cwd: store });
  run("gh", ["pr", "create", "--repo", "KyleYu2024/Mediary-Plugins", "--base", "main", "--head", `Ladavian:${branch}`, "--draft", "--title", `Publish ${manifest.name} ${version}`, "--body", `Automated catalog update for ${pluginId} ${version}.\n\nSource: https://github.com/${sourceRepository}\nRelease: ${releaseUrl}`], { cwd: store });
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
