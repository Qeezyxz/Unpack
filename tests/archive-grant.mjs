// Bridge: list_archive / read_entry refuse paths without a grant.
import puppeteer from "/workspace/testenv/node_modules/puppeteer-core/lib/esm/puppeteer/puppeteer-core.js";
import fs from "fs";
import os from "os";
import path from "path";

const OUT = path.join(os.tmpdir(), `unpack-archive-grant-${process.pid}-${Date.now()}`);
fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });
const arch = path.join(OUT, "secret.7z");
fs.copyFileSync(
  process.env.TESTDATA ? process.env.TESTDATA + "/plain.7z" : "/workspace/testdata/plain.7z",
  arch
);

const shim = fs.readFileSync(new URL("./test-shim.js", import.meta.url), "utf8");
const b = await puppeteer.launch({
  executablePath: "/usr/bin/google-chrome",
  args: ["--no-sandbox"],
});
const p = await b.newPage();
await p.evaluateOnNewDocument(shim);
await p.goto(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/`, { waitUntil: "load" });
await new Promise((r) => setTimeout(r, 500));

const denied = await p.evaluate(async (archivePath) => {
  let listErr = false;
  let readErr = false;
  try {
    await window.__TAURI__.core.invoke("list_archive", { path: archivePath });
  } catch {
    listErr = true;
  }
  try {
    await window.__TAURI__.core.invoke("read_entry", { path: archivePath, entry: "a.txt" });
  } catch {
    readErr = true;
  }
  return { listErr, readErr };
}, arch);

if (!denied.listErr || !denied.readErr) {
  console.error("FAIL: ungranted archive must not list or read", denied);
  await b.close();
  process.exit(1);
}
console.log("PASS ungranted archive refused for list_archive and read_entry");

await fetch(`http://127.0.0.1:${process.env.UNPACK_BRIDGE_PORT || 18790}/invoke/__test_grant`, {
  method: "POST",
  body: JSON.stringify({ paths: [OUT, arch] }),
});
const ok = await p.evaluate(async (archivePath) => {
  try {
    const listing = await window.__TAURI__.core.invoke("list_archive", { path: archivePath });
    return listing.entries?.length > 0;
  } catch {
    return false;
  }
}, arch);

await b.close();
if (!ok) {
  console.error("FAIL: granted archive should list");
  process.exit(1);
}
console.log("PASS granted archive lists after pick grant");
