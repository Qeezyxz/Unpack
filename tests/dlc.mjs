// E2E: DLC auto-install after a batch extract + manual "Install DLC" (headless Chrome + Rust bridge).
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";
const TMP = os.tmpdir();
import { execFileSync, spawn } from "child_process";
const W = path.join(TMP, "unpack-dlc-e2e"), OUT = W + "/out", SRC = W + "/src";
fs.rmSync(W, { recursive: true, force: true });
fs.mkdirSync(SRC + "/pack/sub", { recursive: true });
const mk = (...a) => execFileSync("python3", [new URL("./make_stfs.py", import.meta.url).pathname, ...a]);
mk(SRC + "/pack/sub/MAPPACK1", "LIVE", "2", "4D5307E6", "Mythic Map Pack");
mk(SRC + "/pack/TU_HALO", "CON", "B0000", "4D5307E6", "Title Update 3");
mk(SRC + "/pack/OTHERDLC", "PIRS", "2", "0ABCDEF1", "Other Game DLC");
const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za"].find((x) => fs.existsSync(x));
execFileSync(SZ, ["a", "-tzip", "-bso0", W + "/dlcs.zip", "pack"], { cwd: SRC });
// the game: a GOD folder in the output folder, an existing different DLC with the same name
fs.mkdirSync(OUT + "/Halo 3 (GOD)/4D5307E6/00007000", { recursive: true });
mk(OUT + "/Halo 3 (GOD)/4D5307E6/00007000/HDR", "LIVE", "7000", "4D5307E6", "Halo 3");
fs.mkdirSync(OUT + "/Halo 3 (GOD)/4D5307E6/00000002", { recursive: true });
fs.writeFileSync(OUT + "/Halo 3 (GOD)/4D5307E6/00000002/MAPPACK1", "SOMETHING ELSE");
// a second library with the other game (console layout), added in Settings
const LIB2 = W + "/usb";
fs.mkdirSync(LIB2 + "/Content/0000000000000000/0ABCDEF1/00007000", { recursive: true });
const tauri = new URL("../src-tauri/", import.meta.url).pathname;
const PORT = 18900 + Math.floor(Math.random() * 40);
const br = spawn("./target/debug/examples/bridge", ["../src"], {
  cwd: tauri,
  env: {
    ...process.env,
    UNPACK_BRIDGE_PORT: String(PORT),
    UNPACK_TEST_DRIVES: LIB2,
    UNPACK_7ZA: tauri + "binaries/7za-x86_64-unknown-linux-gnu",
  },
  stdio: ["ignore", "ignore", "pipe"],
});
await new Promise((r) => br.stderr.on("data", (d) => /bridge on/.test(d) && r()));
const BR = `http://127.0.0.1:${PORT}`;
const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
const p = await b.newPage(); await p.setViewport({ width: 1180, height: 900 });
const errors = []; p.on("pageerror", (e) => errors.push(e.message));
await p.evaluateOnNewDocument(fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8"));
await p.evaluateOnNewDocument(() => { if (!sessionStorage.getItem("x")) { localStorage.clear(); localStorage.setItem("unpack.theme", "dark"); sessionStorage.setItem("x", "1"); } });
await p.goto(`${BR}/`, { waitUntil: "load" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
await wait(900);
let fails = 0; const check = (n, ok, i = "") => { if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + n + (i ? " — " + i : "")); };
check("auto DLC on, move off by default", await p.evaluate(() => localStorage.getItem("unpack.autoDlc") !== "0" && localStorage.getItem("unpack.moveDlc") !== "1"));
await p.evaluate((o) => { window.__TEST.folder = o; }, OUT);
await p.select("[data-setting=destMode]", "folder"); await wait(300);
await p.evaluate((x) => window.__TEST.drop(x), [W + "/dlcs.zip"]);
await p.waitForFunction(() => [...document.querySelectorAll(".qrow.sub .qstatus")].filter((e) => /DLC/.test(e.textContent) && !/waiting|looking/.test(e.textContent)).length >= 2, { timeout: 30000 }).catch(() => {});
await wait(500);
const rows = () => p.evaluate(() => [...document.querySelectorAll(".qrow.sub")].map((r) => r.textContent.replace(/\s+/g, " ").trim()));
let r = await rows(); console.log(r);
const dl = OUT + "/Halo 3 (GOD)/4D5307E6/00000002";
check("DLC installed into its game, conflicting name not overwritten", fs.readFileSync(dl + "/MAPPACK1", "utf8") === "SOMETHING ELSE" && fs.existsSync(dl + "/MAPPACK1-2"), fs.readdirSync(dl).join(","));
check("title update placed in 000B0000", r.some((t) => /Title update installed/i.test(t)) && fs.existsSync(OUT + "/Halo 3 (GOD)/4D5307E6/000B0000/TU_HALO") && !fs.existsSync(dl + "/TU_HALO"));
check("other game: Game not found (TitleID 0ABCDEF1)", r.some((t) => /Game not found \(TitleID 0ABCDEF1\)/.test(t)));
check("copy, not move: extracted DLC still there", fs.existsSync(OUT + "/pack/sub/MAPPACK1"));
// Settings: scan drives (bridge maps it to UNPACK_TEST_DRIVES) and approve the found root
await p.click("[data-action=settings]"); await wait(300);
await p.click("[data-lib-scan]"); await wait(500);
const found = await p.evaluate(() => [...document.querySelectorAll("[data-root-add]")].length);
check("scan drives lists Content folder for approval (not auto-added)", found === 1 && (await p.evaluate(() => localStorage.getItem("unpack.libraries"))) === null);
await wait(500); if (process.env.UPDATE_SHOTS === "1") await p.screenshot({ path: new URL("../screenshots/11-settings-dlc-library.png", import.meta.url).pathname });
await p.click("[data-root-add]"); await wait(200);
await p.click("[data-settings-close]"); await wait(200);
// manual Install DLC for the other game, now that its library is known
await p.evaluate((x) => { window.__TEST.pick = x; }, [OUT + "/pack/OTHERDLC", OUT + "/pack/OTHERDLC"]);
await p.click("[data-action=dlc]");
await wait(2500);
r = await rows(); console.log(r);
check("manual DLC installed into console-layout library", fs.existsSync(LIB2 + "/Content/0000000000000000/0ABCDEF1/00000002/OTHERDLC"));
check("same DLC twice: identical skipped", r.some((t) => /already installed \(identical/.test(t)) && fs.readdirSync(LIB2 + "/Content/0000000000000000/0ABCDEF1/00000002").length === 1);
const SHOTS = process.env.UPDATE_SHOTS === "1" ? new URL("../screenshots/", import.meta.url).pathname : path.join(TMP, "unpack-shots-dlc/");
fs.mkdirSync(SHOTS, { recursive: true });
check("batch summary shown", await p.evaluate(() => /Batch finished/.test(document.querySelector(".summary")?.textContent || "")));
await p.screenshot({ path: SHOTS + "09-dlc-queue-summary.png" });
// Library view
await p.keyboard.down("Control"); await p.keyboard.press("l"); await p.keyboard.up("Control");
await p.waitForFunction(() => document.querySelectorAll(".lib-card").length >= 2, { timeout: 10000 }).catch(() => {});
const cards = await p.evaluate(() => [...document.querySelectorAll(".lib-card")].map((c) => c.textContent.replace(/\s+/g, " ").trim()));
console.log(cards);
check("library lists both games with DLC/TU counts", cards.length === 2 && cards.some((c) => /4D5307E6/.test(c) && /2 DLC/.test(c) && /1 TU/.test(c)));
await wait(700); await p.screenshot({ path: SHOTS + "10-library.png" });
// Copy to USB
const USB = W + "/stick"; fs.mkdirSync(USB);
await p.evaluate((x) => { window.__TEST.folder = x; }, USB);
const idx = await p.evaluate(() => [...document.querySelectorAll(".lib-card")].findIndex((c) => /4D5307E6/.test(c.textContent)));
await p.click(`[data-lib-usb="${idx}"]`);
await wait(1500);
const ud = USB + "/Content/0000000000000000/4D5307E6";
check("copy to USB: game + DLC + TU copied", fs.existsSync(ud + "/00007000") && fs.readdirSync(ud + "/00000002").length === 2 && fs.existsSync(ud + "/000B0000/TU_HALO"));
check("copy to USB: originals kept", fs.existsSync(dl + "/MAPPACK1-2"));
await p.keyboard.press("Escape"); await wait(200);
check("Esc closes library", !(await p.$("[data-library]")));
await p.keyboard.press("?"); await wait(200);
check("? shows shortcuts", !!(await p.$(".sc-list")));
await p.keyboard.press("Escape"); await wait(200);
check("tips dismissible", await p.evaluate(() => { document.querySelector("[data-tips-done]")?.click(); return localStorage.getItem("unpack.tipsDone") === "1"; }));
check("no page errors", !errors.length, errors.join("; "));
await b.close();
br.kill();
fs.rmSync(W, { recursive: true, force: true });
if (process.env.UPDATE_SHOTS !== "1") fs.rmSync(path.join(TMP, "unpack-shots-dlc"), { recursive: true, force: true });
process.exit(fails ? 1 : 0);
