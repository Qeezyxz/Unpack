// E2E: batch extract into one folder with "Auto-convert ISOs to GOD" on + screenshots of the new UI.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
const SRC = process.env.SRC || "/tmp/gt", OUT = "/tmp/gt-out", SHOTS = process.env.UPDATE_SHOTS === "1" ? new URL("../screenshots/", import.meta.url).pathname : "/tmp/unpack-shots/";
fs.mkdirSync(SHOTS, { recursive: true }); // screenshots/ in the repo is only rewritten with UPDATE_SHOTS=1
fs.rmSync(OUT, { recursive: true, force: true }); fs.mkdirSync(OUT);
// fixtures: built here when missing (an Xbox ISO + a non-Xbox ISO in a zip, and a plain 7z)
if (!fs.existsSync(SRC + "/batch1.zip")) {
  const { execFileSync } = await import("child_process");
  const T = SRC + "/.mk"; fs.mkdirSync(T, { recursive: true });
  execFileSync("python3", [new URL("./make_iso.py", import.meta.url).pathname, T + "/game.iso", "4D5307E6", String(2 * 1024 * 1024)]);
  fs.writeFileSync(T + "/linux.iso", Buffer.alloc(64 * 1024, 1));
  fs.writeFileSync(T + "/readme.txt", "plain");
  const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za"].find((x) => fs.existsSync(x));
  execFileSync(SZ, ["a", "-tzip", "-bso0", SRC + "/batch1.zip", "game.iso", "linux.iso"], { cwd: T });
  execFileSync(SZ, ["a", "-bso0", SRC + "/plain.7z", "readme.txt"], { cwd: T });
  fs.rmSync(T, { recursive: true, force: true });
}
const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
const p = await b.newPage(); await p.setViewport({ width: 1180, height: 820 });
const errors = []; p.on("pageerror", (e) => errors.push(e.message));
await p.evaluateOnNewDocument(fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8"));
await p.evaluateOnNewDocument(() => { if (!sessionStorage.getItem("x")) { localStorage.clear(); localStorage.setItem("unpack.theme", "dark"); sessionStorage.setItem("x", "1"); } });
await p.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" }); await new Promise((r) => setTimeout(r, 900));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let fails = 0; const check = (n, ok, i = "") => { if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + n + (i ? " — " + i : "")); };
check("auto off by default", await p.evaluate(() => (document.querySelector(".desk [data-setting=autoIsoMode]")?.value || "off") === "off"));
await p.screenshot({ path: SHOTS + "01-home-dark.png" });
await p.click("[data-action=settings]"); await wait(400);
await p.screenshot({ path: SHOTS + "02-settings-dark.png" });
await p.click("[data-settings] [data-auto-iso=god]"); await wait(200);
await p.evaluate(() => document.querySelector("[data-set-deliso]").click()); await wait(300);
await p.screenshot({ path: SHOTS + "03-settings-delete-confirm.png" });
check("delete needs confirm", await p.evaluate(() => localStorage.getItem("unpack.delIso") !== "1" && !!document.querySelector("[data-deliso-yes]")));
await p.click("[data-deliso-no]"); await wait(200);
await p.click("[data-settings-close]"); await wait(200);
check("auto on persisted", await p.evaluate(() => localStorage.getItem("unpack.autoIso") === "god" || localStorage.getItem("unpack.autoGod") === "1"));
// folder destination mode
await p.evaluate((o) => { window.__TEST.folder = o; }, OUT);
await p.select("[data-setting=destMode]", "folder"); await wait(400);
await p.evaluate((x) => window.__TEST.drop(x), [SRC + "/batch1.zip", SRC + "/plain.7z"]);
await p.waitForFunction(() => [...document.querySelectorAll(".qrow.sub .qstatus")].filter((e) => /done|Skipped|failed/i.test(e.textContent)).length >= 2, { timeout: 60000 }).catch(() => {});
await wait(500);
await p.screenshot({ path: SHOTS + "04-batch-autogod-dark.png" });
const subs = await p.evaluate(() => [...document.querySelectorAll(".qrow.sub")].map((r) => r.textContent.replace(/\s+/g, " ").trim()));
console.log(subs);
const top = fs.readdirSync(OUT).sort(); console.log(top);
check("GOD folder in output", top.some((n) => /\(GOD\)$/.test(n)));
check("non-Xbox skipped with message", subs.some((t) => /linux\.iso.*Skipped: not an Xbox/.test(t)));
check("ISO kept (delete off)", fs.readdirSync(OUT).filter((n) => !/GOD/.test(n)).some((d) => fs.existsSync(`${OUT}/${d}/game.iso`)));
check("linux.iso kept", fs.readdirSync(OUT).some((d) => fs.existsSync(`${OUT}/${d}/linux.iso`)));
// light theme screenshots
await p.evaluate(() => { localStorage.setItem("unpack.theme", "light"); window.__applyTheme(); });
await wait(300); await p.screenshot({ path: SHOTS + "05-batch-autogod-light.png" });
await p.click(".qname"); await wait(600); await p.screenshot({ path: SHOTS + "06-archive-view-light.png" });
await p.click("[data-action=god]"); await wait(400); await p.screenshot({ path: SHOTS + "07-god-panel-light.png" });
// second run with delete on: only extracted copy deleted after success
await p.evaluate(() => { localStorage.setItem("unpack.delIso", "1"); document.querySelector("[data-god-close]")?.click(); });
const before = fs.readdirSync(OUT).length;
await p.evaluate((x) => window.__TEST.drop(x), [SRC + "/batch1.zip"]);
await p.waitForFunction((n) => document.querySelectorAll(".qrow.sub .qstatus").length >= n && [...document.querySelectorAll(".qrow.sub .qstatus")].slice(0, 2).every((e) => /done|Skipped|failed/i.test(e.textContent)), { timeout: 60000 }, 4).catch(() => {});
await wait(500);
const after = fs.readdirSync(OUT).sort(); console.log(after);
const newExtract = after.find((n) => /^batch1-2$/.test(n));
check("second batch: new folders, nothing reused", after.length === before + 2 && !!newExtract, String(newExtract));
check("extracted ISO deleted after success", newExtract && !fs.existsSync(`${OUT}/${newExtract}/game.iso`));
check("non-Xbox ISO still kept", newExtract && fs.existsSync(`${OUT}/${newExtract}/linux.iso`));
check("source archive untouched", fs.existsSync(SRC + "/batch1.zip"));
// C1: Replace merge into a folder that already holds my own ISO, delete on -> mine kept, extracted kept
fs.mkdirSync(OUT + "/batch1/keep", { recursive: true });
fs.writeFileSync(OUT + "/batch1/keep/game.iso", "MINE");
await p.select("[data-setting=collision]", "replace"); await wait(200);
await p.evaluate((x) => window.__TEST.drop(x), [SRC + "/batch1.zip"]);
await wait(6000);
check("C1: my pre-existing ISO untouched on Replace", fs.readFileSync(OUT + "/batch1/keep/game.iso", "utf8") === "MINE");
check("C1: extracted ISO kept when merged (delete disabled)", fs.existsSync(OUT + "/batch1/game.iso"));
check("C1: pre-existing ISO not converted", !fs.readdirSync(OUT).some((n) => /\(GOD\)-4$/.test(n)), fs.readdirSync(OUT).join(","));
await p.select("[data-setting=collision]", "number"); await wait(200);
await p.evaluate(() => { localStorage.setItem("unpack.theme", "dark"); window.__applyTheme(); }); await wait(200);
await p.setViewport({ width: 760, height: 900 }); await wait(300);
await p.screenshot({ path: SHOTS + "08-narrow-dark.png", fullPage: true });
check("no page errors", !errors.length, errors.join("; "));
await b.close();
fs.rmSync(OUT, { recursive: true, force: true });
if (process.env.UPDATE_SHOTS !== "1") fs.rmSync(SHOTS, { recursive: true, force: true });
process.exit(fails ? 1 : 0);
