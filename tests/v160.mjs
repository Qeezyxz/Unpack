// UI tests for the features ported in v1.6.0: Discord presence (and its off toggle), jobs at once,
// nested archives, the red error banner, unknown-TitleID warning, multi-disc merge + companion prompt.
// Headless Chrome + tests/test-shim.js + the real Rust code through examples/bridge.rs.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";
const TMP = os.tmpdir();
import { execFileSync } from "child_process";

const D = process.env.TESTDATA || "/workspace/testdata";
const OUT = path.join(TMP, "unpack-v160");
const PORT = process.env.UNPACK_BRIDGE_PORT || 18790;
const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za", "/usr/bin/7z"].find((x) => fs.existsSync(x));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });
if (!fs.existsSync(D + "/big.7z")) {
  execFileSync("sh", ["-c", `head -c 314572800 /dev/urandom > "${OUT}/big.bin" && "${SZ}" a -t7z -mx=0 -bso0 "${D}/big.7z" "${OUT}/big.bin" && rm "${OUT}/big.bin"`]);
}
if (!fs.existsSync(D + "/unknown.iso")) execFileSync("python3", [new URL("./make_iso.py", import.meta.url).pathname, D + "/unknown.iso", "0ABCDEF1", "65536"]);

// fixtures: zip in a zip, a damaged zip, and two discs of one game
const F = OUT + "/fx";
fs.mkdirSync(F + "/inner", { recursive: true });
fs.writeFileSync(F + "/inner/note.txt", "inner note\n");
execFileSync(SZ, ["a", "-tzip", "-bso0", "-bsp0", F + "/inner.zip", "note.txt"], { cwd: F + "/inner" });
fs.mkdirSync(F + "/outer", { recursive: true });
fs.copyFileSync(F + "/inner.zip", F + "/outer/inner.zip");
fs.writeFileSync(F + "/outer/top.txt", "top\n");
execFileSync(SZ, ["a", "-tzip", "-bso0", "-bsp0", F + "/outer.zip", "inner.zip", "top.txt"], { cwd: F + "/outer" });
const bad = Buffer.concat([Buffer.from("PK\x03\x04"), Buffer.alloc(300, 7)]);
fs.writeFileSync(F + "/broken.zip", bad);
const DISCS = OUT + "/discs";
fs.mkdirSync(DISCS, { recursive: true });
const mk = (name, tid, media, disc) => execFileSync("python3", [new URL("./make_iso.py", import.meta.url).pathname, DISCS + "/" + name, tid, String(256 * 1024), media, String(disc), "2"]);
mk("Test Game (Disc 1).iso", "4D5307E6", "AAAA0001", 1);
mk("Test Game (Disc 2).iso", "4D5307E6", "AAAA0002", 2);

const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"], protocolTimeout: 60000 });
const p = await b.newPage();
await p.setViewport({ width: 1180, height: 820 });
const errors = [];
p.on("pageerror", (e) => errors.push(e.message));
p.on("dialog", (d) => { errors.push("DIALOG " + d.message()); d.dismiss(); });
await p.evaluateOnNewDocument(shim);
await p.evaluateOnNewDocument(() => { if (!sessionStorage.getItem("v160")) { localStorage.clear(); sessionStorage.setItem("v160", "1"); } });

let fails = 0;
const check = (name, ok, info = "") => { if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + name + (info ? " — " + info : "")); };
const discordCalls = () => fetch(`http://127.0.0.1:${PORT}/invoke/__discord_calls`, { method: "POST", body: "{}" }).then((r) => r.json());
const drop = (paths) => p.evaluate((x) => window.__TEST.drop(x), paths);
const click = (sel) => p.evaluate((s) => document.querySelector(s)?.click(), sel);
const ls = (k) => p.evaluate((x) => localStorage.getItem(x), k);

const calls0 = (await discordCalls()).length;
await p.goto(`http://127.0.0.1:${PORT}/`, { waitUntil: "load" });
await wait(900);

// ---- Discord ----
{
  const calls = (await discordCalls()).slice(calls0);
  check("discord: off by default, nothing is sent at startup (R15 L1)", !calls.some((c) => c.call === "activity" || (c.call === "enabled" && c.enabled === true)), JSON.stringify(calls));
  await click("[data-action=settings]");
  await wait(400);
  check("discord: toggle is in Settings and off by default", await p.evaluate(() => document.querySelector("[data-set-discord]")?.checked === false));
  check("discord: no activity while off", !(await discordCalls()).slice(calls0).some((c) => c.call === "activity"));
  await click("[data-set-discord]");
  await wait(300);
  const on1 = (await discordCalls()).slice(calls0);
  check("discord: turning it on enables and publishes", on1.some((c) => c.call === "enabled" && c.enabled === true) && on1.at(-1)?.call === "activity", JSON.stringify(on1));
  check("discord: on is remembered", (await ls("unpack.discord")) === "1");
  await click("[data-settings-close]");
  await wait(300);
  await click("[data-action=settings]");
  await wait(300);
  const after = (await discordCalls()).slice(calls0);
  const settingsCall = after.filter((c) => c.call === "activity").pop();
  check("discord: Settings view is reported with an idle state", settingsCall?.details === "Settings" && settingsCall?.state === "Idle", JSON.stringify(settingsCall));
  await click("[data-set-discord]");
  await wait(300);
  const off = (await discordCalls()).slice(calls0);
  check("discord: turning it off tells Rust to stop", off.at(-1)?.call === "enabled" && off.at(-1)?.enabled === false, JSON.stringify(off.at(-1)));
  check("discord: off is remembered", (await ls("unpack.discord")) === "0");
  const n = off.length;
  await click("[data-settings-close]");
  await wait(300);
  await click("[data-action=settings]");
  await wait(300);
  check("discord: no further activity is sent while off", (await discordCalls()).slice(calls0).length === n);
  await click("[data-set-discord]");
  await wait(300);
  const on = (await discordCalls()).slice(calls0);
  check("discord: turning it back on re-enables and republishes", on.some((c, i) => i >= n && c.call === "enabled" && c.enabled === true) && on.at(-1)?.call === "activity", JSON.stringify(on.slice(n)));
  await click("[data-settings-close]");
}

// ---- Settings defaults ----
{
  await click("[data-action=settings]");
  await wait(300);
  const s = await p.evaluate(() => ({ jobs: document.querySelector("[data-set-maxjobs]")?.value, nested: document.querySelector("[data-set-nested]")?.checked }));
  check("settings: jobs at once defaults to 2", s.jobs === "2", s.jobs);
  check("settings: open zips inside zips defaults to on", s.nested === true);
  await p.select("[data-set-maxjobs]", "3");
  await click("[data-set-nested]");
  check("settings: jobs at once and nested are remembered", (await ls("unpack.maxJobs")) === "3" && (await ls("unpack.nested")) === "0");
  await p.select("[data-set-maxjobs]", "4");
  check("settings: jobs at once is capped at 4", (await ls("unpack.maxJobs")) === "4");
  await p.evaluate(() => { localStorage.setItem("unpack.maxJobs", "99"); });
  await click("[data-settings-close]");
  await click("[data-action=settings]");
  await wait(300);
  check("settings: a bad stored value falls back to 2", await p.evaluate(() => document.querySelector("[data-set-maxjobs]")?.value === "2"));
  await p.select("[data-set-maxjobs]", "2");
  await click("[data-set-nested]");
  await click("[data-settings-close]");
}

// ---- Jobs at once ----
async function runQueue(limit, count) {
  const Q = OUT + "/queue" + limit;
  fs.mkdirSync(Q + "/src", { recursive: true });
  fs.mkdirSync(Q + "/out", { recursive: true });
  const names = [];
  for (let i = 1; i <= count; i++) {
    fs.linkSync(D + "/big.7z", Q + `/src/q${i}.7z`);
    names.push(Q + `/src/q${i}.7z`);
  }
  await p.evaluate((o) => { localStorage.setItem("unpack.maxJobs", String(o.limit)); localStorage.setItem("unpack.destMode", "folder"); localStorage.setItem("unpack.destPath", o.out); }, { limit, out: Q + "/out" });
  await click("[data-action=clear-all]");
  await p.evaluate(() => { const cb = document.querySelector("[data-setting=autoWrite]"); if (cb && !cb.checked) cb.click(); });
  await drop(names);
  let max = 0;
  for (let i = 0; i < 1200; i++) {
    const st = await p.evaluate(() => [...document.querySelectorAll(".qrow")].map((r) => r.querySelector(".qstatus").textContent));
    max = Math.max(max, st.filter((s) => /Extracting/.test(s)).length);
    if (st.length === count && st.every((s) => /Saved|Failed/.test(s))) break;
    await wait(20);
  }
  const outs = fs.readdirSync(Q + "/out").length;
  return { max, outs };
}
{
  const one = await runQueue(1, 3);
  check("jobs at once = 1: never more than one extraction runs", one.max === 1 && one.outs === 3, JSON.stringify(one));
  const three = await runQueue(3, 3);
  check("jobs at once = 3: runs more than one side by side", three.max >= 2 && three.max <= 3 && three.outs === 3, JSON.stringify(three));
  await p.evaluate(() => localStorage.setItem("unpack.maxJobs", "2"));
}

// ---- Nested archives + error banner ----
async function extractOne(zip, out) {
  fs.mkdirSync(out, { recursive: true });
  await p.evaluate((o) => { localStorage.setItem("unpack.destMode", "folder"); localStorage.setItem("unpack.destPath", o); }, out);
  await click("[data-action=clear-all]");
  await drop([zip]);
  await p.waitForFunction(() => { const s = [...document.querySelectorAll(".qrow .qstatus")].map((e) => e.textContent); return s.length && s.every((x) => /Saved|Failed|Skipped/.test(x)); }, { timeout: 30000 }).catch(() => {});
  return p.evaluate(() => ({ status: document.querySelector(".qrow .qstatus")?.textContent || "", all: document.body.innerText }));
}
const find = (dir, name) => { for (const e of fs.readdirSync(dir, { withFileTypes: true })) { const f = dir + "/" + e.name; if (e.isDirectory()) { const r = find(f, name); if (r) return r; } else if (e.name === name) return f; } return null; };
{
  const on = await extractOne(F + "/outer.zip", OUT + "/nest-on");
  check("nested on: the zip inside is opened", !!find(OUT + "/nest-on", "note.txt"), on.status);
  check("nested on: status says how many were opened", /opened 1 archive inside/.test(on.status), on.status);
  await p.evaluate(() => localStorage.setItem("unpack.nested", "0"));
  const off = await extractOne(F + "/outer.zip", OUT + "/nest-off");
  check("nested off: the inner zip stays closed", !find(OUT + "/nest-off", "note.txt") && !!find(OUT + "/nest-off", "inner.zip"));
  check("nested off: the page says how to open it", /left closed[\s\S]*Open zips inside zips/.test(off.all));
  await p.evaluate(() => localStorage.removeItem("unpack.nested"));
}
{
  await extractOne(F + "/broken.zip", OUT + "/broken");
  const vis = await p.evaluate(() => { const e = document.getElementById("job-error"); return !!e && !e.classList.contains("hidden") && getComputedStyle(e).display !== "none" ? e.querySelector("[data-error-text]").textContent : ""; });
  check("error banner: a failed extract shows the red banner", /broken\.zip/.test(vis), vis);
  await click("[data-action=settings]");
  await wait(400);
  const above = await p.evaluate(() => {
    const e = document.getElementById("job-error");
    const r = e.getBoundingClientRect();
    const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    return { onTop: !!hit && e.contains(hit), dialog: !!document.querySelector("[data-settings]"), color: getComputedStyle(e).borderLeftColor };
  });
  check("error banner: stays above an open dialog", above.dialog && above.onTop, JSON.stringify(above));
  await click("[data-settings-close]");
  await click("[data-error-dismiss]");
  check("error banner: Dismiss hides it", await p.evaluate(() => document.getElementById("job-error").classList.contains("hidden")));
  await extractOne(F + "/broken.zip", OUT + "/broken2");
  await extractOne(F + "/outer.zip", OUT + "/nest-again");
  check("error banner: the next job clears an old error", await p.evaluate(() => document.getElementById("job-error").classList.contains("hidden")));
}

// ---- Discord presence while a job runs is coarse (no names) ----
{
  const before = (await discordCalls()).length;
  await extractOne(F + "/outer.zip", OUT + "/presence");
  const mine = (await discordCalls()).slice(before).filter((c) => c.call === "activity");
  check("discord: activity text never contains file names", mine.every((c) => !/outer|inner|note|presence/i.test(c.details + c.state)), JSON.stringify(mine.map((c) => c.details + "/" + c.state)));
}

// The output folder goes through the fake native picker so the bridge grants it, like the real one.
async function pickGodDest(dir) {
  await p.evaluate((x) => { window.__TEST.folder = x; }, dir);
  await click("[data-god-dest]");
  await p.waitForFunction((x) => localStorage.getItem("unpack.godDest") === x, { timeout: 5000 }, dir);
}

// ---- ISO: unknown TitleID is a warning, multi-disc merge, companion prompt ----
{
  const G = OUT + "/god";
  fs.mkdirSync(G, { recursive: true });
  await drop([D + "/unknown.iso"]);
  await p.waitForSelector("[data-god]", { timeout: 5000 }).catch(() => {});
  await pickGodDest(G);
  await p.waitForFunction(() => [...document.querySelectorAll(".god-row .qstatus")].every((e) => !/Reading/.test(e.textContent)), { timeout: 15000 }).catch(() => {});
  const w = await p.evaluate(() => document.querySelector("[data-god-warning]")?.textContent || "");
  check("unknown TitleID: a warning row names the ID", /0ABCDEF1/.test(w) && /not in Unpack's game list/.test(w), w);
  check("unknown TitleID: Convert is not blocked", await p.evaluate(() => !document.querySelector("[data-god-go]")?.disabled));
  await click("[data-god-go]");
  await p.waitForFunction(() => !document.querySelector("[data-god-cancel-all]") && /Done/.test(document.querySelector(".god-row .qstatus")?.textContent || ""), { timeout: 60000 }).catch(() => {});
  check("unknown TitleID: it converted anyway", fs.existsSync(G + "/0ABCDEF1"));
  check("unknown TitleID: no red banner for a warning", await p.evaluate(() => document.getElementById("job-error").classList.contains("hidden")));
  await click("[data-god-close]");

  const G2 = OUT + "/god2";
  fs.mkdirSync(G2, { recursive: true });
  // companion prompt: only disc 2 is dropped; disc 1 sits next to it
  await drop([DISCS + "/Test Game (Disc 2).iso"]);
  await p.waitForSelector("[data-god-offer]", { timeout: 10000 }).catch(() => {});
  const offer = await p.evaluate(() => document.querySelector("[data-god-offer]")?.textContent.replace(/\s+/g, " ") || "");
  check("multi-disc: other discs next to the ISO are offered", /Disc 1\)\.iso/.test(offer), offer);
  check("multi-disc: nothing is added without a click", (await p.evaluate(() => document.querySelectorAll(".god-row").length)) === 1);
  await pickGodDest(G2);
  await click("[data-god-offer-yes]");
  await p.waitForFunction(() => document.querySelectorAll(".god-row").length === 2 && [...document.querySelectorAll(".god-row .qstatus")].every((e) => !/Reading/.test(e.textContent)), { timeout: 15000 }).catch(() => {});
  const rows = await p.evaluate(() => [...document.querySelectorAll(".god-row")].map((r) => r.querySelector(".qstatus").textContent));
  check("multi-disc: rows show the disc number", rows.some((r) => /Disc 1/.test(r)) && rows.some((r) => /Disc 2/.test(r)), JSON.stringify(rows));
  await click("[data-god-go]");
  await p.waitForFunction(() => !document.querySelector("[data-god-cancel-all]") && [...document.querySelectorAll(".god-row .qstatus")].every((e) => /Done/.test(e.textContent)), { timeout: 60000 }).catch(() => {});
  const done = await p.evaluate(() => [...document.querySelectorAll(".god-row")].map((r) => r.querySelector(".name").textContent + " :: " + r.querySelector(".qstatus").textContent));
  const tids = fs.readdirSync(G2);
  const media = fs.existsSync(G2 + "/4D5307E6/00007000") ? fs.readdirSync(G2 + "/4D5307E6/00007000").filter((n) => !n.endsWith(".data")) : [];
  check("multi-disc: both discs land in one TitleID folder", tids.length === 1 && tids[0] === "4D5307E6" && media.length === 2, JSON.stringify({ tids, media }));
  check("multi-disc: the second convert says it was merged", done.some((r) => /merged into the same game folder/.test(r)), JSON.stringify(done));
  check("multi-disc: discs are converted lowest disc first", done[0].startsWith("Test Game (Disc 1)"), done[0]);
  await click("[data-god-close]");
}

// ---- Auto-convert: two discs in one archive end up in one GOD folder ----
{
  execFileSync(SZ, ["a", "-tzip", "-bso0", "-bsp0", F + "/twodiscs.zip", "Test Game (Disc 1).iso", "Test Game (Disc 2).iso"], { cwd: DISCS });
  const A = OUT + "/auto";
  fs.mkdirSync(A, { recursive: true });
  await p.evaluate((o) => { localStorage.setItem("unpack.autoIso", "god"); localStorage.setItem("unpack.destMode", "folder"); localStorage.setItem("unpack.destPath", o); }, A);
  await click("[data-action=clear-all]");
  await drop([F + "/twodiscs.zip"]);
  await p.waitForFunction(() => { const s = [...document.querySelectorAll(".qrow.sub .qstatus")]; return s.length === 2 && s.every((e) => /done|Skipped|failed/i.test(e.textContent)); }, { timeout: 60000 }).catch(() => {});
  const subs = await p.evaluate(() => [...document.querySelectorAll(".qrow.sub .qstatus")].map((e) => e.textContent));
  const gods = fs.readdirSync(A).filter((n) => /\(GOD\)$/.test(n));
  const media = gods.length === 1 ? fs.readdirSync(`${A}/${gods[0]}/4D5307E6/00007000`).filter((n) => !n.endsWith(".data")) : [];
  check("auto-convert: both discs of one archive share one GOD folder", gods.length === 1 && media.length === 2, JSON.stringify({ gods, media }));
  check("auto-convert: the second disc is reported as merged", subs.some((x) => /merged into the same game folder/.test(x)) && subs.every((x) => /Disc \d of 2/.test(x)), JSON.stringify(subs));
  await p.evaluate(() => localStorage.setItem("unpack.autoIso", "off"));
}

// ---- One-time migration notice (R19 L5) ----
{
  const planted = await fetch(`http://127.0.0.1:${PORT}/invoke/__plant_migration_notice`, { method: "POST", body: "{}" }).then((r) => r.ok);
  await p.reload({ waitUntil: "load" });
  await p.waitForSelector("#migration-notice", { timeout: 5000 }).catch(() => {});
  const txt = await p.evaluate(() => document.querySelector("#migration-notice")?.textContent || "");
  check("migration: one-time notice says where the older settings are", planted && /older settings/.test(txt) && txt.includes("EBWebView.old-version"), txt);
  await click("[data-migration-ok]");
  check("migration: notice dismisses", await p.evaluate(() => !document.querySelector("#migration-notice")));
  await p.reload({ waitUntil: "load" });
  await new Promise((r) => setTimeout(r, 1500));
  check("migration: notice is shown only once", await p.evaluate(() => !document.querySelector("#migration-notice")));
}

// ---- Wrap + no page errors ----
{
  await drop([D + "/unknown.iso"]);
  await p.waitForSelector("[data-god]", { timeout: 5000 }).catch(() => {});
  await click("[data-god-close]");
  const ws = await p.evaluate(() => { const probe = document.createElement("div"); probe.className = "qrow sub"; probe.innerHTML = '<span class="qstatus">x</span>'; document.body.appendChild(probe); const v = getComputedStyle(probe.firstChild).whiteSpace; probe.remove(); return v; });
  check("long status messages wrap in sub-rows", ws === "normal", ws);
  check("no page errors", errors.length === 0, errors.join(" | "));
}

await b.close();
console.log(fails ? `${fails} FAILED` : "all passed");
process.exit(fails ? 1 : 0);
