// End-to-end UI test: headless Chrome + fake __TAURI__ (tests/test-shim.js) + real Rust archive code (examples/bridge.rs).
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";
const TMP = os.tmpdir();
const D = process.env.TESTDATA || "/workspace/testdata";
const OUT = path.join(TMP, "unpack-e2e");
// Self-contained test data: build the big/ISO fixtures if they're missing. Failure = test failure.
import { execFileSync } from "child_process";
const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za", "/usr/bin/7z"].find((x) => fs.existsSync(x));
if (!fs.existsSync(D + "/big.7z")) {
  console.log("generating big.7z (300 MiB, stored)…");
  execFileSync("sh", ["-c", `head -c 314572800 /dev/urandom > "${D}/big.bin" && "${SZ}" a -t7z -mx=0 -bso0 "${D}/big.7z" "${D}/big.bin" && rm "${D}/big.bin"`], { stdio: "inherit" });
}
if (!fs.existsSync(D + "/halo3.iso")) {
  console.log("generating halo3.iso (synthetic Xbox 360 image)…");
  execFileSync("python3", [new URL("./make_iso.py", import.meta.url).pathname, D + "/halo3.iso", "4D5307E6", String(400 * 1024 * 1024)], { stdio: "inherit" });
}
for (const f of ["big.7z", "halo3.iso"]) if (!fs.existsSync(D + "/" + f)) throw new Error("could not generate " + f);
fs.rmSync(OUT, { recursive: true, force: true }); fs.mkdirSync(OUT, { recursive: true });
const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"], protocolTimeout: 60000 });
const p = await b.newPage();
const errors = [];
p.on("pageerror", (e) => errors.push(e.message));
p.on("dialog", (d) => { errors.push("DIALOG " + d.message()); d.dismiss(); });
await p.evaluateOnNewDocument(shim);
await p.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" }); await new Promise((r) => setTimeout(r, 800));
const results = [];
const check = (name, ok, info = "") => { results.push({ name, ok: !!ok, info }); console.log((ok ? "PASS " : "FAIL ") + name + (info ? " — " + info : "")); };
const status = () => p.evaluate(() => document.querySelector(".status-line")?.textContent.trim() || "");
const waitStatus = (re, t = 30000) => p.waitForFunction((r) => new RegExp(r).test(document.querySelector(".status-line")?.textContent || "") || document.querySelector("#pw"), { timeout: t }, re.source).catch(() => {});
const drop = (paths) => p.evaluate((x) => window.__TEST.drop(x), paths);
const click = (sel) => p.evaluate((s) => document.querySelector(s)?.click(), sel);
const setFolder = (f) => p.evaluate((x) => (window.__TEST.folder = x), f);
const tests = process.argv.slice(2);
const want = (n) => !tests.length || tests.includes(n);

check("desktop desk rendered", await p.$("[data-setting=destMode]"));

if (want("extract")) {
  fs.copyFileSync(D + "/plain.7z", OUT + "/plain.7z");
  await drop([OUT + "/plain.7z"]);
  await waitStatus(/files ·/);
  check("open 7z lists files", /4 files/.test(await status()), await status());
  const rows = await p.evaluate(() => [...document.querySelectorAll(".tree .row .name")].map((e) => e.textContent));
  check("html-looking name shown as text", rows.includes("<img src=x onerror=alert(1)>.txt") && !(await p.$("img[src=x]")));
  await click("[data-action=extract-default]");
  await waitStatus(/Saved/);
  check("extract next to archive", fs.existsSync(OUT + "/plain/nested/deep/er/d.txt"), await status());
  await click("[data-action=extract-default]");
  await waitStatus(/Saved .*plain-2/);
  check("second extract numbered", fs.existsSync(OUT + "/plain-2/a.txt"), await status());
  await click("[data-action=extract-here]");
  await waitStatus(/Saved .*unpack-e2e$/);
  check("extract here", fs.existsSync(OUT + "/a.txt") && fs.existsSync(OUT + "/nested/n.json"), await status());
  // preview
  await p.evaluate(() => [...document.querySelectorAll(".tree [data-file]")].find((e) => e.textContent.includes("a.txt"))?.click());
  await p.waitForFunction(() => document.querySelector("#preview pre"), { timeout: 10000 }).catch(() => {});
  check("text preview via read_entry", (await p.evaluate(() => document.querySelector("#preview pre")?.textContent)) === "hello\n");
  // dest folder mode
  fs.mkdirSync(OUT + "/chosen");
  await setFolder(OUT + "/chosen");
  await click("[data-action=choose-folder]");
  await new Promise((r) => setTimeout(r, 300));
  await click("[data-action=extract-default]");
  await waitStatus(/Saved .*chosen/);
  check("extract into chosen folder", fs.existsSync(OUT + "/chosen/plain/a.txt"), await status());
}

if (want("password")) {
  await p.evaluate(() => { localStorage.setItem("unpack.destMode", "beside"); });
  fs.copyFileSync(D + "/pw.7z", OUT + "/pw.7z");
  await drop([OUT + "/pw.7z"]);
  await p.waitForSelector("#pw", { timeout: 15000 }).catch(() => {});
  check("password dialog for encrypted-header 7z", await p.$("#pw"));
  await p.type("#pw", "nope"); await click("[data-pw] button[type=submit]");
  await p.waitForFunction(() => document.querySelector("#pw") && /didn't work/.test(document.querySelector(".dialog p")?.textContent || ""), { timeout: 15000 }).catch(() => {});
  check("wrong password re-asks with hint", await p.evaluate(() => /didn't work/.test(document.querySelector(".dialog p")?.textContent || "")));
  await p.type("#pw", "secret"); await click("[data-pw] button[type=submit]");
  await waitStatus(/files ·/);
  await click("[data-action=extract-default]");
  await waitStatus(/Saved/);
  check("password 7z extracted", fs.readFileSync(OUT + "/pw/a.txt", "utf8") === "hello\n", await status());
  // zip with encrypted entries: listing works, password asked before extract
  fs.copyFileSync(D + "/pw.zip", OUT + "/pw.zip");
  await drop([OUT + "/pw.zip"]);
  await p.waitForSelector("#pw", { timeout: 15000 }).catch(() => {});
  check("password asked for encrypted zip", await p.$("#pw"));
  await p.type("#pw", "secret"); await click("[data-pw] button[type=submit]");
  await waitStatus(/files ·/);
  await click("[data-action=extract-default]");
  await waitStatus(/Saved/);
  check("password zip extracted", fs.readFileSync(OUT + "/pw-2/a.txt", "utf8") === "hello\n" || fs.readFileSync(OUT + "/pw/a.txt", "utf8") === "hello\n", await status());
}

if (want("evil")) {
  fs.mkdirSync(OUT + "/evil-dest/inner", { recursive: true });
  fs.copyFileSync(D + "/evil.zip", OUT + "/evil-dest/inner/evil.zip");
  await drop([OUT + "/evil-dest/inner/evil.zip"]);
  await waitStatus(/files ·/);
  await click("[data-action=extract-here]");
  await waitStatus(/Saved/);
  const outside = fs.readdirSync(OUT + "/evil-dest");
  check("zip-slip: nothing escapes", outside.length === 1 && outside[0] === "inner", JSON.stringify(outside));
}

if (want("big")) {
  fs.mkdirSync(OUT + "/big", { recursive: true });
  await setFolder(OUT + "/big");
  await p.evaluate(() => { localStorage.setItem("unpack.destMode", "ask"); });
  await drop([D + "/big.7z"]);
  await waitStatus(/files ·/);
  // progress sampling
  await click("[data-action=extract-default]");
  const seen = new Set();
  for (let i = 0; i < 100; i++) {
    const s = await status();
    const m = s.match(/(\d+)%/); if (m) seen.add(+m[1]);
    if (/Saved/.test(s)) break;
    await new Promise((r) => setTimeout(r, 20));
  }
  await waitStatus(/Saved/, 60000);
  check("big 300MB extracted", fs.statSync(OUT + "/big/big/big.bin").size === 300 * 1024 * 1024, "progress seen: " + [...seen].join(","));
  // cancel
  await click("[data-action=extract-default]");
  await new Promise((r) => setTimeout(r, 150));
  await click("[data-action=cancel-one]");
  await waitStatus(/Cancelled|Saved/, 60000);
  const st = await status();
  check("cancel stops & cleans", /Cancelled/.test(st) ? !fs.readdirSync(OUT + "/big").some((n) => n.startsWith(".unpack-tmp")) : true, st + (/Saved/.test(st) ? " (finished before cancel landed)" : ""));
  fs.rmSync(OUT + "/big", { recursive: true, force: true });
}

if (want("launch")) {
  fs.mkdirSync(OUT + "/launch", { recursive: true });
  fs.copyFileSync(D + "/plain.zip", OUT + "/launch/plain.zip");
  fs.copyFileSync(D + "/wrapped.zip", OUT + "/launch/wrapped.zip");
  const push = (args) => p.evaluate((a) => window.__TAURI__.core.invoke("__push_launch", { args: a }), args);
  await push(["--extract-here", OUT + "/launch/plain.zip"]);
  await push(["--extract-folder", OUT + "/launch/wrapped.zip"]);
  await push(["--extract-folder", OUT + "/launch/plain.zip"]);
  await new Promise((r) => setTimeout(r, 4000));
  const l = fs.readdirSync(OUT + "/launch").sort();
  check("explorer: extract here", fs.existsSync(OUT + "/launch/a.txt") && fs.existsSync(OUT + "/launch/nested/n.json"), l.join(","));
  check("explorer: extract to folder (single root kept)", fs.existsSync(OUT + "/launch/MyGame/readme.txt"));
  check("explorer: extract to folder", fs.existsSync(OUT + "/launch/plain/a.txt"));
  await push([OUT + "/launch/plain.zip"]);
  await new Promise((r) => setTimeout(r, 1500));
  check("double-click opens without extracting", (await p.evaluate(() => document.querySelector(".panel h2")?.textContent)) === "plain.zip" && !fs.existsSync(OUT + "/launch/plain-2"));
}

if (want("create")) {
  const C = OUT + "/create";
  fs.mkdirSync(C, { recursive: true });
  await drop([D + "/src/nested", D + "/src/a.txt"]);
  await p.waitForSelector("[data-composer]", { timeout: 5000 }).catch(() => {});
  check("dropping non-archives opens Create archive", await p.$("[data-composer]"));
  const items = await p.evaluate(() => document.querySelectorAll(".cmp-list .row").length);
  check("composer lists both items", items === 2, String(items));
  await p.evaluate((x) => (window.__TEST.save = x), C + "/made");
  await click("[data-composer] button[type=submit]");
  await p.waitForFunction(() => /Created|Failed/.test(document.querySelector("[data-composer] .status-line")?.textContent || ""), { timeout: 20000 }).catch(() => {});
  check("zip created (extension added)", fs.existsSync(C + "/made.zip"), await p.evaluate(() => document.querySelector("[data-composer] .status-line")?.textContent));
  // 7z with password
  await drop([D + "/src/nested"]);
  await new Promise((r) => setTimeout(r, 300));
  await p.select("[data-cmp-format]", "7z");
  await new Promise((r) => setTimeout(r, 200));
  await p.evaluate(() => {
    const el = document.querySelector("[data-cmp-pw]");
    el.value = "pa ss\"word";
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await p.evaluate((x) => (window.__TEST.save = x), C + "/secret.7z");
  await click("[data-composer] button[type=submit]");
  await p.waitForFunction(() => /Created|Failed/.test(document.querySelector("[data-composer] .status-line")?.textContent || ""), { timeout: 20000 }).catch(() => {});
  check("password 7z created", fs.existsSync(C + "/secret.7z"));
  await click("[data-cmp-close]");
  // open it back: password needed (names hidden)
  await drop([C + "/secret.7z"]);
  await p.waitForSelector("#pw", { timeout: 10000 }).catch(() => {});
  check("created 7z asks for password", await p.$("#pw"));
  await p.type("#pw", "pa ss\"word"); await click("[data-pw] button[type=submit]");
  await waitStatus(/files ·/);
  check("created 7z opens with password (quote+space)", /2 files/.test(await status()), await status());
}

if (want("queue")) {
  const Q = OUT + "/queue";
  fs.mkdirSync(Q + "/src", { recursive: true });
  for (const n of ["b1", "b2", "b3"]) fs.linkSync(D + "/big.7z", Q + "/src/" + n + ".7z");
  fs.copyFileSync(D + "/plain.zip", Q + "/src/plain.zip");
  fs.mkdirSync(Q + "/out");
  await p.evaluate((x) => { localStorage.setItem("unpack.destMode", "folder"); localStorage.setItem("unpack.destPath", x); }, Q + "/out");
  await click("[data-action=clear-all]");
  await p.evaluate(() => { const cb = document.querySelector("[data-setting=autoWrite]"); if (cb && !cb.checked) cb.click(); });
  await drop(["b1.7z", "b2.7z", "b3.7z", "plain.zip"].map((n) => Q + "/src/" + n));
  let maxRunning = 0, sawQueued = false, cancelled = false;
  for (let i = 0; i < 600; i++) {
    const st = await p.evaluate(() => [...document.querySelectorAll(".qrow")].map((r) => ({ name: r.querySelector(".name").textContent, s: r.querySelector(".qstatus").textContent, bar: r.querySelector("[data-job-bar]").style.width })));
    const run = st.filter((r) => /Extracting/.test(r.s)).length;
    maxRunning = Math.max(maxRunning, run);
    const q = st.find((r) => /Waiting in queue/.test(r.s));
    if (q) sawQueued = true;
    if (q && !cancelled) {
      await p.evaluate((n) => [...document.querySelectorAll(".qrow")].find((r) => r.querySelector(".name").textContent === n)?.querySelector("[data-cancel-job]")?.click(), q.name);
      cancelled = q.name.replace(".7z", "").replace(".zip", "");
    }
    if (st.length === 4 && st.every((r) => /Saved|Removed|Cancelled|Failed/.test(r.s))) break;
    await new Promise((r) => setTimeout(r, 25));
  }
  const fin = await p.evaluate(() => [...document.querySelectorAll(".qrow")].map((r) => r.querySelector(".name").textContent + ": " + r.querySelector(".qstatus").textContent));
  check("batch: never more than 2 at once", maxRunning <= 2 && maxRunning >= 1, "max running " + maxRunning);
  check("batch: others wait in queue", sawQueued);
  const outs = fs.readdirSync(Q + "/out").sort();
  check("batch: all finished, queued one cancelled", cancelled && !outs.includes(cancelled) && outs.length === 3, outs.join(",") + " | " + fin.join(" ; "));
  const hist = await p.evaluate(() => [...document.querySelectorAll(".hist-row .name")].map((e) => e.textContent));
  check("history lists recent jobs", hist.includes("b2.7z") && hist.includes("b3.7z"), hist.slice(0, 6).join(","));
  fs.rmSync(Q, { recursive: true, force: true });
}

if (want("god")) {
  const G = OUT + "/god";
  fs.mkdirSync(G, { recursive: true });
  await p.evaluate((x) => localStorage.setItem("unpack.godDest", x), G);
  await drop([D + "/halo3.iso", D + "/unknown.iso", D + "/notxbox.iso"]);
  await p.waitForSelector("[data-god]", { timeout: 5000 }).catch(() => {});
  check("dropping ISOs opens ISO → GOD", await p.$("[data-god]"));
  await p.waitForFunction(() => [...document.querySelectorAll(".god-row .qstatus")].every((e) => !/Reading/.test(e.textContent)), { timeout: 15000 }).catch(() => {});
  const rows = await p.evaluate(() => [...document.querySelectorAll(".god-row")].map((r) => ({ n: r.querySelector(".name").textContent, t: r.querySelector("[data-god-title]").value, s: r.querySelector(".qstatus").textContent, e: r.querySelector(".error-box")?.textContent || "" })));
  check("game title read from ISO (Halo 3)", rows.find((r) => r.n === "halo3.iso")?.t === "Halo 3", JSON.stringify(rows.map((r) => r.t + " | " + r.s)));
  check("unknown title falls back to file name", rows.find((r) => r.n === "unknown.iso")?.t === "unknown");
  check("non-Xbox ISO rejected clearly", /Not an Xbox ISO/.test(rows.find((r) => r.n === "notxbox.iso")?.e || ""));
  // rename unknown one
  await p.evaluate(() => { const i = [...document.querySelectorAll(".god-row")].find((r) => r.querySelector(".name").textContent === "unknown.iso").querySelector("[data-god-title]"); i.value = "My <b>Homebrew</b>"; i.dispatchEvent(new Event("input")); });
  await click("[data-god-go]");
  // cancel the second (queued) one? no: let both run, cancel nothing; then test cancel separately
  await p.waitForFunction(() => !document.querySelector("[data-god-cancel-all]"), { timeout: 120000 }).catch(() => {});
  const after = await p.evaluate(() => [...document.querySelectorAll(".god-row .qstatus")].map((e) => e.textContent));
  check("batch converted", fs.existsSync(G + "/4D5307E6/00007000/12345678") && fs.readdirSync(G + "/4D5307E6/00007000/12345678.data").length === 3 && fs.existsSync(G + "/0ABCDEF1/00007000/12345678"), after.join(" ; "));
  const hdr = fs.readFileSync(G + "/0ABCDEF1/00007000/12345678");
  check("custom title written to header", hdr.includes(Buffer.from("My <b>Homebrew</b>", "utf16le").swap16()));
  // cancel test
  fs.rmSync(G + "/4D5307E6", { recursive: true });
  await p.evaluate(() => { const r = [...document.querySelectorAll(".god-row")].find((r) => r.querySelector(".name").textContent === "halo3.iso"); });
  await click("[data-god-close]");
  await drop([D + "/halo3.iso"]);
  await p.waitForFunction(() => document.querySelector("[data-god-go]") && !document.querySelector("[data-god-go]").disabled, { timeout: 10000 }).catch(() => {});
  await click("[data-god-go]");
  await p.waitForFunction(() => /Part 1\//.test(document.querySelector(".god-row .qstatus")?.textContent || ""), { timeout: 30000 }).catch(() => {});
  await click("[data-god-cancel-all]");
  await p.waitForFunction(() => !document.querySelector("[data-god-cancel-all]"), { timeout: 60000 }).catch(() => {});
  const st = await p.evaluate(() => document.querySelector(".god-row .qstatus")?.textContent);
  check("GOD cancel cleans up", /Cancelled/.test(st) && !fs.existsSync(G + "/4D5307E6"), st);
  await click("[data-god-close]");
  fs.rmSync(G, { recursive: true, force: true });
}

check("no page errors", errors.length === 0, errors.join(" | "));
if (want("csp")) {
  // M4: the in-page fallbacks (RAR via unrar wasm, 7Z wasm, ZIP) must work under the app's CSP,
  // which no longer allows 'unsafe-eval'. Browser mode = no __TAURI__, so the wasm paths run.
  const q = await b.newPage();
  const viol = [];
  q.on("console", (m) => { if (/Content Security Policy|unsafe-eval/i.test(m.text())) viol.push(m.text()); });
  q.on("pageerror", (e) => viol.push(e.message));
  await q.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" });
  const hdr = await q.evaluate(async () => (await fetch("/")).headers.get("content-security-policy") || "");
  check("page served with app CSP (no unsafe-eval)", /wasm-unsafe-eval/.test(hdr) && !/'unsafe-eval'/.test(hdr), hdr);
  for (const [f, n] of [["sample.rar", 2], ["plain.7z", 4]]) {
    await q.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" });
    const inp = await q.$("input[type=file]");
    await inp.uploadFile(D + "/" + f);
    await q.waitForFunction(() => /files ·/.test(document.querySelector(".status-line")?.textContent || "") || document.querySelector(".error-box.danger"), { timeout: 30000 }).catch(() => {});
    const st = await q.evaluate(() => document.querySelector(".status-line")?.textContent.trim() || document.querySelector(".error-box.danger")?.textContent);
    check(`${f} opens in-page under strict CSP`, new RegExp("^" + n + " files ·").test(st), st);
  }
  check("no CSP violations", viol.length === 0, viol.join(" | "));
  await q.close();
}

await b.close();
const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
