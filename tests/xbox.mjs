// E2E: Send to Xbox (headless Chrome + Rust bridge + fake_aurora.py, a pyftpdlib server that
// mimics Aurora: xboxftp/xboxftp, no MLSD/EPSV, one connection). Local loopback only.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";
const TMP = os.tmpdir();
import { execFileSync, spawn } from "child_process";
const W = path.join(TMP, "unpack-xbox-e2e"), ROOT = W + "/console", LIB = W + "/lib";
const FTP_PORT = 2121 + Math.floor(Math.random() * 500);
fs.rmSync(W, { recursive: true, force: true });
fs.mkdirSync(ROOT, { recursive: true });
const here = new URL(".", import.meta.url).pathname;
const stfs = (out, ct, tid, name) => execFileSync("python3", [here + "make_stfs.py", out, "LIVE", ct, tid, name]);
// Library: one GOD game with DLC, one extracted XEX game
fs.mkdirSync(LIB + "/4D5307E6/00007000", { recursive: true }); fs.mkdirSync(LIB + "/4D5307E6/00000002", { recursive: true });
stfs(LIB + "/4D5307E6/00007000/ABCDEF0123", "7000", "4D5307E6", "Halo 3");
fs.mkdirSync(LIB + "/4D5307E6/00007000/ABCDEF0123.data", { recursive: true });
fs.writeFileSync(LIB + "/4D5307E6/00007000/ABCDEF0123.data/Data0000", Buffer.alloc(3 << 20, 7));
stfs(LIB + "/4D5307E6/00000002/MAPPACK1", "2", "4D5307E6", "Map Pack");
const xex = Buffer.alloc(0x400); xex.write("XEX2", 0); xex.writeUInt32BE(1, 0x14); xex.writeUInt32BE(0x00040006, 0x18); xex.writeUInt32BE(0x100, 0x1c); xex.writeUInt32BE(0x545408A7, 0x10c);
fs.mkdirSync(LIB + "/GTAV", { recursive: true }); fs.writeFileSync(LIB + "/GTAV/default.xex", xex); fs.writeFileSync(LIB + "/GTAV/data.rpf", Buffer.alloc(100000, 3));
// Loose files
stfs(W + "/DLCPACK", "2", "415608C3", "DLC");
stfs(W + "/TUPDATE", "B0000", "415608C3", "TU");
const plug = Buffer.alloc(0x400); plug.write("XEX2", 0); fs.writeFileSync(W + "/plugin.xex", plug);
fs.mkdirSync(W + "/MyApp/cfg", { recursive: true }); fs.writeFileSync(W + "/MyApp/app.xex", "app"); fs.writeFileSync(W + "/MyApp/cfg/a.ini", "x=1");
fs.writeFileSync(W + "/secret.txt", "SECRET"); fs.symlinkSync(W + "/secret.txt", W + "/MyApp/link.txt");
fs.writeFileSync(W + "/big.bin", Buffer.alloc(400 << 20, 9));

const ftpProcs = [];
const trackFtp = (...args) => {
  const p = spawn(...args);
  ftpProcs.push(p);
  return p;
};
const killFtps = () => {
  for (const p of ftpProcs) {
    try {
      p.kill();
    } catch {}
  }
};
process.on("exit", killFtps);

const srv = trackFtp("python3", [here + "fake_aurora.py", ROOT, String(FTP_PORT), "--limit", "40"], { stdio: ["ignore", "pipe", "pipe"] });
await new Promise((r) => srv.stdout.on("data", (d) => /fake aurora/.test(d) && r()));
const BR = `http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}`;
await fetch(BR + "/invoke/__test_grant", { method: "POST", body: JSON.stringify({ paths: [LIB] }) });

const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
const p = await b.newPage(); await p.setViewport({ width: 1240, height: 940 });
const errors = []; p.on("pageerror", (e) => errors.push(e.message));
const external = []; p.on("request", (r) => { if (!/^http:\/\/127\.0\.0\.1/.test(r.url()) && !r.url().startsWith("data:")) external.push(r.url()); });
await p.evaluateOnNewDocument(fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8"));
await p.evaluateOnNewDocument((lib) => { if (!sessionStorage.getItem("x")) { localStorage.clear(); localStorage.setItem("unpack.theme", "dark"); localStorage.setItem("unpack.tipsDone", "1"); localStorage.setItem("unpack.libraries", JSON.stringify([lib])); sessionStorage.setItem("x", "1"); } }, LIB);
await p.goto(BR + "/", { waitUntil: "load" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const until = async (fn, ms = 20000) => { const t = Date.now(); while (Date.now() - t < ms) { if (await fn()) return true; await wait(100); } return false; };
let fails = 0, total = 0; const check = (n, ok, i = "") => { total++; if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + n + (i ? " — " + i : "")); };
const R = (rel) => path.join(ROOT, rel);
const txt = (sel) => p.evaluate((s) => document.querySelector(s)?.textContent || "", sel);
const setField = async (f, v) => { await p.evaluate((f, v) => { const e = document.querySelector(`[data-xb-f=${f}]`); e.value = v; e.dispatchEvent(new Event("input", { bubbles: true })); }, f, v); };
// DOM clicks (the re-rendered dialog animates in, which makes mouse clicks flaky in headless Chrome)
const click = (sel) => p.evaluate((s) => { const e = document.querySelector(s); if (!e) throw new Error("no " + s); e.click(); }, sel);
const sends = () => p.evaluate(() => [...document.querySelectorAll(".xb-send")].map((e) => e.textContent.replace(/\s+/g, " ").trim()));
const waitXboxIdle = async (ms = 120000) => until(async () => !(await p.$("[data-xb-cancel]")), ms);
const openDrive = async (name) => {
  await until(async () => p.evaluate((name) => [...document.querySelectorAll("[data-xb-open]")].some((b) => new RegExp(`^${name}$`, "i").test(b.textContent.trim())), name), 30000);
  await p.evaluate((name) => {
    const e = [...document.querySelectorAll("[data-xb-open]")].find((b) => new RegExp(`^${name}$`, "i").test(b.textContent.trim()));
    if (!e) throw new Error("no drive " + name);
    e.click();
  }, name);
};
const SHOTS = process.env.UPDATE_SHOTS === "1" ? new URL("../screenshots/", import.meta.url).pathname : path.join(TMP, "unpack-shots-xbox/");
fs.mkdirSync(SHOTS, { recursive: true });

await wait(800);
await click("[data-action=xbox]"); await wait(300);
check("dialog opens with connect art", /Connect to your console/.test(await txt("[data-xbox]")));
await setField("host", "8.8.8.8"); await click("[data-xb-connect]");
await until(async () => !!(await p.$("[data-xb-error]")));
check("public IP refused", /local network/.test(await txt("[data-xb-error]")));
await setField("host", "127.0.0.1"); await setField("port", String(FTP_PORT)); await setField("pass", "nope");
await click("[data-xb-connect]"); await until(async () => !!(await p.$("[data-xb-error]")) || !!(await p.$("[data-xb-open]")));
check("wrong password explained", /Login refused/.test(await txt("[data-xb-error]")), await txt("[data-xb-error]"));
await setField("pass", "xboxftp"); await click("[data-xb-connect]");
await until(async () => !!(await p.$("[data-xb-open]"))); await wait(400);
const ents = await p.evaluate(() => [...document.querySelectorAll("[data-xb-open]")].map((e) => e.textContent.trim()));
check("test connection lists drives", ents.join() === "Hdd1,Usb0", ents.join());
await wait(500); await setField("name", "Living room"); await click("[data-xb-save]"); await wait(300);
check("console saved", (await p.evaluate(() => JSON.parse(localStorage.getItem("unpack.consoles"))[0])).host === "127.0.0.1");

// Loose files: DLC + TU + plugin auto-detected
await p.evaluate((w) => { window.__TEST.pick = [w + "/DLCPACK", w + "/TUPDATE", w + "/plugin.xex"]; }, W);
await click("[data-xb-files]"); await until(async () => (await sends()).length === 3);
const kinds = await p.evaluate(() => [...document.querySelectorAll("[data-xb-kind]")].map((e) => e.value));
check("DLC / TU / plugin detected", kinds.join() === "dlc,tu,plugins", kinds.join());
await wait(400);
const dests = () => p.evaluate(() => [...document.querySelectorAll("[data-xb-dest]")].map((e) => e.value));
check("destinations pre-filled with standard folders", (await dests()).join() === "/Hdd1/Content/0000000000000000/415608C3/00000002,/Hdd1/Content/0000000000000000/415608C3/000B0000,/Hdd1/Plugins", (await dests()).join());
const runAll = async () => { for (;;) { const b = await p.$("[data-xb-go]:not([disabled])"); const st = await sends(); const idx = await p.evaluate(() => { const bs = [...document.querySelectorAll("[data-xb-go]")]; const i = bs.findIndex((b) => !/again/i.test(b.textContent) && !b.disabled); return i < 0 ? -1 : +bs[i].dataset.xbGo; }); if (idx < 0) return; await click(`[data-xb-go='${idx}']`); await until(async () => !(await p.$("[data-xb-cancel]")) && (await p.evaluate((i) => /Send again|Resume/.test(document.querySelector(`[data-xb-go='${i}']`)?.textContent || "") || !!document.querySelector(".xb-send .danger"), idx))); if (await p.$(".xb-send .danger")) return; } };
await runAll();
check("DLC → 00000002", fs.existsSync(R("Hdd1/Content/0000000000000000/415608C3/00000002/DLCPACK")));
check("TU → 000B0000", fs.existsSync(R("Hdd1/Content/0000000000000000/415608C3/000B0000/TUPDATE")));
check("plugin → Plugins", fs.existsSync(R("Hdd1/Plugins/plugin.xex")));
check("status shows speed and verify", (await sends()).every((s) => /MB\/s avg.*sizes verified/.test(s)), (await sends())[0]);
check("no .part left", !execFileSync("find", [ROOT, "-name", "*.part"]).toString().trim());

// From Library: GOD game (whole TitleID folder) and extracted XEX game
for (const [i, name] of [[0, "GTAV"], [1, "Halo"]]) {
  await click("[data-xb-lib]"); await until(async () => !!(await p.$("[data-lib-xbox]")));
  const idx = await p.evaluate((n) => [...document.querySelectorAll(".lib-card")].findIndex((c) => c.textContent.includes(n)), name === "Halo" ? "4D5307E6" : "545408A7");
  await click(`[data-lib-xbox='${idx}']`); await wait(300);
}
check("library items queued as xex + god", (await p.evaluate(() => [...document.querySelectorAll("[data-xb-kind]")].slice(3).map((e) => e.value))).join() === "xex,god");
await runAll();
check("XEX game → Games/<name>", fs.readFileSync(R("Hdd1/Games/GTAV/default.xex")).length === 0x400 && fs.statSync(R("Hdd1/Games/GTAV/data.rpf")).size === 100000);
check("GOD game + DLC → Content/<TID>", fs.statSync(R("Hdd1/Content/0000000000000000/4D5307E6/00007000/ABCDEF0123.data/Data0000")).size === 3 << 20 && fs.existsSync(R("Hdd1/Content/0000000000000000/4D5307E6/00000002/MAPPACK1")));

// From Mod Menus
await p.evaluate((w) => { localStorage.setItem("unpack.mods", JSON.stringify([{ path: w + "/plugin.xex", notes: "", mode: "plugins" }])); }, W);
await fetch(BR + "/invoke/__test_grant", { method: "POST", body: JSON.stringify({ paths: [W + "/plugin.xex"] }) });
await click("[data-xb-mods]"); await until(async () => !!(await p.$("[data-mod-card='0']")));
await click("[data-mod-card='0']"); await wait(300); await click("[data-mod-xbox]"); await wait(300);
check("mod queued from Mod Menus", (await sends()).length === 6 && (await p.evaluate(() => document.querySelectorAll("[data-xb-kind]")[5].value)) === "plugins");
await click("[data-xb-go='5']"); await until(async () => /already there/.test((await sends())[5] || ""));
check("identical file skipped, not overwritten", /already there/.test((await sends())[5]));

// Folder into the browsed folder (custom), links skipped
await click("[data-xb-open='1']"); await until(async () => /Usb0/.test(await txt(".xb-crumbs")));
await p.evaluate((w) => { window.__TEST.folder = w + "/MyApp"; }, W);
await click("[data-xb-folder]"); await until(async () => (await sends()).length === 7);
await click("[data-xb-go='6']"); await until(async () => /Send again|danger/.test(await p.evaluate(() => document.querySelectorAll(".xb-send")[6].innerHTML)));
check("folder sent to browsed Usb0", fs.readFileSync(R("Usb0/MyApp/cfg/a.ini"), "utf8") === "x=1");
check("symlink not followed", !fs.existsSync(R("Usb0/MyApp/link.txt")) && /1 link skipped/.test((await sends())[6]));
await click("[data-xb-refresh]"); await until(async () => /MyApp/.test(await txt(".xb-list")));
check("browser shows uploaded folder", /MyApp/.test(await txt(".xb-list")));
await p.screenshot({ path: SHOTS + "14-send-to-xbox.png" });

// Conflict: a different-size file on the console stops the send before anything is written
fs.writeFileSync(R("Hdd1/Plugins/plugin.xex"), "DIFFERENT");
await click("[data-xb-go='2']"); await until(async () => /different size/.test((await sends())[2]));
check("conflict refused", /different size/.test((await sends())[2]) && fs.readFileSync(R("Hdd1/Plugins/plugin.xex"), "utf8") === "DIFFERENT");

// Big file: live MB/s + ETA, one-connection lock, cancel, resume
await p.evaluate((w) => { window.__TEST.pick = [w + "/big.bin"]; }, W);
await click("[data-xb-files]"); await until(async () => (await sends()).length === 8);
await p.select("[data-xb-kind='7']", "custom"); await wait(400);
await click("[data-xb-go='7']");
await until(async () => /MB\/s · ETA/.test((await sends())[7]), 30000);
const live = (await sends())[7];
check("live MB/s and ETA", /\d+(\.\d)? MB\/s · ETA \d+:\d\d/.test(live), live);
const busy = await p.evaluate((port) => window.__TAURI__.core.invoke("ftp_list", { host: "127.0.0.1", port, user: "xboxftp", pass: "xboxftp", path: "/" }).then(() => "ok", (e) => String(e)), FTP_PORT);
check("one connection at a time", /already talking/.test(busy), busy);
await click("[data-xb-cancel='7']"); await until(async () => /Cancelled/.test((await sends())[7]));
const partSize = fs.existsSync(R("Usb0/big.bin.part")) ? fs.statSync(R("Usb0/big.bin.part")).size : 0;
check("cancel keeps .part", /Cancelled/.test((await sends())[7]) && partSize > 0 && partSize < 400 << 20 && !fs.existsSync(R("Usb0/big.bin")), String(partSize));
await click("[data-xb-go='7']"); await until(async () => !/Cancelled/.test((await sends())[7]));
await until(async () => /Send again|danger/.test(await p.evaluate(() => document.querySelectorAll(".xb-send")[7].innerHTML)), 120000);
check("resume completes + verified", /1 resumed.*sizes verified/.test((await sends())[7]) && fs.statSync(R("Usb0/big.bin")).size === 400 << 20, (await sends())[7]);

// Editable destinations: validation, Browse, Reset, last folder per type, Send all to this folder
fs.writeFileSync(W + "/one.xex", "1"); fs.writeFileSync(W + "/two.xex", "22");
const n0 = (await sends()).length;
await p.evaluate((w) => { window.__TEST.pick = [w + "/one.xex", w + "/two.xex"]; }, W);
await click("[data-xb-files]"); await until(async () => (await sends()).length === n0 + 2); await wait(400);
const A = n0, B2 = n0 + 1;
const def = (await dests())[A];
check("loose file defaults to the open folder", def === "/Usb0", def);
const setDest = async (i, v) => p.evaluate((i, v) => { const e = document.querySelector(`[data-xb-dest='${i}']`); e.value = v; e.dispatchEvent(new Event("input", { bubbles: true })); e.dispatchEvent(new Event("change", { bubbles: true })); }, i, v);
const destErr = (i) => txt(`[data-xb-desterr='${i}']`);
const goDisabled = (i) => p.evaluate((i) => document.querySelector(`[data-xb-go='${i}']`).disabled, i);
await setDest(A, "/Hdd1/bad:name"); await wait(400);
check("invalid Xbox name refused", /can't store/.test(await destErr(A)) && await goDisabled(A), await destErr(A));
await setDest(A, "/Not-A-Drive/x"); await wait(400);
check("non-drive start refused", /isn't a console drive/.test(await destErr(A)), await destErr(A));
await setDest(A, "/Hdd1/" + "x".repeat(43)); await wait(400);
check("over 42 chars refused", /42 characters/.test(await destErr(A)));
await click(`[data-xb-reset='${A}']`); await wait(400);
check("Reset to default", (await dests())[A] === def && !(await destErr(A)));
await click(`[data-xb-browse='${A}']`); await until(async () => !!(await p.$("[data-xb-pickbar]")));
await until(async () => /Usb0|Hdd1/.test(await txt(".xb-crumbs")));
await click("[data-xb-crumb='0']");
await until(async () => /Hdd1|Usb0/.test(await txt(".xb-list")), 30000);
await openDrive("Usb0");
await until(async () => /MyApp/.test(await txt(".xb-list")));
await p.evaluate(() => [...document.querySelectorAll("[data-xb-open]")].find((b) => /MyApp/.test(b.textContent)).click());
await until(async () => /Use \/Usb0\/MyApp/.test(await txt("[data-xb-pickbar]")));
await click("[data-xb-usehere]"); await wait(400);
check("Browse picks a destination", (await dests())[A] === "/Usb0/MyApp" && /custom/.test((await sends())[A]));
await click(`[data-xb-go='${A}']`); await until(async () => /Send again|danger/.test(await p.evaluate((i) => document.querySelectorAll(".xb-send")[i].innerHTML, A)));
check("sent to the edited folder", fs.readFileSync(R("Usb0/MyApp/one.xex"), "utf8") === "1", (await sends())[A]);
check("last custom folder remembered per type", (await p.evaluate(() => JSON.parse(localStorage.getItem("unpack.xboxLastDest")))).custom === "/Usb0/MyApp");
check("Last chip offered on another item of that type", !!(await p.$(`[data-xb-last='${B2}']`)));
await click(`[data-xb-last='${B2}']`); await wait(400);
check("Last chip applies the folder", (await dests())[B2] === "/Usb0/MyApp");
await click(`[data-xb-reset='${B2}']`); await wait(400);
// Send all to this folder: browse to /Hdd1, make it the target for the whole queue
await click("[data-xb-crumb='0']");
await until(async () => /Hdd1|Usb0/.test(await txt(".xb-list")), 30000);
await openDrive("Hdd1");
await until(async () => !!(await p.$("[data-xb-allhere]")));
await click("[data-xb-allhere]"); await wait(600);
const all = await dests();
check("Send all to this folder sets every item", all.length === n0 + 2 && all.every((d) => d === "/Hdd1"), all.join());
await click(`[data-xb-go='${B2}']`); await until(async () => /Send again|danger/.test(await p.evaluate((i) => document.querySelectorAll(".xb-send")[i].innerHTML, B2)));
check("queue item sent to the shared folder", fs.readFileSync(R("Hdd1/two.xex"), "utf8") === "22", (await sends())[B2]);
const bad = await p.evaluate((w, port) => window.__TAURI__.core.invoke("ftp_send", { id: "y", args: { host: "127.0.0.1", port, user: "xboxftp", pass: "xboxftp", src: w + "/one.xex", kind: "plugins", drive: "Hdd1", dest: "/Hdd1/../../etc" } }).then(() => "sent", (e) => String(e)), W, FTP_PORT);
check("Rust re-checks an edited path", /valid name/.test(bad), bad);
await p.screenshot({ path: SHOTS + "15-send-to-xbox-destinations.png" });

// Path grants: an unpicked file is refused even if the page asks directly
const ung = await p.evaluate((w, port) => window.__TAURI__.core.invoke("ftp_send", { id: "x", args: { host: "127.0.0.1", port, user: "xboxftp", pass: "xboxftp", src: w + "/secret.txt", kind: "custom", drive: "Hdd1", custom: "/Hdd1" } }).then(() => "sent", (e) => String(e)), W, FTP_PORT);
check("ungranted source refused", /wasn't picked/.test(ung) && !fs.existsSync(R("Hdd1/secret.txt")), ung);
// Overseer R10 HIGH-1 repro on pyftpdlib: LIST answers 425 -> nothing is sent, the console file survives
const ROOT2 = W + "/console425", P2 = FTP_PORT + 1;
fs.mkdirSync(ROOT2 + "/Hdd1/Games", { recursive: true }); fs.writeFileSync(ROOT2 + "/Hdd1/Games/game.bin", "existing-18-bytes!");
fs.mkdirSync(W + "/r10", { recursive: true }); fs.writeFileSync(W + "/r10/game.bin", Buffer.alloc(5000, 1));
await fetch(BR + "/invoke/__test_grant", { method: "POST", body: JSON.stringify({ paths: [W + "/r10"] }) });
const srv2 = trackFtp("python3", [here + "fake_aurora.py", ROOT2, String(P2), "--list-425"], { stdio: ["ignore", "pipe", "pipe"] });
await new Promise((r) => srv2.stdout.on("data", (d) => /fake aurora/.test(d) && r()));
const r425 = await p.evaluate((w, port) => window.__TAURI__.core.invoke("ftp_send", { id: "z", args: { host: "127.0.0.1", port, user: "xboxftp", pass: "xboxftp", src: w + "/r10/game.bin", kind: "custom", drive: "Hdd1", custom: "/Hdd1/Games" } }).then((r) => JSON.stringify(r), (e) => String(e)), W, P2);
srv2.kill();
check("R10: failed LIST never overwrites (pyftpdlib 425)", /Couldn't list/.test(r425) && fs.readFileSync(ROOT2 + "/Hdd1/Games/game.bin", "utf8") === "existing-18-bytes!" && !fs.existsSync(ROOT2 + "/Hdd1/Games/game.bin.part"), r425);
// Drives: a console that has Usb0 + Usb1 and no Hdd1. Every drive is listed, the user picks the
// USB, and the choice is remembered per console.
const ROOT3 = W + "/console-usb", P3 = FTP_PORT + 2;
fs.mkdirSync(ROOT3, { recursive: true });
const srv3 = trackFtp("python3", [here + "fake_aurora.py", ROOT3, String(P3), "--drives", "Usb0,Usb1"], { stdio: ["ignore", "pipe", "pipe"] });
await new Promise((r) => srv3.stdout.on("data", (d) => /fake aurora/.test(d) && r()));
while (await p.$("[data-xb-remove]")) { await click("[data-xb-remove='0']"); await wait(50); }
await setField("host", "127.0.0.1"); await setField("port", String(P3)); await setField("pass", "xboxftp");
await click("[data-xb-connect]"); await until(async () => !!(await p.$("[data-xb-defdrive]")) && /Usb1/.test(await txt("[data-xb-drives]")));
const defOpts = () => p.evaluate(() => { const e = document.querySelector("[data-xb-defdrive]"); return e ? [...e.options].map((o) => o.value).join() + "|" + e.value : ""; });
check("drives: every console drive listed, first USB default without Hdd1", (await defOpts()) === "Usb0,Usb1|Usb0", await defOpts());
check("drives: hint to pick the USB", /pick which USB/.test(await txt("[data-xb-drives]")));
await p.evaluate((w) => { window.__TEST.pick = [w + "/plugin.xex"]; }, W);
await click("[data-xb-files]"); await until(async () => (await sends()).length === 1); await wait(400);
const itemDrives = await p.evaluate(() => [...document.querySelector("[data-xb-drive='0']").options].map((o) => o.value).join());
check("drives: item picker lists Usb0 and Usb1", itemDrives === "Usb0,Usb1", itemDrives);
check("drives: default destination on Usb0", (await dests())[0] === "/Usb0/Plugins", (await dests())[0]);
await p.evaluate(() => { const e = document.querySelector("[data-xb-defdrive]"); e.value = "Usb1"; e.dispatchEvent(new Event("change", { bubbles: true })); });
await until(async () => (await dests())[0] === "/Usb1/Plugins");
check("drives: choosing Usb1 moves the default destination", (await dests())[0] === "/Usb1/Plugins", (await dests())[0]);
check("drives: choice remembered per console", (await p.evaluate(() => JSON.parse(localStorage.getItem("unpack.xboxDrives") || "{}")))[`127.0.0.1:${P3}`] === "Usb1");
await click("[data-xb-go='0']"); await until(async () => /Send again|danger/.test(await p.evaluate(() => document.querySelector(".xb-send").innerHTML)));
check("drives: sent to Usb1", fs.existsSync(ROOT3 + "/Usb1/Plugins/plugin.xex") && !fs.existsSync(ROOT3 + "/Usb0/Plugins"), (await sends())[0]);
await click("[data-xb-remove='0']"); await wait(100);
await click("[data-xb-connect]"); await until(async () => !!(await p.$("[data-xb-defdrive]")));
await p.evaluate((w) => { window.__TEST.pick = [w + "/plugin.xex"]; }, W);
await click("[data-xb-files]"); await until(async () => (await sends()).length === 1); await wait(400);
check("drives: remembered Usb1 used next time", (await defOpts()).endsWith("|Usb1") && (await dests())[0] === "/Usb1/Plugins", (await defOpts()) + " " + (await dests())[0]);
// The other console (Hdd1 + Usb0) still defaults to Hdd1.
await click("[data-xb-remove='0']"); await wait(100);
await setField("port", String(FTP_PORT)); await click("[data-xb-connect]");
await until(async () => /Hdd1/.test(await defOpts()));
check("drives: Hdd1 is the default when present", (await defOpts()).startsWith("Hdd1") && (await defOpts()).endsWith("|Hdd1"), await defOpts());
// Stored per-console drives: bad ports / addresses are dropped when the map is next saved.
await p.evaluate(() => { const v = JSON.parse(localStorage.getItem("unpack.xboxDrives") || "{}"); v["1.1.1.1:99999"] = "Usb0"; v["1.1.1.1:0"] = "Usb0"; v["1.1.1.300:21"] = "Usb0"; v["10.0.0.07:021"] = "Usb1"; localStorage.setItem("unpack.xboxDrives", JSON.stringify(v)); });
await p.evaluate(() => { const e = document.querySelector("[data-xb-defdrive]"); e.value = "Usb0"; e.dispatchEvent(new Event("change", { bubbles: true })); }); await wait(200);
const stored = await p.evaluate(() => JSON.parse(localStorage.getItem("unpack.xboxDrives") || "{}"));
check("drives: stored keys need a valid IP and port 1–65535", !("1.1.1.1:99999" in stored) && !("1.1.1.1:0" in stored) && !("1.1.1.300:21" in stored) && stored[`127.0.0.1:${FTP_PORT}`] === "Usb0" && stored[`127.0.0.1:${P3}`] === "Usb1" && stored["10.0.0.7:21"] === "Usb1" && !("10.0.0.07:021" in stored), JSON.stringify(stored));
srv3.kill();
check("no page errors", errors.length === 0, errors.join(" | "));
check("no external requests", external.length === 0, external.join(" "));
killFtps();
await b.close().catch(() => {});
if (process.env.UPDATE_SHOTS !== "1") fs.rmSync(path.join(TMP, "unpack-shots-xbox"), { recursive: true, force: true });
fs.rmSync(W, { recursive: true, force: true });
console.log(`${total - fails}/${total} passed`);
process.exit(fails ? 1 : 0);
