// Multi-disc ISO handling: the per-game "Disc 1 of 2: …" line, the amber missing-disc warning,
// companions and disc order on the auto (batch extract → GOD) path, and the batch summary warning.
// Headless Chrome + tests/test-shim.js + the real Rust code through examples/bridge.rs.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";
const TMP = os.tmpdir();
import { execFileSync } from "child_process";

const OUT = path.join(TMP, "unpack-multidisc");
const PORT = process.env.UNPACK_BRIDGE_PORT || 18790;
const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za", "/usr/bin/7z"].find((x) => fs.existsSync(x));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
fs.rmSync(OUT, { recursive: true, force: true });
const mk = (dir, name, tid, media, disc, discs) => {
  fs.mkdirSync(dir, { recursive: true });
  execFileSync("python3", [new URL("./make_iso.py", import.meta.url).pathname, dir + "/" + name, tid, String(128 * 1024), media, String(disc), String(discs)]);
};
const F = OUT + "/fx";
// auto batch: header-only disc numbers, names sort disc 2 first
mk(F + "/kilo", "Kilo A.iso", "0BADC0D1", "BBBB0002", 2, 2);
mk(F + "/kilo", "Kilo B.iso", "0BADC0D1", "BBBB0001", 1, 2);
execFileSync(SZ, ["a", "-tzip", "-bso0", "-bsp0", F + "/kilo.zip", "Kilo A.iso", "Kilo B.iso"], { cwd: F + "/kilo" });
// auto batch: disc 2 alone
mk(F + "/lima", "Lima (Disc 2).iso", "0BADC0D2", "CCCC0002", 2, 2);
execFileSync(SZ, ["a", "-tzip", "-bso0", "-bsp0", F + "/lima.zip", "Lima (Disc 2).iso"], { cwd: F + "/lima" });
// GOD panel: disc 1 alone, then disc 2 from elsewhere after disc 1 converted
mk(F + "/mike1", "Mike (Disc 1).iso", "0BADC0D3", "DDDD0001", 1, 2);
mk(F + "/mike2", "Mike (Disc 2).iso", "0BADC0D3", "DDDD0002", 2, 2);
// GOD panel: a similar name of another game next to it must not group
mk(F + "/nov", "November (Disc 2).iso", "0BADC0D4", "EEEE0002", 2, 2);
mk(F + "/nov", "November Legends (Disc 1).iso", "0BADC0D5", "EEEE0011", 1, 2);

const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"], protocolTimeout: 60000 });
const p = await b.newPage();
await p.setViewport({ width: 1180, height: 820 });
const errors = [];
p.on("pageerror", (e) => errors.push(e.message));
await p.evaluateOnNewDocument(fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8"));
await p.evaluateOnNewDocument(() => { if (!sessionStorage.getItem("md")) { localStorage.clear(); sessionStorage.setItem("md", "1"); } });
let fails = 0;
const check = (name, ok, info = "") => { if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + name + (info ? " — " + info : "")); };
const drop = (paths) => p.evaluate((x) => window.__TEST.drop(x), paths);
const click = (sel) => p.evaluate((s) => document.querySelector(s)?.click(), sel);
const txt = (sel) => p.evaluate((s) => [...document.querySelectorAll(s)].map((e) => e.textContent.replace(/\s+/g, " ").trim()), sel);
await p.goto(`http://127.0.0.1:${PORT}/`, { waitUntil: "load" });
await wait(900);

async function pickGodDest(dir) {
  fs.mkdirSync(dir, { recursive: true });
  await p.evaluate((x) => { window.__TEST.folder = x; }, dir);
  await click("[data-god-dest]");
  await p.waitForFunction((x) => localStorage.getItem("unpack.godDest") === x, { timeout: 5000 }, dir);
}
const settled = () => p.waitForFunction(() => document.querySelectorAll(".god-row").length && [...document.querySelectorAll(".god-row .qstatus")].every((e) => !/Reading/.test(e.textContent)), { timeout: 15000 }).catch(() => {});

// ---- GOD panel: disc 1 alone → missing warning, still convertible ----
{
  await drop([F + "/mike1/Mike (Disc 1).iso"]);
  await p.waitForSelector("[data-god]", { timeout: 5000 }).catch(() => {});
  await settled();
  await p.waitForSelector("[data-god-discs]", { timeout: 5000 }).catch(() => {});
  const line = (await txt("[data-god-discs]"))[0] || "";
  check("panel: status line per game", line === "Disc 1 of 2: this one · Disc 2 of 2: MISSING", line);
  const warn = (await txt("[data-god-missing]"))[0] || "";
  check("panel: amber missing-disc warning", /Disc 2 of 2 of this game is missing/.test(warn) && (await p.evaluate(() => document.querySelector("[data-god-missing]").classList.contains("warn-text"))), warn);
  const G = OUT + "/god";
  await pickGodDest(G);
  check("panel: a missing disc never blocks Convert", await p.evaluate(() => !document.querySelector("[data-god-go]")?.disabled));
  // adding disc 2 (from another folder) turns MISSING into queued
  await drop([F + "/mike2/Mike (Disc 2).iso"]);
  await settled();
  await p.waitForFunction(() => /Disc 2 of 2: queued/.test(document.querySelector("[data-god-discs]")?.textContent || ""), { timeout: 5000 }).catch(() => {});
  const lines = await txt("[data-god-discs]");
  check("panel: added disc shows as queued on the other row", lines[0] === "Disc 1 of 2: this one · Disc 2 of 2: queued" && lines[1] === "Disc 1 of 2: queued · Disc 2 of 2: this one", JSON.stringify(lines));
  check("panel: no warning once every disc is there", (await txt("[data-god-missing]")).length === 0);
  // convert only disc 1, then re-check disc 2 → already converted
  await p.evaluate(() => [...document.querySelectorAll(".god-row")].find((r) => /Disc 2/.test(r.querySelector(".name").textContent)).querySelector("[data-god-remove]").click());
  await wait(300);
  await click("[data-god-go]");
  await p.waitForFunction(() => !document.querySelector("[data-god-cancel-all]") && /Done/.test(document.querySelector(".god-row .qstatus")?.textContent || ""), { timeout: 60000 }).catch(() => {});
  await click("[data-god-close]");
  await wait(200);
  await drop([F + "/mike2/Mike (Disc 2).iso"]);
  await settled();
  await p.waitForSelector("[data-god-discs]", { timeout: 5000 }).catch(() => {});
  const l2 = (await txt("[data-god-discs]"))[0] || "";
  check("panel: disc 2 alone with disc 1 already converted", l2 === "Disc 1 of 2: already converted · Disc 2 of 2: this one", l2);
  check("panel: no missing warning when disc 1 was converted", (await txt("[data-god-missing]")).length === 0);
  await click("[data-god-close]");
  await wait(200);
}

// ---- GOD panel: similar names of different games never group ----
{
  await drop([F + "/nov/November (Disc 2).iso"]);
  await settled();
  await p.waitForSelector("[data-god-discs]", { timeout: 5000 }).catch(() => {});
  const l = (await txt("[data-god-discs]"))[0] || "";
  check("panel: a similar-named other game is not disc 1", l === "Disc 1 of 2: MISSING · Disc 2 of 2: this one", l);
  check("panel: and it is not offered", !(await p.$("[data-god-offer]")));
  await click("[data-god-close]");
  await wait(200);
}

// ---- Auto batch: disc 1 first (header-only numbers), and a missing disc warning in the summary ----
{
  const A = OUT + "/auto";
  fs.mkdirSync(A, { recursive: true });
  await p.evaluate((o) => { localStorage.setItem("unpack.autoIso", "god"); localStorage.setItem("unpack.destMode", "folder"); localStorage.setItem("unpack.destPath", o); }, A);
  await p.reload({ waitUntil: "load" });
  await wait(900);
  await drop([F + "/kilo.zip", F + "/lima.zip"]);
  await p.waitForFunction(() => { const s = [...document.querySelectorAll(".qrow.sub .qstatus")]; return s.length === 3 && s.every((e) => /done|Skipped|failed/i.test(e.textContent)); }, { timeout: 90000 }).catch(() => {});
  const subs = await p.evaluate(() => [...document.querySelectorAll(".qrow.sub")].map((r) => r.querySelector(".name").textContent + " :: " + r.querySelector(".qstatus").textContent));
  const kilo = subs.filter((s) => s.startsWith("Kilo"));
  check("auto: disc 1 (by header) converts before disc 2", kilo[0]?.startsWith("Kilo B.iso") && /Disc 1 of 2/.test(kilo[0]) && /merged/.test(kilo[1] || ""), JSON.stringify(subs));
  const gods = fs.readdirSync(A).filter((n) => /\(GOD\)$/.test(n));
  check("auto: the Kilo discs share one GOD folder", gods.filter((n) => /kilo/i.test(n)).length === 1, JSON.stringify(gods));
  const jobWarn = await txt(".error-box");
  check("auto: the job warns that Lima disc 1 is missing", jobWarn.some((w) => /Lima \(Disc 2\)\.iso: Disc 1 of 2 of this game is missing/.test(w)), JSON.stringify(jobWarn));
  await p.waitForSelector("[data-sum-discs]", { timeout: 8000 }).catch(() => {});
  const sum = (await txt("[data-sum-discs]"))[0] || "";
  check("auto: batch summary warns about the missing disc", /Lima \(Disc 2\)\.iso: Disc 1 of 2: MISSING/.test(sum) && !/Kilo/.test(sum), sum);
  await p.evaluate(() => localStorage.setItem("unpack.autoIso", "off"));
}

check("no page errors", !errors.length, errors.join("; "));
await b.close();
fs.rmSync(OUT, { recursive: true, force: true });
process.exit(fails ? 1 : 0);
