const invoke = (...args) => window.__TAURI__.core.invoke(...args);
const listen = (...args) => window.__TAURI__.event.listen(...args);

const views = {
  extract: ["Extract", "Unpack ZIP, RAR, 7Z, TAR, and more with 7-Zip."],
  convert: ["Convert ISO", "ISO → Games on Demand or an extracted XEX folder, then verify."],
  library: ["Game Library", "Search the built-in TitleID database and scan folders on disk."],
  usb: ["USB copy", "Copy to a stick with FAT32 checks and hash verification."],
  content: ["DLC / Title Updates", "Read STFS headers and place packages in the Xbox Content layout."],
  mods: ["Mod menus", "Manage a local mods folder and copy it where you need it."],
  downloads: ["Downloader", "HTTP download with resume and optional SHA-1 / SHA-256 check."],
  homebrew: ["Homebrew tools", "Browse a local tools folder (Aurora, XeXMenu, DashLaunch, …)."],
  xbox: ["Send to Xbox", "FTP to Aurora / XeXMenu / DashLaunch on the local network only."],
  settings: ["Settings", "Paths, theme, and toolchain status."],
};

const state = {
  extractFiles: [],
  convertFiles: [],
  packFiles: [],
  usbFiles: [],
  xboxFiles: [],
  packageFiles: [],
  settings: {},
};

const $ = (id) => document.getElementById(id);
const logEl = $("log");
let busy = false;

function log(line) {
  const time = new Date().toLocaleTimeString();
  logEl.textContent += `[${time}] ${line}\n`;
  logEl.scrollTop = logEl.scrollHeight;
  $("job-status").textContent = line;
}

function setList(id, paths) {
  const el = $(id);
  el.innerHTML = "";
  paths.forEach((p, i) => {
    const li = document.createElement("li");
    li.innerHTML = `<span>${p}</span><button data-i="${i}">remove</button>`;
    li.querySelector("button").onclick = () => {
      paths.splice(i, 1);
      setList(id, paths);
    };
    el.appendChild(li);
  });
}

function showView(name) {
  document.querySelectorAll(".view").forEach((v) => v.classList.add("hidden"));
  $(`view-${name}`).classList.remove("hidden");
  document.querySelectorAll("nav button").forEach((b) => b.classList.toggle("active", b.dataset.view === name));
  $("view-title").textContent = views[name][0];
  $("view-sub").textContent = views[name][1];
}

async function pickMany(title, extensions) {
  const files = (await invoke("pick_files", { title, extensions })) || [];
  return files.filter(Boolean);
}
async function pickDir(title) {
  return invoke("pick_folder", { title });
}

async function remember() {
  try {
    await invoke("save_settings", { settings: collectSettings() });
  } catch (_) {
    /* ignore */
  }
}

async function job(label, fn) {
  if (busy) {
    log("Wait for the current job to finish.");
    return;
  }
  busy = true;
  $("job-status").textContent = label;
  try {
    await fn();
    await remember();
  } catch (e) {
    log(String(e));
  } finally {
    busy = false;
  }
}

function bindDrop(el, onPaths) {
  el.addEventListener("dragover", (e) => {
    e.preventDefault();
    el.classList.add("over");
  });
  el.addEventListener("dragleave", () => el.classList.remove("over"));
  el.addEventListener("drop", (e) => {
    e.preventDefault();
    el.classList.remove("over");
  });
  el._onPaths = onPaths;
}

async function init() {
  document.querySelectorAll("nav button").forEach((b) => b.addEventListener("click", () => showView(b.dataset.view)));
  document.querySelectorAll("[data-drop]").forEach((el) => bindDrop(el, null));

  $("theme-select").value = localStorage.getItem("unpack.theme") || "system";
  $("theme-select").onchange = () => {
    localStorage.setItem("unpack.theme", $("theme-select").value);
    window.__applyTheme();
  };

  try {
    const meta = await invoke("app_meta");
    $("meta-line").textContent = `v${meta.version} · ${meta.title_count.toLocaleString()} TitleIDs`;
    $("settings-meta").innerHTML = `
      <dt>Version</dt><dd>${meta.version}</dd>
      <dt>TitleIDs</dt><dd>${meta.title_count}</dd>
      <dt>7-Zip</dt><dd>${meta.seven_zip || "not found (put 7za.exe next to Unpack.exe)"}</dd>
      <dt>Config</dt><dd>${meta.config_dir}</dd>`;
    state.settings = await invoke("load_settings");
    applySettings(state.settings);
  } catch (err) {
    log(`Running without native bridge (${err}). Open via \`npx tauri dev\`.`);
  }

  if (window.__TAURI__?.event) {
    await listen("unpack-progress", (event) => {
      const p = event.payload;
      $("job-progress").max = Math.max(1, p.total);
      $("job-progress").value = p.current;
      $("job-status").textContent = p.message;
    });
    try {
      const webview = window.__TAURI__.webview.getCurrentWebview();
      webview.onDragDropEvent((event) => {
        if (event.payload.type === "drop") {
          const paths = event.payload.paths || [];
          const view = document.querySelector(".view:not(.hidden)").id.replace("view-", "");
          ingestDrop(view, paths);
        }
      });
    } catch (_) {
      /* older webview API */
    }
  }

  bindExtract();
  bindConvert();
  bindLibrary();
  bindUsb();
  bindContent();
  bindMods();
  bindDownloads();
  bindHomebrew();
  bindXbox();
  bindSettings();
  document.addEventListener("keydown", onKeys);
  log("Unpack ready. Local files only — nothing is uploaded.");
}

function ingestDrop(view, paths) {
  if (view === "extract") {
    state.extractFiles.push(...paths);
    setList("extract-files", state.extractFiles);
  } else if (view === "convert") {
    state.convertFiles.push(...paths);
    setList("iso-files", state.convertFiles);
    $("iso-path").value = state.convertFiles[0] || paths[0] || "";
  } else if (view === "usb") {
    state.usbFiles.push(...paths);
    setList("usb-sources", state.usbFiles);
  } else if (view === "xbox") {
    state.xboxFiles.push(...paths);
    setList("xbox-files", state.xboxFiles);
  } else if (view === "content") {
    state.packageFiles.push(...paths);
  }
}

function applySettings(s) {
  if (!s) return;
  if (s.libraryRoot) $("library-root").value = s.libraryRoot;
  if (s.usbRoot) $("usb-dest").value = s.usbRoot;
  if (s.downloadDir) $("dl-dest").value = s.downloadDir;
  if (s.xboxHost) $("xbox-host").value = s.xboxHost;
  if (s.xboxPort) $("xbox-port").value = s.xboxPort;
  if (s.xboxUser) $("xbox-user").value = s.xboxUser;
  if (s.xboxPass) $("xbox-pass").value = s.xboxPass;
  if (s.xboxDest) $("xbox-dest").value = s.xboxDest;
  if (s.extractDest) $("extract-dest").value = s.extractDest;
  if (s.contentRoot) $("content-root").value = s.contentRoot;
  if (s.modsRoot) $("mods-root").value = s.modsRoot;
  if (s.homebrewRoot) $("hb-root").value = s.homebrewRoot;
}

function collectSettings() {
  return {
    libraryRoot: $("library-root").value,
    usbRoot: $("usb-dest").value,
    downloadDir: $("dl-dest").value,
    xboxHost: $("xbox-host").value,
    xboxPort: Number($("xbox-port").value || 21),
    xboxUser: $("xbox-user").value,
    xboxPass: $("xbox-pass").value,
    xboxDest: $("xbox-dest").value,
    extractDest: $("extract-dest").value,
    contentRoot: $("content-root").value,
    modsRoot: $("mods-root").value,
    homebrewRoot: $("hb-root").value,
    theme: $("theme-select").value,
  };
}

function bindExtract() {
  $("pick-archives").onclick = async () => {
    const files = await pickMany("Archives", ["zip", "rar", "7z", "tar", "gz", "xz", "iso"]);
    state.extractFiles.push(...files);
    setList("extract-files", state.extractFiles);
  };
  $("pick-extract-dest").onclick = async () => {
    const d = await pickDir("Extract to");
    if (d) $("extract-dest").value = d;
  };
  $("run-extract").onclick = () => job("Extracting…", async () => {
    if (!state.extractFiles.length || !$("extract-dest").value) throw new Error("Choose archives and a destination.");
    const result = await invoke("extract_archives", { archives: state.extractFiles, dest: $("extract-dest").value });
    log(`Extracted ${result.length} archive(s).`);
    await invoke("notify", { title: "Unpack", body: `Extracted ${result.length} archive(s)` });
  });
  $("pick-archive-inputs").onclick = async () => {
    state.packFiles = await pickMany("Files to pack");
    log(`Packing ${state.packFiles.length} item(s).`);
  };
  let packOut = "";
  $("pick-archive-out").onclick = async () => {
    packOut = await invoke("pick_save", { title: "Save archive", defaultName: "archive.7z" });
    if (packOut) log(`Archive will be ${packOut}`);
  };
  $("run-pack").onclick = () => job("Creating archive…", async () => {
    if (!state.packFiles.length || !packOut) throw new Error("Choose files and an archive name.");
    const result = await invoke("create_archive", { inputs: state.packFiles, dest: packOut });
    log(`Created ${result.archive}`);
  });
}

function bindConvert() {
  $("pick-iso").onclick = async () => {
    const files = await pickMany("Xbox ISO", ["iso", "ISO", "000"]);
    if (files.length) {
      state.convertFiles.push(...files);
      setList("iso-files", state.convertFiles);
      $("iso-path").value = state.convertFiles[0];
    }
  };
  $("pick-iso-dest").onclick = async () => {
    const d = await pickDir("Output folder");
    if (d) $("iso-dest").value = d;
  };
  $("inspect-iso").onclick = async () => {
    const paths = convertPaths();
    if (!paths.length) {
      log("Choose an ISO first.");
      return;
    }
    try {
      const infos = [];
      for (const path of paths) {
        infos.push(await invoke("inspect_iso", { path }));
      }
      $("iso-info").textContent = JSON.stringify(infos, null, 2);
      infos.forEach((info) => log(`ISO disc ${info.disc_number}/${info.disc_count} ${info.title_id} ${info.name}`));
    } catch (e) {
      $("iso-info").textContent = String(e);
      log(String(e));
    }
  };
  $("run-convert").onclick = () => job("Converting ISO…", async () => {
    const paths = convertPaths();
    if (!paths.length) throw new Error("Choose Disc 1 (and Disc 2 if you have it).");
    const mode = document.querySelector("input[name=iso-mode]:checked").value;
    const reports = await invoke("convert_iso", {
      paths,
      dest: $("iso-dest").value,
      mode,
      trim: $("iso-trim").checked,
      verify: $("iso-verify").checked,
      autoCompanions: $("iso-companions").checked,
    });
    const list = Array.isArray(reports) ? reports : [reports];
    $("iso-info").textContent = JSON.stringify(list, null, 2);
    for (const report of list) {
      const discs = report.merged ? ` (${report.discs?.length || 0} discs merged)` : "";
      log(`Converted ${report.info.name}${discs} → ${report.output}`);
      await invoke("notify", { title: "Unpack", body: `Converted ${report.info.name}${discs}` });
    }
  });
}

function convertPaths() {
  const listed = state.convertFiles.filter(Boolean);
  if (listed.length) return listed;
  const one = $("iso-path").value.trim();
  return one ? [one] : [];
}

function bindLibrary() {
  let timer;
  $("title-search").oninput = () => {
    clearTimeout(timer);
    timer = setTimeout(runSearch, 160);
  };
  async function runSearch() {
    const rows = await invoke("search_titles", { query: $("title-search").value, limit: 80 });
    const el = $("title-results");
    el.innerHTML = "";
    rows.forEach((r) => {
      const li = document.createElement("li");
      li.innerHTML = `<span>${r.name}</span><span class="muted">${r.title_id} · ${r.title_type}</span>`;
      el.appendChild(li);
    });
  }
  runSearch().catch(() => {});
  $("pick-library").onclick = async () => {
    const d = await pickDir("Library folder");
    if (d) $("library-root").value = d;
  };
  $("scan-library").onclick = () => job("Scanning library…", async () => {
    const items = await invoke("scan_library", { root: $("library-root").value });
    const el = $("library-results");
    el.innerHTML = "";
    items.forEach((item) => {
      const li = document.createElement("li");
      li.innerHTML = `<span>${item.name}</span><span class="muted">${item.kind} ${item.title_id || ""}</span>`;
      el.appendChild(li);
    });
    log(`Found ${items.length} item(s).`);
  });
}

function bindUsb() {
  $("pick-usb").onclick = async () => {
    const d = await pickDir("USB destination");
    if (d) $("usb-dest").value = d;
  };
  document.querySelector("#view-usb .drop").addEventListener("click", async () => {
    const files = await pickMany("Copy these");
    state.usbFiles.push(...files);
    setList("usb-sources", state.usbFiles);
  });
  $("check-usb").onclick = async () => {
    try {
      const info = await invoke("volume_info", { path: $("usb-dest").value });
      $("usb-info").textContent = JSON.stringify(info, null, 2);
      if (info.is_fat32) log("Destination is FAT32 — files over 4 GiB will be rejected.");
    } catch (e) { log(String(e)); }
  };
  $("run-usb").onclick = () => job("Copying to USB…", async () => {
    const report = await invoke("copy_to_usb", {
      sources: state.usbFiles,
      dest: $("usb-dest").value,
      verify: $("usb-verify").checked,
      overwriteProtected: false,
    });
    $("usb-info").textContent = JSON.stringify(report, null, 2);
    log(`Copied ${report.copied} file(s), skipped ${report.skipped_protected} protected.`);
  });
}

function bindContent() {
  $("pick-packages").onclick = async () => {
    state.packageFiles = await pickMany("DLC / Title Updates");
    log(`${state.packageFiles.length} package(s) selected.`);
  };
  $("pick-content-root").onclick = async () => {
    const d = await pickDir("Content root");
    if (d) $("content-root").value = d;
  };
  $("run-place").onclick = () => job("Placing DLC / updates…", async () => {
    if (!$("content-root").value) throw new Error("Choose a library or USB root.");
    const el = $("content-results");
    el.innerHTML = "";
    for (const path of state.packageFiles) {
      const item = await invoke("place_content", { path, libraryRoot: $("content-root").value });
      const li = document.createElement("li");
      li.innerHTML = `<span>${item.name || path}</span><span class="muted">${item.detail}</span>`;
      el.appendChild(li);
      log(item.detail);
    }
  });
}

function bindMods() {
  $("pick-mods").onclick = async () => {
    const d = await pickDir("Mods folder");
    if (d) $("mods-root").value = d;
  };
  $("pick-mods-dest").onclick = async () => {
    const d = await pickDir("Copy mods to");
    if (d) $("mods-dest").value = d;
  };
  $("list-mods").onclick = async () => {
    try {
      const names = await invoke("list_dir", { path: $("mods-root").value });
      $("mods-list").innerHTML = names.map((n) => `<li><span>${n}</span></li>`).join("");
    } catch (e) { log(String(e)); }
  };
  $("copy-mods").onclick = () => job("Copying mods…", async () => {
    const report = await invoke("copy_to_usb", {
      sources: [$("mods-root").value],
      dest: $("mods-dest").value,
      verify: true,
      overwriteProtected: false,
    });
    log(`Copied mods (${report.copied} files).`);
  });
}

function bindDownloads() {
  $("pick-dl-dest").onclick = async () => {
    const d = await pickDir("Download folder");
    if (d) $("dl-dest").value = d;
  };
  $("run-download").onclick = () => job("Downloading…", async () => {
    const report = await invoke("download_url", {
      url: $("dl-url").value,
      destDir: $("dl-dest").value,
      expectedHash: $("dl-hash").value || null,
    });
    log(`Downloaded ${report.bytes} bytes to ${report.path}${report.resumed ? " (resumed)" : ""}`);
  });
}

function bindHomebrew() {
  $("pick-hb").onclick = async () => {
    const d = await pickDir("Homebrew folder");
    if (d) $("hb-root").value = d;
  };
  $("list-hb").onclick = async () => {
    try {
      const names = await invoke("list_dir", { path: $("hb-root").value });
      $("hb-list").innerHTML = names.map((n) => `<li><span>${n}</span></li>`).join("");
    } catch (e) { log(String(e)); }
  };
  $("open-hb").onclick = async () => {
    try { await invoke("open_path", { path: $("hb-root").value }); }
    catch (e) { log(String(e)); }
  };
}

function bindXbox() {
  $("pick-xbox-files").onclick = async () => {
    const files = await pickMany("Send to Xbox");
    state.xboxFiles.push(...files);
    setList("xbox-files", state.xboxFiles);
  };
  $("discover-xbox").onclick = () => job("Scanning LAN…", async () => {
    log("Scanning local subnet for FTP on port 21…");
    const hosts = await invoke("discover_xbox", { port: Number($("xbox-port").value || 21) });
    const el = $("xbox-hosts");
    el.innerHTML = "";
    if (!hosts.length) log("No FTP listeners found. Enter the IP from Aurora.");
    hosts.forEach((h) => {
      const li = document.createElement("li");
      li.innerHTML = `<span>${h.ip}:${h.port}</span><span class="muted">${h.banner || "FTP"}</span>`;
      li.onclick = () => { $("xbox-host").value = h.ip; };
      el.appendChild(li);
    });
  });
  $("run-send").onclick = () => job("Sending to Xbox…", async () => {
    const report = await invoke("send_to_xbox", {
      host: $("xbox-host").value,
      port: Number($("xbox-port").value || 21),
      user: $("xbox-user").value,
      pass: $("xbox-pass").value,
      dest: $("xbox-dest").value,
      paths: state.xboxFiles,
      includeLaunchIni: $("xbox-launchini").checked,
    });
    log(`Sent ${report.uploaded} file(s) to ${report.host}. Protected skips: ${report.skipped_protected}.`);
    report.warnings.forEach(log);
  });
}

function bindSettings() {
  $("save-settings").onclick = async () => {
    try {
      await invoke("save_settings", { settings: collectSettings() });
      log("Settings saved.");
    } catch (e) { log(String(e)); }
  };
  $("show-notify").onclick = async () => {
    try { await invoke("notify", { title: "Unpack", body: "Notifications are working." }); }
    catch (e) { log(String(e)); }
  };
}

function onKeys(e) {
  if (e.target.matches("input, textarea, select")) return;
  const map = { Digit1: "extract", Digit2: "convert", Digit3: "library", Digit4: "usb", Digit5: "content", Digit6: "mods", Digit7: "downloads", Digit8: "homebrew", Digit9: "xbox", Digit0: "settings" };
  if (e.ctrlKey && map[e.code]) {
    e.preventDefault();
    showView(map[e.code]);
  }
}

init();
