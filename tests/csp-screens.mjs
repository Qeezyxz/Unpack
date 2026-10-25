// CSP smoke: open major UI surfaces under the production CSP (bridge serves tauri.conf.json).
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";

const BR = `http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`;
const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"] });
const p = await b.newPage();
const viol = [];
p.on("console", (m) => {
  if (/Content Security Policy|Refused to apply inline style|unsafe-inline/i.test(m.text())) viol.push(m.text());
});
await p.evaluateOnNewDocument(() => {
  document.addEventListener("securitypolicyviolation", (e) => {
    window.__cspViolations = window.__cspViolations || [];
    window.__cspViolations.push(`${e.violatedDirective}:${e.blockedURI}`);
  });
});
await p.evaluateOnNewDocument(shim);
await p.evaluateOnNewDocument(() => {
  localStorage.clear();
  localStorage.setItem("unpack.theme", "dark");
  localStorage.setItem("unpack.tipsDone", "1");
});
await p.goto(BR, { waitUntil: "load" });
await new Promise((r) => setTimeout(r, 1200));

const hdr = await p.evaluate(() => document.querySelector('meta[http-equiv="Content-Security-Policy"]')?.content || "");
const fetchHdr = await p.evaluate(async () => (await fetch("/")).headers.get("content-security-policy") || "");
const csp = fetchHdr || hdr;
let fails = 0;
const check = (n, ok, info = "") => {
  if (!ok) fails++;
  console.log((ok ? "PASS " : "FAIL ") + n + (info ? " — " + info : ""));
};

check("CSP served", /style-src/.test(csp), csp.slice(0, 120));
check("style-src omits unsafe-inline", !/style-src[^;]*unsafe-inline/i.test(csp), csp);

const click = (sel) => p.evaluate((s) => document.querySelector(s)?.click(), sel);
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

await click("[data-action=settings]");
await wait(500);
await click("[data-settings-close]");
await wait(300);
await click("[data-action=library]");
await wait(800);
await click("[data-lib-close]");
await wait(300);
await click("[data-action=mods]");
await wait(500);
await click("[data-mods-close]");
await wait(300);
await click("[data-action=xbox]");
await wait(500);
await click("[data-xbox-close]");
await wait(300);
await click("[data-action=god]");
await wait(400);
await click("[data-god-close]");
await wait(300);

await fetch(`${BR}invoke/__plant_migration_notice`, { method: "POST", body: "{}" }).catch(() => {});
await p.reload({ waitUntil: "load" });
await wait(1500);
await click("[data-migration-ok]");
await wait(300);

const pageViol = await p.evaluate(() => window.__cspViolations || []);
const all = [...viol, ...pageViol];
check("0 CSP violations across screens", all.length === 0, all.join(" | "));
check("progress bar width uses JS not blocked", await p.evaluate(() => {
  const el = document.createElement("div");
  el.dataset.jobBar = "t";
  el.className = "qbar-fill";
  document.body.appendChild(el);
  el.style.width = "50%";
  return el.style.width === "50%";
}), "");

await b.close();
console.log(fails ? `${fails} FAILED` : "ALL PASS");
process.exit(fails ? 1 : 0);
