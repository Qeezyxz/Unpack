const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
function check(name, ok, detail) { results.push(`${ok ? "PASS" : "FAIL"} ${name}${ok ? "" : " :: " + detail}`); }
const $$ = (id) => document.getElementById(id);
const drop = (paths) => window.__drop({ payload: { type: "drop", paths } });
const calls = (cmd) => window.__calls.filter((c) => c.cmd === cmd);

(async () => {
  await sleep(300);
  // settings applied
  check("settings host applied", $$("xbox-host").value === "192.168.1.9", $$("xbox-host").value);
  check("max jobs applied", $$("max-jobs").value === "3", $$("max-jobs").value);
  check("nested applied", $$("extract-nested").checked === false, String($$("extract-nested").checked));
  check("meta escaped", !document.querySelector("#settings-meta i"), $$("settings-meta").innerHTML);

  // 1. destination required
  drop(["/a/game.zip"]);
  await sleep(50);
  check("dest required banner", !$$("job-error").classList.contains("hidden") && /destination/i.test($$("job-error-text").textContent), $$("job-error-text").textContent);
  $$("dismiss-error").click();
  check("dismiss hides", $$("job-error").classList.contains("hidden"), "");

  // 2. concurrency limit with maxJobs=3
  $$("extract-dest").value = "/out";
  let active = 0, peak = 0; const gates = [];
  window.__behaviour.extract_archives = () => new Promise((res) => {
    active++; peak = Math.max(peak, active);
    gates.push(() => { active--; res([{ archive: "a", isos: [], nested: 0, inner_left: 0 }]); });
  });
  for (let i = 0; i < 6; i++) drop([`/a/g${i}.zip`]);
  await sleep(100);
  check("peak limited to 3", peak === 3 && gates.length === 3, `peak=${peak} gates=${gates.length}`);
  check("status says waiting", /waiting/i.test($$("job-status").textContent), $$("job-status").textContent);
  while (gates.length) { gates.shift()(); await sleep(30); }
  await sleep(100);
  check("all 6 ran, never over limit", calls("extract_archives").length === 6 && peak === 3, `calls=${calls("extract_archives").length} peak=${peak}`);

  // 3. notification failure must not stop auto-convert
  window.__behaviour.extract_archives = () => Promise.resolve([{ archive: "a", isos: ["/out/g/game.iso"], nested: 0, inner_left: 0 }]);
  window.__behaviour.notify = () => Promise.reject("no notifications");
  window.__behaviour.convert_iso = () => Promise.resolve([{ info: { name: "Halo" }, output: "/out/X", merged: false }]);
  const before = calls("convert_iso").length;
  drop(["/a/halo.zip"]);
  await sleep(200);
  check("convert ran despite notify failure", calls("convert_iso").length === before + 1, "");
  check("no error banner after notify failure", $$("job-error").classList.contains("hidden"), $$("job-error-text").textContent);
  check("status shows converted", /Converted Halo/.test($$("job-status").textContent), $$("job-status").textContent);

  // 4. failing job shows banner; banner clears on next clean job
  window.__behaviour.extract_archives = () => Promise.reject("Error: disk <b>full</b>");
  drop(["/a/bad.zip"]);
  await sleep(100);
  check("error banner shown", !$$("job-error").classList.contains("hidden") && $$("job-error-text").textContent === "disk <b>full</b>", $$("job-error-text").textContent);
  check("error text not parsed as html", !document.querySelector("#job-error-text b"), "");
  window.__behaviour.extract_archives = () => Promise.resolve([{ archive: "a", isos: [], nested: 0, inner_left: 2 }]);
  drop(["/a/ok.zip"]);
  await sleep(100);
  check("banner cleared by next job", $$("job-error").classList.contains("hidden"), "");
  check("inner_left hint", /left closed/.test($$("job-status").textContent), $$("job-status").textContent);

  // 5. error from a long-running job survives an unrelated quick job
  let release;
  window.__behaviour.extract_archives = () => new Promise((r) => { release = () => r([{ archive: "a", isos: [], nested: 0, inner_left: 0 }]); });
  drop(["/a/slow.zip"]);
  await sleep(50);
  window.__behaviour.xbox_test = () => Promise.reject("login failed");
  $$("xbox-test").click();
  await sleep(80);
  check("error shown while other job runs", !$$("job-error").classList.contains("hidden"), "");
  window.__behaviour.xbox_test = () => Promise.resolve("Connected");
  $$("xbox-test").click();
  await sleep(80);
  check("error kept while slow job still running", !$$("job-error").classList.contains("hidden"), "");
  release();
  await sleep(80);

  // 6. XSS via title search
  $$("title-search").value = "x";
  $$("title-search").oninput();
  await sleep(400);
  check("title results escaped", !document.querySelector("#title-results img") && !window.__xss && document.querySelectorAll("#title-results li").length === 1, `xss=${window.__xss}`);

  // 7. save settings carries the password but only as JSON
  $$("xbox-save").click();
  await sleep(50);
  check("settings saved", window.__saved && window.__saved.xboxHost === "192.168.1.9" && window.__saved.maxJobs === 3, JSON.stringify(window.__saved));
  check("password field is a password input", $$("xbox-pass").type === "password", $$("xbox-pass").type);

  // 8. progress events
  window.__listeners["unpack-progress"]({ payload: { message: "Hello <b>", current: 3, total: 6 } });
  check("progress text", $$("job-status").textContent === "Hello <b>" && $$("job-progress").value === 3, "");

  // 9. iso drop with auto-iso off does nothing
  $$("auto-iso").value = "off";
  const c0 = calls("convert_iso").length;
  drop(["/a/x.iso"]);
  await sleep(50);
  check("iso ignored when mode is off", calls("convert_iso").length === c0, "");
  check("off mode explains itself", /Auto ISO is Off/.test($$("job-status").textContent), $$("job-status").textContent);
  $$("extract-auto").checked = false;
  const e0 = calls("extract_archives").length;
  drop(["/a/manual.zip"]);
  await sleep(50);
  check("auto-extract off explains itself", calls("extract_archives").length === e0 && /nothing was extracted/.test($$("job-status").textContent), $$("job-status").textContent);
  $$("extract-auto").checked = true;
  $$("auto-iso").value = "god";

  // 10. Escape closes modals; Enter modals open
  document.querySelector('[data-modal="settings"]').click();
  check("modal opens", !$$("overlay").classList.contains("hidden"), "");
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  check("escape closes", $$("overlay").classList.contains("hidden"), "");

  // mods list and search
  window.__behaviour.pick_files = () => Promise.resolve(["/m/Aurora.xex", "/m/<b>Plugin</b>.xex", "/m/Other.bin"]);
  $$("pick-mod-files").click();
  await sleep(50);
  check("mods listed", document.querySelectorAll("#mods-list li").length === 3 && !document.querySelector("#mods-list b"), "");
  $$("mods-search").value = "plugin";
  $$("mods-search").oninput();
  check("mods search filters", document.querySelectorAll("#mods-list li").length === 1, String(document.querySelectorAll("#mods-list li").length));
  document.querySelector("#mods-list li button").click();
  $$("mods-search").value = "";
  $$("mods-search").oninput();
  check("mods remove hits the right file", document.querySelectorAll("#mods-list li").length === 2 && !/Plugin/.test($$("mods-list").textContent), $$("mods-list").textContent);
  check("mods empty hint hidden when list has files", $$("mods-empty").classList.contains("hidden"), "");

  // 11. Send to Xbox with nothing selected
  window.__behaviour.send_to_xbox = () => Promise.reject("choose files or folders to send");
  $$("run-send").click();
  await sleep(60);
  check("send with no files errors", /choose files/.test($$("job-error-text").textContent), $$("job-error-text").textContent);

  document.title = "DONE";
  const pre = document.createElement("pre");
  pre.id = "results";
  pre.textContent = results.join("\n");
  document.body.prepend(pre);
})().catch((e) => { document.title = "CRASH"; const pre = document.createElement("pre"); pre.id = "results"; pre.textContent = "CRASH " + e + "\n" + results.join("\n"); document.body.prepend(pre); });
