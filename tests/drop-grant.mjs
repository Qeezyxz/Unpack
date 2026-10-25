// Bridge-only: session_drop_prepare must not widen grants without a native drop.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";

const OUT = path.join(os.tmpdir(), "unpack-drop-grant");
fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });
const secretDir = path.join(OUT, "undropped");
fs.mkdirSync(secretDir);
fs.writeFileSync(path.join(secretDir, "secret.txt"), "x");

const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({
  executablePath: "/usr/bin/google-chrome",
  args: ["--no-sandbox"],
  protocolTimeout: 60000,
});
const p = await b.newPage();
await p.evaluateOnNewDocument(shim);
await p.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" });
await new Promise((r) => setTimeout(r, 600));

// Clear any unconsumed pending drop from a reused bridge (CI / local).
await p.evaluate(async () => {
  await window.__TAURI__.core.invoke("session_drop_prepare", { paths: [] });
});

const result = await p.evaluate(async (dir) => {
  const allowed = await window.__TAURI__.core.invoke("session_drop_prepare", { paths: [dir] });
  let denied = false;
  try {
    await window.__TAURI__.core.invoke("list_archives_in_folder", { path: dir });
  } catch {
    denied = true;
  }
  return { allowed, denied };
}, secretDir);

await b.close();

if (result.allowed.length !== 0) {
  console.error("FAIL: session_drop_prepare returned paths without a native drop", result.allowed);
  process.exit(1);
}
if (!result.denied) {
  console.error("FAIL: undropped folder was readable via list_archives_in_folder");
  process.exit(1);
}
console.log("PASS session_drop_prepare without native drop grants nothing");
