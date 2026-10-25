// Bridge-only: the page cannot grant folders; only native-drop-accepted opens paths.
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
const goodArchive = path.join(OUT, "good.7z");
fs.copyFileSync(process.env.TESTDATA ? process.env.TESTDATA + "/plain.7z" : "/workspace/testdata/plain.7z", goodArchive);

const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({
  executablePath: "/usr/bin/google-chrome",
  args: ["--no-sandbox"],
  protocolTimeout: 60000,
});
const p = await b.newPage();
await p.evaluateOnNewDocument(shim);
await p.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" });
await new Promise((r) => setTimeout(r, 800));

const noGrant = await p.evaluate(async (dir) => {
  let prepareMissing = false;
  try {
    await window.__TAURI__.core.invoke("session_drop_prepare", { paths: [dir] });
  } catch {
    prepareMissing = true;
  }
  let denied = false;
  try {
    await window.__TAURI__.core.invoke("list_archives_in_folder", { path: dir });
  } catch {
    denied = true;
  }
  return { prepareMissing, denied };
}, secretDir);

if (!noGrant.prepareMissing) {
  console.error("FAIL: session_drop_prepare must not exist");
  process.exit(1);
}
if (!noGrant.denied) {
  console.error("FAIL: undropped folder must not be readable");
  process.exit(1);
}
console.log("PASS page invoke cannot grant dropped folders");

await p.evaluate((archive) => window.__TEST.drop([archive]), goodArchive);
await p.waitForFunction(
  () => /files ·/.test(document.querySelector(".status-line")?.textContent || ""),
  { timeout: 15000 }
).catch(() => {});
const opened = await p.evaluate(() => /files ·/.test(document.querySelector(".status-line")?.textContent || ""));
if (!opened) {
  console.error("FAIL: accepted native drop did not open archive");
  process.exit(1);
}
console.log("PASS native-drop-accepted opens granted paths only");

await p.evaluate(() => document.querySelector("[data-action=clear-all]")?.click());
await new Promise((r) => setTimeout(r, 200));
await p.evaluate((bad) => window.__TEST.drop([bad]), "/etc");
await p.waitForFunction(
  () => /system location|whole system drive/i.test(document.querySelector(".toast")?.textContent || ""),
  { timeout: 5000 }
).catch(() => {});
const refused = await p.evaluate(() => {
  const t = document.querySelector(".toast")?.textContent || "";
  const rows = document.querySelectorAll(".qrow").length;
  return /system location|whole system drive/i.test(t) && rows === 0;
});
if (!refused) {
  console.error("FAIL: refused drop should toast without opening archives");
  process.exit(1);
}
console.log("PASS refused drop shows toast and does not add paths");

await b.close();
