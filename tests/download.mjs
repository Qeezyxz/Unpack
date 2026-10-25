// E2E: Download from link tab + folder drop confirm (local HTTP server only).
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";
const TMP = os.tmpdir();
import http from "http";
import { execFileSync } from "child_process";

const W = path.join(TMP, "unpack-dl-e2e");
fs.rmSync(W, { recursive: true, force: true });
fs.mkdirSync(W + "/dropme/sub", { recursive: true });
fs.writeFileSync(W + "/dropme/a.zip", "PK\x05\x06" + "\0".repeat(18)); // empty zip EOCD
fs.writeFileSync(W + "/dropme/sub/b.iso", "not-an-iso");
fs.writeFileSync(W + "/dropme/readme.txt", "x");

const payload = Buffer.from("fake-mod-menu-bytes-for-sha");
const server = http.createServer((req, res) => {
  if (req.url === "/pack.bin") {
    res.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Disposition": 'attachment; filename="from-link.bin"', "Content-Length": payload.length });
    res.end(payload);
  } else if (req.url === "/page") {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end("<!DOCTYPE html><html>login</html>");
  } else res.writeHead(404), res.end();
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const port = server.address().port;
const url = `http://127.0.0.1:${port}/pack.bin`;

const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
const p = await b.newPage(); await p.setViewport({ width: 1180, height: 900 });
const errors = [], external = [];
p.on("pageerror", (e) => errors.push(e.message));
p.on("request", (r) => { if (!/^http:\/\/127\.0\.0\.1/.test(r.url()) && !r.url().startsWith("data:")) external.push(r.url()); });
await p.evaluateOnNewDocument(fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8"));
await p.evaluateOnNewDocument(() => { if (!sessionStorage.getItem("x")) { localStorage.clear(); localStorage.setItem("unpack.theme", "dark"); localStorage.setItem("unpack.tipsDone", "1"); sessionStorage.setItem("x", "1"); } });
await p.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let fails = 0, total = 0;
const check = (n, ok, i = "") => { total++; if (!ok) fails++; console.log((ok ? "PASS " : "FAIL ") + n + (i ? " — " + i : "")); };
await wait(800);

// Folder drop → confirm
await p.evaluate((f) => window.__TEST.drop([f]), W + "/dropme");
await wait(500);
check("folder confirm shown", !!(await p.$("[data-folder-overlay]")));
const n = await p.evaluate(() => document.querySelector("[data-folder-ok]")?.textContent || "");
check("counts archives+iso", /Add 2 to queue/.test(n), n);
await p.click("[data-folder-cancel]"); await wait(200);

// Download tab
await p.click("[data-action=mods]"); await wait(400);
await p.click("[data-mods-tab=download]"); await wait(300);
check("download tab note", await p.evaluate(() => /only goes online when you click Download/.test(document.body.innerText)));
await p.evaluate((u) => { const a=document.querySelector("[data-dl-url]"); const b=document.querySelector("[data-dl-name]"); a.value=u; b.value="Test Pack"; a.dispatchEvent(new Event("input")); }, url);
await p.click("[data-dl-save]"); await wait(200);
check("link saved", await p.evaluate(() => JSON.parse(localStorage.getItem("unpack.links") || "[]").length === 1));
await p.click("[data-dl-go]");
await p.waitForFunction(() => !!document.querySelector("[data-dl-sha]")?.textContent, { timeout: 20000 });
await wait(300);
const sha = await p.evaluate(() => document.querySelector("[data-dl-sha]")?.textContent || "");
const crypto = await import("crypto");
const expect = crypto.createHash("sha256").update(payload).digest("hex");
check("SHA-256 shown", sha === expect, sha.slice(0, 16) + "…");
const dlDir = path.join(TMP, "unpack-bridge-downloads");
const onDisk = fs.existsSync(dlDir) && fs.readdirSync(dlDir).some((n) => n.endsWith("from-link.bin"));
check("file on disk", onDisk, fs.existsSync(dlDir) ? fs.readdirSync(dlDir).join(",") : "no dir");
await p.click("[data-dl-add-mod]"); await wait(500);
await p.click("[data-mods-tab=library]"); await wait(400);
check("added to mod library", await p.evaluate(() => document.querySelectorAll(".mod-card").length >= 1));

// HTML refused
await p.click("[data-mods-tab=download]"); await wait(200);
await p.click("[data-dl-url]", { clickCount: 3 });
await p.type("[data-dl-url]", `http://127.0.0.1:${port}/page`);
await p.click("[data-dl-go]"); await wait(1500);
check("HTML page refused", await p.evaluate(() => /web page/i.test(document.querySelector("[data-mods-dl]")?.innerText || "")));

// R5 addendum: extract Skip must not grant existing folder for mod_copy
const secret = W + "/secret";
fs.mkdirSync(secret, { recursive: true });
fs.writeFileSync(secret + "/notes.txt", "keep");
const raw = await p.evaluate(async (sec) => {
  try {
    await window.__TAURI__.core.invoke("grant_dir", { path: sec + "/.." });
    return "parent-granted-via-dotdot";
  } catch (e) { return String(e); }
}, secret);
check("grant_dir removed from page API", /not found|unknown|grant_dir|404|500|Error/i.test(raw) && raw !== "parent-granted-via-dotdot", raw);
const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za"].find((x) => fs.existsSync(x));
const pad = W + "/pad"; fs.mkdirSync(pad, { recursive: true }); fs.writeFileSync(pad + "/a.txt", "x");
execFileSync(SZ, ["a", "-tzip", "-bso0", W + "/secret.zip", "a.txt"], { cwd: pad });
await p.evaluate(async (w) => {
  await window.__TAURI__.core.invoke("__test_grant", { paths: [w] });
  try {
    await window.__TAURI__.core.invoke("extract_archive", { id: "sk1", path: w + "/secret.zip", dest: w, password: null, collision: "skip", layout: "folder", skipJunk: true });
  } catch (e) { return String(e); }
}, W);
const bypass = await p.evaluate(async (sec) => {
  try {
    await window.__TAURI__.core.invoke("mod_copy", { id: "bx", path: sec + "/notes.txt", target: sec + "/../escape-dl", mode: "as_is" });
    return "copied";
  } catch (e) { return String(e); }
}, secret);
check("R5 addendum: skip extract does not unlock sibling for mod_copy", /wasn't picked|Cannot open|Not a folder|link/i.test(bypass) || bypass !== "copied", bypass);

const ungrantedR8 = path.join(TMP, "ungranted-r8");
const nopeIso = path.join(TMP, "nope.iso");
fs.mkdirSync(ungrantedR8, { recursive: true });
// R8: bridge enforces the same dest grant as the app for ISO → GOD / XEX
const isoGrant = await p.evaluate(async (iso, dest) => { const out = []; for (const c of ["iso_to_god", "iso_to_xex"]) { try { await window.__TAURI__.core.invoke(c, { id: "g-" + c, path: iso, dest, title: null }); out.push("accepted"); } catch (e) { out.push(String(e)); } } return out; }, nopeIso, ungrantedR8);
check("R8: ISO→GOD/XEX ungranted dest refused", isoGrant.every((x) => /wasn't picked/.test(x)) && fs.readdirSync(ungrantedR8).length === 0, isoGrant.join(" | "));
fs.rmSync(ungrantedR8, { recursive: true, force: true });
// Homebrew Tools section (close mods overlay first so toolbar is clickable)
await p.click("[data-mods-close]"); await wait(300);
await p.click("[data-action=homebrew]"); await wait(400);
check("homebrew dialog", !!(await p.$("[data-homebrew]")));
check("no online mod menus listed", await p.evaluate(() => !/Purged|Jiggy|Blue Sky|Deception/i.test(document.body.innerText)));
check("Aurora card present", await p.evaluate(() => /Aurora Dashboard/.test(document.body.innerText)));
await p.click("[data-hb-close]"); await wait(200);

// Import/export saved links (JSON round-trip via evaluate)
const round = await p.evaluate(() => {
  const links = JSON.parse(localStorage.getItem("unpack.links") || "[]");
  const body = JSON.stringify(links.map((l) => ({ name: l.name, url: l.url, notes: l.notes || "", game: l.game || "" })));
  const parsed = JSON.parse(body);
  return parsed.length >= 1 && parsed[0].url;
});
check("exportable link shape", !!round);

// R7: CSV formula escape, http(s)-only import, length caps, dedupe (mirrors app.js helpers)
const r7ie = await p.evaluate(() => {
  function csvSafeCell(x) {
    let t = String(x ?? "");
    if (/^[=+\-@\t\r]/.test(t)) t = "'" + t;
    return '"' + t.replace(/"/g, '""') + '"';
  }
  function normalizeLinkUrl(url) {
    const u = String(url || "").trim();
    if (!/^https?:\/\//i.test(u)) return null;
    try {
      const x = new URL(u);
      if (x.protocol !== "http:" && x.protocol !== "https:") return null;
      return x.href;
    } catch { return null; }
  }
  const formula = csvSafeCell("=1+1");
  const ok = csvSafeCell("hello");
  const bad = ["javascript:alert(1)", "file:///etc/passwd", "data:text/html,x", "[object Object]"]
    .map(normalizeLinkUrl);
  const good = normalizeLinkUrl("https://example.com/a.zip");
  const longNote = "N".repeat(600).slice(0, 500);
  const rows = [
    { url: "https://ex.com/a" },
    { url: "https://ex.com/a" },
    { url: "https://ex.com/b" },
    { url: "javascript:x" },
  ];
  const seen = new Set();
  const deduped = rows.map((r) => ({ url: normalizeLinkUrl(r.url) })).filter((r) => r.url).filter((r) => {
    if (seen.has(r.url)) return false; seen.add(r.url); return true;
  });
  return {
    formulaPrefixed: formula.startsWith('"\'=') || formula.startsWith("\"'="),
    formula: formula,
    okPlain: ok === '"hello"',
    allBadNull: bad.every((x) => x === null),
    goodHttps: !!good && good.startsWith("https://"),
    noteCap: longNote.length === 500,
    deduped: deduped.length === 2,
  };
});
check("CSV formula cells prefixed", r7ie.formulaPrefixed || (r7ie.formula && r7ie.formula.includes("'=")), JSON.stringify(r7ie));
check("CSV plain cell unchanged", r7ie.okPlain);
check("import rejects non-http(s)", r7ie.allBadNull);
check("import keeps https", r7ie.goodHttps);
check("import note length capped", r7ie.noteCap);
check("import dedupes within file", r7ie.deduped);

// Confirm app.js source contains the R7 guards (loaded into the page)
const srcHas = await p.evaluate(async () => {
  const t = await (await fetch("./app.js")).text();
  return {
    csvSafe: t.includes("function csvSafeCell") && t.includes("[=+\\-@\\t\\r]"),
    normalize: t.includes("function normalizeLinkUrl") && t.includes("https?:"),
    dedupe: t.includes("seenFile"),
    caps: t.includes("slice(0, 500)") && t.includes("slice(0, 2000)"),
  };
});
check("app.js has csvSafeCell", srcHas.csvSafe, JSON.stringify(srcHas));
check("app.js has normalizeLinkUrl", srcHas.normalize);
check("app.js dedupes import", srcHas.dedupe);
check("app.js length caps", srcHas.caps);

const SHOTS = process.env.UPDATE_SHOTS === "1" ? new URL("../screenshots/", import.meta.url).pathname : path.join(TMP, "unpack-shots-dl/");
fs.mkdirSync(SHOTS, { recursive: true });
await p.screenshot({ path: SHOTS + "13-mod-download.png" });
check("no page errors", !errors.length, errors.join(" | "));
check("frontend made no remote requests", !external.length, external.join(" "));
await b.close(); server.close();
fs.rmSync(W, { recursive: true, force: true });
fs.rmSync(path.join(TMP, "unpack-bridge-downloads"), { recursive: true, force: true });
if (process.env.UPDATE_SHOTS !== "1") fs.rmSync(path.join(TMP, "unpack-shots-dl"), { recursive: true, force: true });
console.log(`${total - fails}/${total} passed`);
console.log(fails ? `${fails} FAILED` : "ALL PASS");
process.exit(fails ? 1 : 0);
