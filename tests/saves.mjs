// E2E: save backup/restore and Aurora screenshot gallery (bridge + fake_aurora.py).
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";
const TMP = os.tmpdir();
import { spawn } from "child_process";

const W = path.join(TMP, "unpack-saves-e2e");
const ROOT = W + "/console";
const LOCAL = W + "/pc";
const FTP_PORT = 21400 + Math.floor(Math.random() * 200);
const here = new URL(".", import.meta.url).pathname;
const BR = `http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}`;

fs.rmSync(W, { recursive: true, force: true });
fs.mkdirSync(ROOT + "/Hdd1/Aurora/Screenshots", { recursive: true });
fs.mkdirSync(LOCAL + "/backup", { recursive: true });
fs.mkdirSync(LOCAL + "/restore", { recursive: true });
fs.mkdirSync(LOCAL + "/shots", { recursive: true });

const saveDir = ROOT + "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
fs.mkdirSync(saveDir, { recursive: true });
fs.writeFileSync(saveDir + "/profile.sav", "OLD-CONSOLE-SAVE");
fs.writeFileSync(LOCAL + "/restore/profile.sav", "NEW-FROM-PC-SAVE");
const png = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4]);
fs.writeFileSync(ROOT + "/Hdd1/Aurora/Screenshots/demo.png", png);

const srv = spawn("python3", [here + "fake_aurora.py", ROOT, String(FTP_PORT)], { stdio: ["ignore", "pipe", "pipe"] });
await new Promise((r) => srv.stdout.on("data", (d) => /fake aurora/.test(d) && r()));

const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
const p = await b.newPage();
await p.setViewport({ width: 1180, height: 900 });
const errors = [];
p.on("pageerror", (e) => errors.push(e.message));

const prompts = [];
p.on("dialog", async (d) => {
  const m = d.message();
  if (d.type() === "prompt") {
    const ans = prompts.shift() ?? "";
    await d.accept(ans);
  } else if (d.type() === "confirm") await d.accept();
  else await d.dismiss();
});

await p.evaluateOnNewDocument(fs.readFileSync(here + "test-shim.js", "utf8"));
await p.evaluateOnNewDocument((fp, backup, restore, shots) => {
  if (!sessionStorage.getItem("saves")) {
    localStorage.clear();
    localStorage.setItem("unpack.theme", "dark");
    localStorage.setItem("unpack.tipsDone", "1");
    sessionStorage.setItem("saves", "1");
  }
  window.__TEST.folder = null;
  window.__TEST.pick = [];
  window.__SAVES = { fp, backup, restore, shots };
}, FTP_PORT, LOCAL + "/backup", LOCAL + "/restore", LOCAL + "/shots");

await p.goto(BR + "/", { waitUntil: "load" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const click = (sel) => p.evaluate((s) => { const e = document.querySelector(s); if (!e) throw new Error("missing " + s); e.click(); }, sel);
const until = async (fn, ms = 12000) => { const t = Date.now(); while (Date.now() - t < ms) { if (await fn()) return true; await wait(150); } return false; };
let fails = 0;
const check = (n, ok, info = "") => { if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + n + (info ? " — " + info : "")); };

await wait(600);
await click("[data-action=xbox]");
await wait(400);
await p.evaluate((port) => {
  const e = document.querySelector('[data-xb-f="host"]');
  e.value = "127.0.0.1";
  e.dispatchEvent(new Event("input", { bubbles: true }));
  const pr = document.querySelector('[data-xb-f="port"]');
  pr.value = String(port);
  pr.dispatchEvent(new Event("input", { bubbles: true }));
}, FTP_PORT);
await click("[data-xb-connect]");
await until(async () => await p.evaluate(() => !!document.querySelector("[data-xb-save-back]")));

// Backup save
prompts.push("0000000000000000", "4D5307E6");
await p.evaluate(() => { window.__TEST.folder = window.__SAVES.backup; });
await click("[data-xb-save-back]");
await until(() => fs.existsSync(LOCAL + "/backup/profile.sav"));
check("save backup downloads file", fs.readFileSync(LOCAL + "/backup/profile.sav", "utf8") === "OLD-CONSOLE-SAVE");

// Restore save (backs up old on console, uploads new)
prompts.push("0000000000000000", "4D5307E6");
await p.evaluate(() => { window.__TEST.folder = window.__SAVES.restore; });
await click("[data-xb-save-restore]");
await until(() => fs.readFileSync(saveDir + "/profile.sav", "utf8") === "NEW-FROM-PC-SAVE");
const backed = fs.readdirSync(ROOT + "/Hdd1/Content/0000000000000000/4D5307E6").find((n) => n.startsWith(".unpack-save-backup-"));
check("restore uploads and backs up console save", !!backed && fs.existsSync(ROOT + "/Hdd1/Content/0000000000000000/4D5307E6/" + backed + "/profile.sav"));

// Screenshot gallery
await click("[data-xb-shots]");
await until(async () => await p.evaluate(() => !!document.querySelector("[data-xb-shot-grid] .xb-shot")));
check("screenshot grid lists demo.png", await p.evaluate(() => /demo\.png/.test(document.querySelector("[data-xb-shot-grid]")?.textContent || "")));
await p.evaluate(() => { window.__TEST.folder = window.__SAVES.shots; });
await click("[data-xb-shots-dl]");
await until(() => fs.existsSync(LOCAL + "/shots/demo.png"));
check("screenshot pull saves file", fs.readFileSync(LOCAL + "/shots/demo.png").equals(png));

// R21 L3: when demo.png is already taken the pull saves demo-2.png; the thumbnail must show that file.
fs.writeFileSync(LOCAL + "/shots/demo.png", Buffer.from("NOT-THE-SCREENSHOT"));
fs.rmSync(LOCAL + "/shots/demo-2.png", { force: true });
await until(async () => await p.evaluate(() => !!document.querySelector("[data-xb-shots-dl]")));
await click("[data-xb-shots-dl]");
await until(() => fs.existsSync(LOCAL + "/shots/demo-2.png"));
await until(async () => await p.evaluate(() => !!document.querySelector("[data-xb-shot-grid] img")));
const thumb = await p.evaluate(() => document.querySelector("[data-xb-shot-grid] img")?.getAttribute("src") || "");
check("thumbnail uses the real saved file (demo-2.png)", thumb === "data:image/png;base64," + png.toString("base64"), thumb.slice(0, 80));

check("no page errors", !errors.length, errors.join("; "));
await b.close();
srv.kill();
if (fails) process.exit(1);
console.log("saves.mjs: all passed");
