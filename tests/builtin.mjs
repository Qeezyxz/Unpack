// E2E: built-in menus loader (dummy fixtures only; the shipped menus.json is empty).
// Starts its own bridge with UNPACK_TEST_MENUS pointing at a generated fixture folder.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import crypto from "crypto";
import { spawn } from "child_process";
const W = "/tmp/unpack-builtin-e2e", MENUS = W + "/menus", USB = W + "/usb";
fs.rmSync(W, { recursive: true, force: true });
fs.mkdirSync(MENUS + "/sub", { recursive: true }); fs.mkdirSync(USB, { recursive: true });
const plug = Buffer.alloc(0x400); plug.write("XEX2", 0);
fs.writeFileSync(MENUS + "/sub/zorblax.xex", plug);
fs.writeFileSync(MENUS + "/quuxly.xex", plug);
const sha = crypto.createHash("sha256").update(plug).digest("hex");
fs.writeFileSync(MENUS + "/menus.json", JSON.stringify([
  { name: "Zorblax Fixture", file: "sub/zorblax.xex", titleId: "4D5307E6", version: "0.1", sha256: sha },
  { name: "Quuxly Fixture", file: "quuxly.xex", titleId: "415608C3", version: "2" },
  { name: "Broken Fixture", file: "../outside.xex", titleId: "4D5307E6", version: "1" },
]));
fs.writeFileSync(W + "/Zorblax_Fixture_v9.xex", plug); // user's own file: should be auto-detected via the seed
fs.writeFileSync(W + "/Quuxly_Fixture.xex", plug);     // user taught a different game for this name
const PORT = 18890 + Math.floor(Math.random() * 50);
const br = spawn("./target/debug/examples/bridge", ["../src"], { cwd: new URL("../src-tauri/", import.meta.url).pathname, env: { ...process.env, UNPACK_BRIDGE_PORT: String(PORT), UNPACK_TEST_MENUS: MENUS }, stdio: ["ignore", "ignore", "pipe"] });
await new Promise((r) => br.stderr.on("data", (d) => /bridge on/.test(d) && r()));
const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
const p = await b.newPage(); await p.setViewport({ width: 1180, height: 900 });
const errors = []; p.on("pageerror", (e) => errors.push(e.message));
await p.evaluateOnNewDocument(fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8"));
await p.evaluateOnNewDocument(() => { if (!sessionStorage.getItem("x")) { localStorage.clear(); localStorage.setItem("unpack.theme", "dark"); localStorage.setItem("unpack.tipsDone", "1"); sessionStorage.setItem("x", "1"); } });
await p.goto(`http://127.0.0.1:${PORT}/`, { waitUntil: "load" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const click = (sel) => p.evaluate((s) => document.querySelector(s).click(), sel);
let fails = 0, total = 0; const check = (n, ok, i = "") => { total++; if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + n + (i ? " — " + i : "")); };
const cards = () => p.evaluate(() => [...document.querySelectorAll(".mod-card")].map((r) => r.textContent.replace(/\s+/g, " ").trim()));
await wait(800);
const api = await p.evaluate(() => window.__TAURI__.core.invoke("builtin_menus"));
check("loader: 2 valid, broken skipped", api.menus.length === 2 && api.problems.length === 1 && /leaves the menus folder/.test(api.problems[0]), JSON.stringify(api.problems));
const k = await p.evaluate(() => window.__TAURI__.core.invoke("mod_name_key", { name: "Quuxly Fixture", tid: "4D5307E6" }));
await p.evaluate((key) => localStorage.setItem("unpack.modNames", JSON.stringify([{ key, tid: "4D5307E6", gname: "Halo 3" }])), k.key);
await click("[data-action=mods]"); await wait(900);
let c = await cards();
check("built-in items listed with pill", c.length === 2 && c.every((t) => /Built-in/.test(t)) && /Zorblax Fixture.*Halo 3/.test(c[0]), c.join(" | "));
await click("[data-mod-card='0']"); await wait(300);
const det = await p.evaluate(() => { const d = document.querySelector("[data-mod-details]"); return { remove: !!d.querySelector("[data-mod-remove]"), notes: !!d.querySelector("[data-mod-notes]"), text: d.textContent.replace(/\s+/g, " ") }; });
check("read-only: no remove, no notes", !det.remove && !det.notes && /Built-in \(read-only\) · version 0\.1 · SHA-256 checked/.test(det.text), det.text.slice(0, 300));
check("user mods list stays empty", await p.evaluate(() => JSON.parse(localStorage.getItem("unpack.mods") || "[]").length === 0));
await p.evaluate((u) => { window.__TEST.folder = u; }, USB);
await click("[data-mod-copy]"); await wait(900);
check("built-in file copies to USB (session grant)", fs.existsSync(USB + "/Plugins/zorblax.xex"));
await p.evaluate((w) => { window.__TEST.pick = [w + "/Zorblax_Fixture_v9.xex", w + "/Quuxly_Fixture.xex"]; }, W);
await click("[data-mods-add-files]"); await wait(900);
c = await cards();
check("seed auto-detects user's file", /Zorblax_Fixture_v9.*Halo 3/.test(c[2]), c[2]);
check("user's learned name wins over built-in", /Quuxly_Fixture.*Halo 3/.test(c[3]) && !/415608C3/.test(c[3]), c[3]);
check("user mods saved without built-ins", await p.evaluate(() => JSON.parse(localStorage.getItem("unpack.mods")).length === 2));
check("no page errors", errors.length === 0, errors.join(" | "));
await b.close(); br.kill();
fs.rmSync(W, { recursive: true, force: true });
console.log(`${total - fails}/${total} passed`);
process.exit(fails ? 1 : 0);
