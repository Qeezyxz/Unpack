// Console Content profile folder as output: GOD games go in only as <TitleID>\00007000\…, never in
// "<Game> (GOD)" or archive-name folders (auto batch ISO → GOD, the GOD panel, GOD archives), and
// a non-Content output folder is unchanged. Headless Chrome + tests/test-shim.js + the real Rust
// code through examples/bridge.rs.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import { execFileSync } from "child_process";

const OUT = "/tmp/unpack-content";
const PORT = process.env.UNPACK_BRIDGE_PORT || 18790;
const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za", "/usr/bin/7z"].find((x) => fs.existsSync(x));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
fs.rmSync(OUT, { recursive: true, force: true });
const F = OUT + "/fx";
const mkIso = (dir, name, tid, media, disc, discs) => {
  fs.mkdirSync(dir, { recursive: true });
  execFileSync("python3", [new URL("./make_iso.py", import.meta.url).pathname, dir + "/" + name, tid, String(128 * 1024), media, String(disc), String(discs)]);
};
const zip = (cwd, out, ...items) => execFileSync(SZ, ["a", "-tzip", "-bso0", "-bsp0", out, ...items], { cwd });
const put = (f, body = "x") => { fs.mkdirSync(f.replace(/\/[^/]*$/, ""), { recursive: true }); fs.writeFileSync(f, body); };
// ISO zips: disc 1 and disc 2 of one game in separate archives
mkIso(F + "/d1", "Oscar (Disc 1).iso", "0BADC0E1", "FFFF0001", 1, 2);
mkIso(F + "/d2", "Oscar (Disc 2).iso", "0BADC0E1", "FFFF0002", 2, 2);
zip(F + "/d1", F + "/oscar1.zip", "Oscar (Disc 1).iso");
zip(F + "/d2", F + "/oscar2.zip", "Oscar (Disc 2).iso");
// GOD archive with a wrapper folder, one with a bare TitleID at the root, and a non-GOD archive
put(F + "/w/Bo3game (GOD)/415608C3/00007000/AAAA0001", "hdr1");
put(F + "/w/Bo3game (GOD)/415608C3/00007000/AAAA0001.data/Data0000", "d1");
zip(F + "/w", F + "/bo3.zip", "Bo3game (GOD)");
put(F + "/b/415608C3/00007000/AAAA0002", "hdr2");
put(F + "/b/415608C3/00007000/AAAA0002.data/Data0000", "d2");
zip(F + "/b", F + "/bo3disc2.zip", "415608C3");
put(F + "/n/notes.txt", "plain");
zip(F + "/n", F + "/notes.zip", "notes.txt");

const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"], protocolTimeout: 60000 });
const p = await b.newPage();
await p.setViewport({ width: 1180, height: 820 });
const errors = [];
p.on("pageerror", (e) => errors.push(e.message));
await p.evaluateOnNewDocument(fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8"));
await p.evaluateOnNewDocument(() => { if (!sessionStorage.getItem("ct")) { localStorage.clear(); sessionStorage.setItem("ct", "1"); } });
let fails = 0;
const check = (name, ok, info = "") => { if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + name + (info ? " — " + info : "")); };
const drop = (paths) => p.evaluate((x) => window.__TEST.drop(x), paths);
const click = (sel) => p.evaluate((s) => document.querySelector(s)?.click(), sel);
const ls = (d) => (fs.existsSync(d) ? fs.readdirSync(d).filter((n) => !n.startsWith(".")).sort() : []);
const subsDone = (n) => p.waitForFunction((k) => { const s = [...document.querySelectorAll(".qrow.sub .qstatus")]; return s.length >= k && s.every((e) => /done|Skipped|failed/i.test(e.textContent)); }, { timeout: 90000 }, n).catch(() => {});
const jobsDone = () => p.waitForFunction(() => { const r = [...document.querySelectorAll(".qrow:not(.sub) .qstatus")]; return r.length && r.every((e) => !/Waiting|Opening|Extracting|%/.test(e.textContent)); }, { timeout: 60000 }).catch(() => {});
await p.goto(`http://127.0.0.1:${PORT}/`, { waitUntil: "load" });
await wait(900);
const useDest = async (dir, auto) => {
  await p.evaluate((o, a) => { localStorage.setItem("unpack.autoIso", a); localStorage.setItem("unpack.destMode", "folder"); localStorage.setItem("unpack.destPath", o); }, dir, auto);
  await p.reload({ waitUntil: "load" });
  await wait(900);
};

// ---- Auto batch ISO → GOD into Content\0000000000000000: TitleID at the top, disc 2 merges ----
const P = OUT + "/usb/Content/0000000000000000";
{
  fs.mkdirSync(P, { recursive: true });
  await useDest(P, "god");
  await drop([F + "/oscar1.zip"]);
  await subsDone(1);
  await wait(400);
  await drop([F + "/oscar2.zip"]);
  await subsDone(2);
  await wait(400);
  const top = ls(P);
  check("auto: TitleID folder at the top level", top.includes("0BADC0E1"), JSON.stringify(top));
  check("auto: no '(GOD)' wrapper folders", !top.some((n) => /\(GOD\)/.test(n)), JSON.stringify(top));
  const pk = ls(P + "/0BADC0E1/00007000");
  check("auto: disc 2 merged into the existing TitleID folder", pk.includes("FFFF0001") && pk.includes("FFFF0002"), JSON.stringify(pk));
  // Wait until the rows settle on the final text (merged + warning) instead of one snapshot.
  const readSubs = () => p.evaluate(() => [...document.querySelectorAll(".qrow.sub .qstatus")].map((e) => e.textContent));
  let subs = await readSubs();
  for (let i = 0; i < 50 && !(subs.some((s) => /merged/.test(s)) && subs.some((s) => /0BADC0E1 folder/.test(s))); i++) {
    await wait(100);
    subs = await readSubs();
  }
  check("auto: disc 2 reports merged", subs.some((s) => /merged/.test(s)), JSON.stringify(subs));
  // the same disc again: skipped, never a second copy
  await drop([F + "/oscar1.zip"]);
  await subsDone(3);
  await wait(400);
  const subs2 = await p.evaluate(() => [...document.querySelectorAll(".qrow.sub .qstatus")].map((e) => e.textContent));
  check("auto: same MediaID is skipped with a clear message", subs2.some((s) => /Skipped: this disc \(MediaID FFFF0001\) is already in/.test(s)), JSON.stringify(subs2));
  check("auto: still one package per disc", ls(P + "/0BADC0E1/00007000").length === 4 && !ls(P).some((n) => /\(GOD\)/.test(n)));
  check("auto: nothing but TitleID folders inside Content", ls(P).every((n) => /^[0-9A-F]{8}$/.test(n)) && ls(OUT + "/usb/Content").join() === "0000000000000000", JSON.stringify([ls(P), ls(OUT + "/usb/Content")]));
  check("auto: ISO extraction folders sit next to Content", ls(OUT + "/usb").some((n) => /oscar|Oscar/.test(n)), JSON.stringify(ls(OUT + "/usb")));
  {
    // R25 L3: a progress repaint must not wipe the warning off the GOD sub-rows.
    await p.evaluate(() => window.__unpackRepaint());
    const again = await readSubs();
    check("auto: progress repaint keeps the warning", again.filter((s) => /0BADC0E1 folder/.test(s)).length === subs.filter((s) => /0BADC0E1 folder/.test(s)).length, JSON.stringify(again));
  }
  check("auto: unknown-title warning names the TitleID folder", subs.some((s) => /0BADC0E1 folder/.test(s)) && !subs.some((s) => /named after the file/.test(s)), JSON.stringify(subs));
}

// ---- same, with delete-ISO on: the emptied extraction folder is removed too ----
{
  const R = OUT + "/usb3/Content/0000000000000000";
  fs.mkdirSync(R, { recursive: true });
  await p.evaluate(() => localStorage.setItem("unpack.delIso", "1"));
  await useDest(R, "god");
  await drop([F + "/oscar1.zip"]);
  await subsDone(1);
  await wait(600);
  check("auto+delete: GOD in Content", ls(R + "/0BADC0E1/00007000").includes("FFFF0001"), JSON.stringify(ls(R)));
  check("auto+delete: no extraction folder left anywhere", ls(OUT + "/usb3").join() === "Content" && ls(R).join() === "0BADC0E1", JSON.stringify([ls(OUT + "/usb3"), ls(R)]));
  await p.evaluate(() => localStorage.setItem("unpack.delIso", "0"));
}

// ---- GOD archives into a lowercase content profile: just the TitleID tree ----
const Q = OUT + "/usb2/content/0000000000000000";
{
  fs.mkdirSync(Q, { recursive: true });
  await useDest(Q, "off");
  await drop([F + "/bo3.zip"]);
  await jobsDone();
  await wait(800);
  check("archive: wrapper 'Bo3game (GOD)' dropped, TitleID at the top", JSON.stringify(ls(Q)) === '["415608C3"]', JSON.stringify(ls(Q)));
  check("archive: package placed", fs.existsSync(Q + "/415608C3/00007000/AAAA0001.data/Data0000"));
  await drop([F + "/bo3disc2.zip"]);
  await jobsDone();
  await wait(800);
  check("archive: bare TitleID zip merges into it", JSON.stringify(ls(Q)) === '["415608C3"]' && ls(Q + "/415608C3/00007000").length === 4, JSON.stringify(ls(Q + "/415608C3/00007000")));
  await drop([F + "/bo3.zip"]);
  await jobsDone();
  await wait(800);
  const t = await p.evaluate(() => document.body.innerText);
  check("archive: existing MediaID skipped with a message", /Skipped: 415608C3\\00007000\\AAAA0001 \(same MediaID\)/.test(t) && fs.readFileSync(Q + "/415608C3/00007000/AAAA0001", "utf8") === "hdr1");
  check("archive: no numbered copies", JSON.stringify(ls(Q)) === '["415608C3"]', JSON.stringify(ls(Q)));
  await drop([F + "/notes.zip"]);
  await jobsDone();
  await wait(800);
  const t2 = await p.evaluate(() => document.body.innerText);
  check("archive: non-GOD content extracted as before", fs.existsSync(Q + "/notes/notes.txt"));
  check("archive: amber not-a-GOD-game hint", /doesn't look like a GOD game/.test(t2));
}

// ---- GOD panel (manual) into a Content profile, and an ordinary folder unchanged ----
{
  const R = OUT + "/usb3/Content/0000000000000000";
  fs.mkdirSync(R, { recursive: true });
  for (const [dest, tag] of [[R, "content"], [OUT + "/plain", "plain"]]) {
    fs.mkdirSync(dest, { recursive: true });
    await drop([F + "/d1/Oscar (Disc 1).iso"]);
    await p.waitForSelector("[data-god]", { timeout: 5000 }).catch(() => {});
    await wait(800);
    await p.evaluate((x) => { window.__TEST.folder = x; }, dest);
    await click("[data-god-dest]");
    await p.waitForFunction((x) => localStorage.getItem("unpack.godDest") === x, { timeout: 5000 }, dest).catch(() => {});
    await click("[data-god-go]");
    await p.waitForFunction(() => !document.querySelector("[data-god-cancel-all]") && /Done|Skipped/.test(document.querySelector(".god-row .qstatus")?.textContent || ""), { timeout: 60000 }).catch(() => {});
    check(`panel (${tag}): TitleID at the top level`, JSON.stringify(ls(dest)) === '["0BADC0E1"]', JSON.stringify(ls(dest)));
    if (tag === "content") {
      await click("[data-god-close]");
      await wait(200);
      await drop([F + "/d1/Oscar (Disc 1).iso"]);
      await wait(800);
      await click("[data-god-go]");
      await p.waitForFunction(() => /Skipped/.test(document.querySelector(".god-row .qstatus")?.textContent || ""), { timeout: 30000 }).catch(() => {});
      const st = await p.evaluate(() => document.querySelector(".god-row .qstatus")?.textContent || "");
      check("panel: same disc again is skipped, not failed", /Skipped: this disc \(MediaID FFFF0001\)/.test(st), st);
      check("panel: no error banner for a skip", !(await p.evaluate(() => /Failed/.test(document.querySelector(".god-row")?.textContent || ""))));
    }
    await click("[data-god-close]");
    await wait(200);
  }
  // ordinary output folder, auto batch: "<Game> (GOD)" stays
  const A = OUT + "/auto-plain";
  fs.mkdirSync(A, { recursive: true });
  await useDest(A, "god");
  await drop([F + "/oscar1.zip"]);
  await subsDone(1);
  await wait(400);
  check("auto (ordinary folder): '<Game> (GOD)' folder unchanged", ls(A).some((n) => /\(GOD\)$/.test(n)), JSON.stringify(ls(A)));
  await p.evaluate(() => localStorage.setItem("unpack.autoIso", "off"));
}

check("no page errors", !errors.length, errors.join("; "));
await b.close();
fs.rmSync(OUT, { recursive: true, force: true });
process.exit(fails ? 1 : 0);
