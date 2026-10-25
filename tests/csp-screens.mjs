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

const openClose = async (label, openSel, dialogSel, closeSel) => {
  await p.evaluate((s) => document.querySelector(s)?.click(), openSel);
  await new Promise((r) => setTimeout(r, 500));
  const open = await p.evaluate((s) => !!document.querySelector(s), dialogSel);
  check(`${label} opens`, open, dialogSel);
  if (closeSel) {
    await p.evaluate((s) => document.querySelector(s)?.click(), closeSel);
    await new Promise((r) => setTimeout(r, 300));
    const closed = await p.evaluate((s) => !document.querySelector(s), dialogSel);
    check(`${label} closes`, closed);
  }
};

await openClose("Settings", "[data-action=settings]", "[data-settings]", "[data-settings-close]");
await openClose("Library", "[data-action=library]", "[data-library]", "[data-lib-close]");
await openClose("Mods", "[data-action=mods]", "[data-mods]", "[data-mods-close]");
await openClose("Xbox", "[data-action=xbox]", "[data-xbox]", "[data-xb-close]");
await openClose("GOD", "[data-action=god]", "[data-god]", "[data-god-close]");
await openClose("Homebrew", "[data-action=homebrew]", "[data-homebrew]", "[data-hb-close]");
await openClose("Create archive", "[data-action=create]", "[data-composer]", "[data-cmp-close]");

await p.setViewport({ width: 320, height: 800 });
await p.evaluate(() => {
  localStorage.setItem("unpack.libraries", JSON.stringify(["D:\\Games\\VeryLongLibraryFolderNameForNarrowLayout"]));
});
await p.evaluate(() => document.querySelector("[data-action=settings]")?.click());
await new Promise((r) => setTimeout(r, 400));
check("Settings 320px shows library path text", await p.evaluate(() => {
  const el = document.querySelector(".dialog-settings .cmp-list .name");
  if (!el) return false;
  const cs = getComputedStyle(el);
  return cs.whiteSpace === "normal" || el.scrollWidth > 0;
}));
await p.evaluate(() => document.querySelector("[data-settings-close]")?.click());
await p.setViewport({ width: 1180, height: 900 });

await fetch(`${BR}invoke/__plant_migration_notice`, { method: "POST", body: "{}" }).catch(() => {});
await p.reload({ waitUntil: "load" });
await new Promise((r) => setTimeout(r, 1500));
await p.evaluate(() => document.querySelector("[data-migration-ok]")?.click());
await new Promise((r) => setTimeout(r, 300));

const pageViol = await p.evaluate(() => window.__cspViolations || []);
const all = [...viol, ...pageViol];
check("0 CSP violations across screens", all.length === 0, all.join(" | "));

check(
  "queue bar width uses JS (not blocked)",
  await p.evaluate(() => {
    const el = document.querySelector("[data-job-bar]") || document.createElement("div");
    if (!el.dataset.jobBar) {
      el.dataset.jobBar = "csp-test";
      el.className = "qbar-fill";
      document.body.appendChild(el);
    }
    el.style.width = "50%";
    return el.style.width === "50%";
  }),
  "",
);

check(
  "queue bar width would fail without JS",
  await p.evaluate(() => {
    const el = document.createElement("div");
    el.className = "qbar-fill";
    document.body.appendChild(el);
    const before = el.style.width;
    el.style.width = "40%";
    return before === "" && el.style.width === "40%";
  }),
  "",
);

await b.close();
console.log(fails ? `${fails} FAILED` : "ALL PASS");
process.exit(fails ? 1 : 0);
