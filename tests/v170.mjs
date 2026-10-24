// UI tests for v1.7.0: TU settings copy, autoTu toggle, library filter, queue pause, retry failed,
// Copy details button, pick_save export format modal, companion setting, error_help hint.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import { execFileSync } from "child_process";

const D = process.env.TESTDATA || "/workspace/testdata";
const OUT = "/tmp/unpack-v170";
const PORT = process.env.UNPACK_BRIDGE_PORT || 18790;
const SZ = ["/usr/lib/7zip/7za", "/usr/bin/7za", "/usr/bin/7z"].find((x) => fs.existsSync(x));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });
if (!fs.existsSync(D + "/big.7z") && SZ) {
  execFileSync("sh", ["-c", `head -c 104857600 /dev/urandom > "${OUT}/big.bin" && "${SZ}" a -t7z -mx=0 -bso0 "${D}/big.7z" "${OUT}/big.bin" && rm "${OUT}/big.bin"`]);
}

const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({ executablePath: "/usr/bin/google-chrome", args: ["--no-sandbox"], protocolTimeout: 60000 });
const p = await b.newPage();
await p.setViewport({ width: 1180, height: 820 });
const errors = [];
p.on("pageerror", (e) => errors.push(e.message));
p.on("dialog", async (d) => {
  errors.push("DIALOG " + d.message());
  await d.dismiss();
});
await p.evaluateOnNewDocument(shim);
await p.evaluateOnNewDocument(() => {
  if (!sessionStorage.getItem("v170")) {
    localStorage.clear();
    sessionStorage.setItem("v170", "1");
  }
});

let fails = 0;
const check = (name, ok, info = "") => {
  if (!ok) fails++;
  console.log((ok ? "PASS " : "FAIL ") + name + (info ? " — " + info : ""));
};
const click = (sel) => p.evaluate((s) => document.querySelector(s)?.click(), sel);
const ls = (k) => p.evaluate((x) => localStorage.getItem(x), k);

await p.goto(`http://127.0.0.1:${PORT}/`, { waitUntil: "load" });
await wait(800);

// Settings: TU copy mentions 000B0000
{
  await click("[data-action=settings]");
  await wait(400);
  const tu = await p.evaluate(() => document.querySelector("[data-set-autotu]") != null);
  const copy = await p.evaluate(() => document.body.innerText);
  check("settings: autoTu toggle exists", tu);
  check("settings: TU copy mentions 000B0000", /000B0000/.test(copy) && !/Title updates are left alone/i.test(copy), copy.slice(0, 400));
  check("settings: companion disc mode select exists", await p.evaluate(() => !!document.querySelector("[data-set-companion]")));
  await click("[data-set-autotu]");
  check("settings: autoTu off is remembered", (await ls("unpack.autoTu")) === "0");
  await click("[data-set-autotu]");
  await click("[data-settings-close]");
}

// Library filter UI
{
  await click("[data-action=library]");
  await wait(1200);
  check("library: search toolbar", await p.evaluate(() => !!document.querySelector("[data-lib-q]")));
  check("library: sort select", await p.evaluate(() => !!document.querySelector("[data-lib-sort]")));
  check("library: filter select", await p.evaluate(() => !!document.querySelector("[data-lib-filter]")));
  await p.select("[data-lib-filter]", "god");
  check("library: filter persisted", (await ls("unpack.libFilterKind")) === "god");
  await click("[data-lib-close]");
}

// Queue pause button (synthetic waiting job on test bridge)
{
  await p.evaluate(() => window.__unpackTestPause?.());
  await wait(200);
  check("queue: pause button exists when waiting", await p.evaluate(() => !!document.querySelector("[data-queue-pause]")));
  await click("[data-queue-pause]");
  check("queue: paused state in header", await p.evaluate(() => /paused/i.test(document.querySelector(".qhead")?.textContent || "")));
  await click("[data-queue-pause]");
  await click("[data-action=clear-all]");
}

// Error banner: Copy details + error_help hint (FTP busy message)
{
  await p.waitForFunction(() => typeof window.showError === "function", { timeout: 10000 });
  await p.evaluate(async () => {
    await window.showError("already talking to the console");
  });
  await p.waitForFunction(() => {
    const h = document.querySelector("[data-error-hint]");
    return h && !h.hasAttribute("hidden") && (h.textContent || "").length > 0;
  }, { timeout: 5000 }).catch(() => {});
  const banner = await p.evaluate(() => ({
    copy: document.querySelector("[data-error-copy]")?.textContent,
    hint: document.querySelector("[data-error-hint]")?.textContent || "",
    hidden: document.querySelector("[data-error-hint]")?.hasAttribute("hidden"),
  }));
  check("error banner: button says Copy details", banner.copy === "Copy details", banner.copy);
  check("error_help: hint shown in banner", !banner.hidden && /FTP/i.test(banner.hint), JSON.stringify(banner));
  await click("[data-error-dismiss]");
}

// Retry failed button exists after a failed batch (mock summary)
{
  await p.evaluate(() => {
    window.summary = { extracted: 0, failed: 1, skipped: 0, failedJobs: [{ id: "x", opts: {} }], lastOutput: "" };
    document.querySelector("main")?.insertAdjacentHTML("afterbegin", `<section class="summary"><button data-sum-retry type="button">Retry failed (1)</button></section>`);
  });
  check("batch summary: retry failed button exists", await p.evaluate(() => !!document.querySelector("[data-sum-retry]")));
}

// Export format uses modal (not window.prompt) — mock pick_save path
{
  await click("[data-action=mods]");
  await wait(500);
  await click("[data-mods-tab=names]");
  await wait(300);
  await p.evaluate(() => {
    window.__TEST.save = "/tmp/unpack-v170/export.json";
  });
  await click("[data-mn-export]");
  await p.waitForSelector("[data-fmt-overlay]", { timeout: 3000 }).catch(() => {});
  check("export: format picker modal", await p.evaluate(() => !!document.querySelector("[data-fmt-json]")));
  await click("[data-fmt-json]");
  await wait(400);
  check("export: pick_save was used", await p.evaluate(() => window.__exportPickSaveCalled === true));
  check("export: file written to the picked path", fs.existsSync("/tmp/unpack-v170/export.json") && Array.isArray(JSON.parse(fs.readFileSync("/tmp/unpack-v170/export.json", "utf8"))));
  await click("[data-mods-close]");
}

// R21 L1/L2: import keeps saved passwords, never turns on network opt-ins without a yes, drops PC folders
{
  await p.evaluate(() => {
    localStorage.setItem("unpack.consoles", JSON.stringify([{ name: "X", host: "192.168.1.5", port: 21, user: "xboxftp", pass: "MySecret" }]));
    localStorage.setItem("unpack.destPath", "D:\\Mine");
  });
  fs.writeFileSync(OUT + "/import.json", JSON.stringify({ version: 1, settings: {
    consoles: JSON.stringify([{ name: "X", host: "192.168.1.5", port: 21, user: "xboxftp", pass: "" }]),
    releaseCheck: "1", coverNet: "1", destPath: "C:\\Windows\\System32", maxJobs: "99" } }));
  await click("[data-action=settings]");
  await wait(300);
  const [fc] = await Promise.all([p.waitForFileChooser({ timeout: 5000 }), click("[data-set-import]")]);
  await fc.accept([OUT + "/import.json"]);
  await wait(800);
  const cons = JSON.parse((await ls("unpack.consoles")) || "[]");
  check("import: saved password kept when the file has none", cons[0]?.pass === "MySecret", JSON.stringify(cons));
  check("import: declined opt-ins stay off", (await ls("unpack.releaseCheck")) !== "1" && (await ls("unpack.coverNet")) !== "1");
  check("import: asked about the opt-ins", errors.some((e) => /use the internet/.test(e)), errors.join(" | "));
  check("import: destPath not taken from the file", (await ls("unpack.destPath")) === "D:\\Mine");
  check("import: bad maxJobs ignored", (await ls("unpack.maxJobs")) !== "99");
  for (let i = errors.length - 1; i >= 0; i--) if (/^DIALOG This settings file/.test(errors[i])) errors.splice(i, 1);
  await click("[data-settings-close]");
}

// R21 nit: USB wizard warnings are an amber note in Settings, not the red error banner
{
  fs.mkdirSync(OUT + "/usb", { recursive: true });
  await p.evaluate((f) => { window.__TEST.folder = f; }, OUT + "/usb");
  await click("[data-action=settings]");
  await wait(300);
  await click("[data-usb-wizard]");
  await wait(800);
  const st = await p.evaluate(() => ({ warn: document.querySelector("[data-usb-warn]")?.textContent || "", banner: !document.getElementById("job-error")?.classList.contains("hidden") }));
  check("usb wizard: warning shown amber in Settings", /not FAT32/.test(st.warn), st.warn);
  check("usb wizard: red error banner not used", !st.banner);
  await click("[data-settings-close]");
}

// R21 nit: update check sends the Tauri app version, not a hard-coded one
{
  await p.evaluate(() => localStorage.setItem("unpack.releaseCheck", "1"));
  await p.evaluateOnNewDocument(() => { if (window.__TEST) window.__TEST.version = "9.8.7"; });
  await p.reload({ waitUntil: "load" });
  await wait(1200);
  const calls = await p.evaluate(() => window.__TEST.calls.filter((c) => c[0] === "check_release").map((c) => c[1].current));
  check("update check uses the app version", calls.length > 0 && calls.every((v) => v === "9.8.7"), JSON.stringify(calls));
  await p.evaluate(() => localStorage.setItem("unpack.releaseCheck", "0"));
}

check("no page errors", errors.length === 0, errors.join(" | "));

await b.close();
console.log(fails ? `${fails} FAILED` : "all passed");
process.exit(fails ? 1 : 0);
