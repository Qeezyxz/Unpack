const invoke = (...args) => window.__TAURI__.core.invoke(...args);
const listen = (...args) => window.__TAURI__.event.listen(...args);

const HOMEBREW = [
  { name: "Aurora Dashboard", cat: "Dashboards & File Explorers", blurb: "Feature-rich custom dashboard for RGH/JTAG consoles (game scanning, covers, scripts).", url: "https://consolemods.org/wiki/Xbox_360:Aurora" },
  { name: "XeXMenu 1.2", cat: "Dashboards & File Explorers", blurb: "Classic file manager / launcher used as a backup dashboard on modded consoles.", url: "https://consolemods.org/wiki/Xbox_360:XeXMenu" },
  { name: "DashLaunch", cat: "Dashboards & File Explorers", blurb: "Configures launch.ini (default dashboard, plugins, fans) on modded consoles.", url: "https://consolemods.org/wiki/Xbox_360:DashLaunch" },
  { name: "XE Unshackle", cat: "Dashboards & File Explorers", blurb: "Temporary HV/kernel patch loader for BadUpdate; loads DashLaunch from USB.", url: "https://github.com/Byrom90/XeUnshackle" },
  { name: "Horizon", cat: "PC Utilities", blurb: "PC content manager for Xbox 360 profiles, saves, and device browsing.", url: "https://www.horizonmb.com/" },
  { name: "Modio", cat: "PC Utilities", blurb: "PC all-in-one for browsing Xbox 360 USB/HDD content and editing packages.", url: "https://github.com/bboxy/modio" },
  { name: "Le Fluffie", cat: "PC Utilities", blurb: "Open-source PC tool for profiles, achievements, and package security (rehash/resign).", url: "https://consolemods.org/wiki/Xbox_360:Game_Saves" },
  { name: "Xbox 360 Hash Block Calculator", cat: "PC Utilities", blurb: "Recalculates STFS hash blocks after editing a package or save.", url: "https://consolemods.org/wiki/Xbox_360:STFS" },
  { name: "USBXTAFGUI", cat: "PC Utilities", blurb: "Browse, extract, and inject files on Xbox 360-formatted USB drives.", url: "https://github.com/Swizzy/USBXTAFGUI" },
  { name: "RDR Gamesave Editor", cat: "Save Editors", blurb: "Edit Red Dead Redemption Xbox 360 save games offline.", url: "https://consolemods.org/wiki/Xbox_360:Game_Saves" },
  { name: "Saints Row save editor", cat: "Save Editors", blurb: "Edit Saints Row Xbox 360 save games offline.", url: "https://www.saintsrowmods.com/" },
  { name: "WillowTree (Borderlands)", cat: "Save Editors", blurb: "Borderlands save editor for Xbox 360 (characters, items, stats).", url: "https://github.com/mattweidner/WillowTreeSharp" },
];

const state = {
  packFiles: [],
  xboxFiles: [],
  packageFiles: [],
  modFiles: [],
  settings: {},
  packOut: "",
};

const $ = (id) => document.getElementById(id);
let currentView = "extract";
const runningJobs = [];
const jobWaiters = [];
let jobSeq = 0;

function native() {
  return Boolean(window.__TAURI__?.core?.invoke);
}

function log(line) {
  $("job-status").textContent = line;
}

function setPresence(view, job) {
  currentView = view;
  if (!native()) return;
  invoke("discord_set_activity", { view, job: job || null }).catch(() => {});
}

async function pickMany(title, extensions) {
  if (!native()) return [];
  return ((await invoke("pick_files", { title, extensions })) || []).filter(Boolean);
}
async function pickDir(title) {
  if (!native()) return null;
  return invoke("pick_folder", { title });
}

async function remember() {
  if (!native()) return;
  try { await invoke("save_settings", { settings: collectSettings() }); } catch (_) { /* ignore */ }
}

function maxJobs() {
  const n = Number($("max-jobs")?.value || 2);
  return Math.min(4, Math.max(1, Number.isFinite(n) ? n : 2));
}

function showJobs() {
  const bar = $("job-progress");
  if (!runningJobs.length) {
    bar.hidden = true;
    setPresence(currentView, null);
    return;
  }
  bar.hidden = false;
  const labels = runningJobs.map((j) => j.label);
  const extra = jobWaiters.length ? ` · ${jobWaiters.length} waiting` : "";
  log(`${labels.join(" · ")}${extra}`);
  setPresence(currentView, labels[labels.length - 1]);
}

async function job(label, fn) {
  if (runningJobs.length >= maxJobs()) {
    log(`${label} waiting (${runningJobs.length} already running)…`);
    await new Promise((resolve) => jobWaiters.push(resolve));
  }
  const id = ++jobSeq;
  runningJobs.push({ id, label });
  showJobs();
  try {
    await fn();
    await remember();
  } catch (e) {
    log(String(e));
  } finally {
    const i = runningJobs.findIndex((j) => j.id === id);
    if (i >= 0) runningJobs.splice(i, 1);
    const next = jobWaiters.shift();
    if (next) next();
    showJobs();
  }
}

function isIso(path) {
  return /\.(iso|000)$/i.test(path);
}

function openModal(name) {
  closeModal();
  $("overlay").classList.remove("hidden");
  $(`modal-${name}`).classList.remove("hidden");
  const view = { library: "library", mods: "mods", homebrew: "homebrew", xbox: "xbox", dlc: "content", archive: "extract", settings: "settings" }[name] || "extract";
  setPresence(view, null);
}

function closeModal() {
  $("overlay").classList.add("hidden");
  document.querySelectorAll(".modal").forEach((el) => el.classList.add("hidden"));
  setPresence("extract", null);
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
    xboxName: $("xbox-name").value,
    extractDest: $("extract-dest").value,
    contentRoot: $("content-root").value,
    theme: $("theme-select").value,
    discordPresence: $("discord-presence").checked,
    autoIso: $("auto-iso").value,
    extractAuto: $("extract-auto").checked,
    skipJunk: $("extract-junk").checked,
    extractNested: $("extract-nested").checked,
    existing: $("extract-existing").value,
    maxJobs: maxJobs(),
  };
}

function applySettings(s) {
  if (!s) return;
  const set = (id, v) => { if (v != null && $(id)) $(id).value = v; };
  set("library-root", s.libraryRoot);
  set("usb-dest", s.usbRoot);
  set("dl-dest", s.downloadDir);
  set("xbox-host", s.xboxHost);
  if (s.xboxPort) $("xbox-port").value = s.xboxPort;
  set("xbox-user", s.xboxUser);
  set("xbox-pass", s.xboxPass);
  set("xbox-name", s.xboxName);
  set("extract-dest", s.extractDest);
  set("content-root", s.contentRoot);
  if (s.autoIso) $("auto-iso").value = s.autoIso;
  if (typeof s.extractAuto === "boolean") $("extract-auto").checked = s.extractAuto;
  if (typeof s.skipJunk === "boolean") $("extract-junk").checked = s.skipJunk;
  if (typeof s.extractNested === "boolean") $("extract-nested").checked = s.extractNested;
  if (s.existing) $("extract-existing").value = s.existing;
  if (s.maxJobs) $("max-jobs").value = String(Math.min(4, Math.max(1, Number(s.maxJobs) || 2)));
  syncIsoBadge();
}

function syncIsoBadge() {
  const mode = $("auto-iso").value;
  const badge = $("auto-badge");
  if (mode === "off") {
    badge.textContent = "ISO OFF";
    badge.style.color = "var(--muted)";
  } else {
    badge.textContent = `AUTO ${mode.toUpperCase()}`;
    badge.style.color = "var(--green)";
  }
}

function setList(id, paths) {
  const el = $(id);
  el.innerHTML = "";
  paths.forEach((p, i) => {
    const li = document.createElement("li");
    li.innerHTML = `<span>${p}</span><button type="button">remove</button>`;
    li.querySelector("button").onclick = () => {
      paths.splice(i, 1);
      setList(id, paths);
    };
    el.appendChild(li);
  });
}

async function convertIsos(isos, dest) {
  if (!isos?.length || $("auto-iso").value === "off") return;
  await job("Converting ISO…", async () => {
    const reports = await invoke("convert_iso", {
      paths: isos,
      dest,
      mode: $("auto-iso").value,
      trim: true,
      verify: true,
      autoCompanions: true,
    });
    const list = Array.isArray(reports) ? reports : [reports];
    for (const report of list) {
      const discs = report.merged ? ` (${report.discs?.length || 0} discs)` : "";
      log(`Converted ${report.info?.name || "ISO"}${discs} → ${report.output}`);
    }
  });
}

async function handlePaths(paths) {
  if (!paths?.length) return;
  const dest = $("extract-dest").value.trim();
  const isos = paths.filter(isIso);
  const archives = paths.filter((p) => !isIso(p));
  const tasks = [];
  if (archives.length) {
    setList("recent-files", archives.slice());
    if ($("extract-auto").checked) {
      if (!dest) {
        log("Choose a destination folder first.");
        return;
      }
      tasks.push((async () => {
        let found = [];
        await job("Extracting…", async () => {
          const result = await invoke("extract_archives", {
            archives,
            dest,
            skipJunk: $("extract-junk").checked,
            existing: $("extract-existing").value,
            nested: $("extract-nested").checked,
          });
          const inner = result.reduce((n, r) => n + (r.nested || 0), 0);
          log(inner ? `Extracted ${result.length} archive(s) and opened ${inner} inside.` : `Extracted ${result.length} archive(s).`);
          await invoke("notify", { title: "Unpack", body: `Extracted ${result.length} archive(s)` });
          found = result.flatMap((r) => r.isos || []);
        });
        if (found.length) await convertIsos(found, dest);
      })());
    }
  }
  if (isos.length && $("auto-iso").value !== "off") {
    if (!dest) {
      log("Choose a destination folder first.");
      return;
    }
    tasks.push(convertIsos(isos, dest));
  }
  await Promise.all(tasks);
}

function renderHomebrew() {
  const q = ($("hb-search").value || "").toLowerCase();
  const cat = $("hb-cat").value;
  const el = $("hb-cards");
  el.innerHTML = "";
  HOMEBREW.filter((t) => (!cat || t.cat === cat) && (!q || `${t.name} ${t.blurb} ${t.cat}`.toLowerCase().includes(q)))
    .forEach((t) => {
      const card = document.createElement("article");
      card.className = "card";
      const save = t.cat === "Save Editors";
      card.innerHTML = `<div><h3>${t.name}</h3><span class="tag${save ? " save" : ""}">${t.cat}</span></div><p>${t.blurb}</p><button type="button">Open source page</button>`;
      card.querySelector("button").onclick = async () => {
        try { await invoke("open_path", { path: t.url }); }
        catch (e) { log(String(e)); window.open(t.url, "_blank"); }
      };
      el.appendChild(card);
    });
}

async function refreshLibrary() {
  const root = $("library-root").value.trim();
  const empty = $("library-empty");
  const list = $("library-results");
  list.innerHTML = "";
  if (!root) {
    empty.classList.remove("hidden");
    return;
  }
  try {
    const items = await invoke("scan_library", { root });
    if (!items.length) {
      empty.classList.remove("hidden");
      return;
    }
    empty.classList.add("hidden");
    items.forEach((item) => {
      const li = document.createElement("li");
      li.innerHTML = `<span>${item.name}</span><span class="muted">${item.kind} ${item.title_id || ""}</span>`;
      li.onclick = () => {
        state.xboxFiles = [item.path].filter(Boolean);
        if (state.xboxFiles.length) {
          setList("xbox-files", state.xboxFiles);
          openModal("xbox");
        }
      };
      list.appendChild(li);
    });
    log(`Found ${items.length} item(s).`);
  } catch (e) {
    empty.classList.remove("hidden");
    log(String(e));
  }
}

async function init() {
  document.querySelectorAll("[data-modal]").forEach((b) => {
    b.addEventListener("click", (e) => {
      e.preventDefault();
      openModal(b.dataset.modal);
      if (b.dataset.modal === "library") refreshLibrary();
      if (b.dataset.modal === "homebrew") renderHomebrew();
    });
  });
  document.querySelectorAll(".close-modal").forEach((b) => b.addEventListener("click", closeModal));
  $("overlay").onclick = closeModal;

  $("theme-select").value = localStorage.getItem("unpack.theme") || "dark";
  $("theme-select").onchange = () => {
    localStorage.setItem("unpack.theme", $("theme-select").value);
    window.__applyTheme();
  };
  $("auto-iso").onchange = syncIsoBadge;
  $("iso-mode-btn").onclick = () => {
    const order = ["god", "xex", "off"];
    const i = order.indexOf($("auto-iso").value);
    $("auto-iso").value = order[(i + 1) % order.length];
    syncIsoBadge();
  };

  const cats = [...new Set(HOMEBREW.map((t) => t.cat))];
  cats.forEach((c) => {
    const o = document.createElement("option");
    o.value = c;
    o.textContent = c;
    $("hb-cat").appendChild(o);
  });
  $("hb-search").oninput = renderHomebrew;
  $("hb-cat").onchange = renderHomebrew;
  renderHomebrew();

  document.querySelectorAll(".tab").forEach((tab) => {
    tab.onclick = () => {
      tab.parentElement.querySelectorAll(".tab").forEach((t) => t.classList.remove("on"));
      tab.classList.add("on");
      ["mods-files", "mods-dl", "mods-names"].forEach((id) => $(id).classList.toggle("hidden", tab.dataset.tab !== id));
    };
  });

  try {
    if (native()) {
      const meta = await invoke("app_meta");
      $("settings-meta").innerHTML = `
        <dt>Version</dt><dd>${meta.version}</dd>
        <dt>TitleIDs</dt><dd>${meta.title_count.toLocaleString()}</dd>
        <dt>7-Zip</dt><dd>${meta.seven_zip || "not found (put 7za.exe next to Unpack.exe)"}</dd>
        <dt>Config</dt><dd>${meta.config_dir}</dd>`;
      state.settings = await invoke("load_settings");
      applySettings(state.settings);
      const presenceOn = state.settings?.discordPresence !== false;
      $("discord-presence").checked = presenceOn;
      await invoke("discord_set_enabled", { enabled: presenceOn }).catch(() => {});
      setPresence("extract", null);
    }
  } catch (err) {
    log(`Open via Unpack.exe / npx tauri dev (${err}).`);
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
        if (event.payload.type === "drop") handlePaths(event.payload.paths || []);
      });
    } catch (_) { /* older webview */ }
  }

  const drop = $("drop-zone");
  drop.addEventListener("click", async () => {
    const files = await pickMany("Archives", ["zip", "rar", "7z", "tar", "gz", "xz", "iso"]);
    await handlePaths(files);
  });
  drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("over");
  });

  $("pick-extract-dest").onclick = async () => {
    const d = await pickDir("Extract to");
    if (d) $("extract-dest").value = d;
  };
  $("sample-zip").onclick = () => job("Creating sample ZIP…", async () => {
    const dest = $("extract-dest").value.trim();
    if (!dest) throw new Error("Choose a destination folder first.");
    const inputs = await pickMany("Pick any small file to pack as a sample");
    if (!inputs.length) throw new Error("Pick a file to wrap in a sample ZIP, or drop a real archive.");
    const out = `${dest.replace(/[\\/]+$/, "")}/Unpack-sample.zip`;
    await invoke("create_archive", { inputs, dest: out });
    await handlePaths([out]);
  });

  $("pick-library").onclick = async () => {
    const d = await pickDir("Game library folder");
    if (d) $("library-root").value = d;
  };
  $("scan-library").onclick = () => job("Scanning library…", refreshLibrary);

  $("pick-mod-files").onclick = async () => {
    const files = await pickMany("Mod files");
    state.modFiles.push(...files);
    $("mods-empty").classList.toggle("hidden", state.modFiles.length > 0);
    setList("mods-list", state.modFiles);
  };
  $("pick-mod-folder").onclick = async () => {
    const d = await pickDir("Mods folder");
    if (d) {
      state.modFiles.push(d);
      $("mods-empty").classList.toggle("hidden", true);
      setList("mods-list", state.modFiles);
    }
  };
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
    log(`Downloaded ${report.bytes} bytes to ${report.path}`);
  });

  let searchTimer;
  $("title-search").oninput = () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(async () => {
      try {
        const rows = await invoke("search_titles", { query: $("title-search").value, limit: 80 });
        const el = $("title-results");
        el.innerHTML = "";
        rows.forEach((r) => {
          const li = document.createElement("li");
          li.innerHTML = `<span>${r.name}</span><span>${r.title_id}</span>`;
          el.appendChild(li);
        });
      } catch (e) { log(String(e)); }
    }, 160);
  };

  $("pick-xbox-files").onclick = async () => {
    const files = await pickMany("Send to Xbox");
    state.xboxFiles.push(...files);
    setList("xbox-files", state.xboxFiles);
  };
  $("pick-xbox-folder").onclick = async () => {
    const d = await pickDir("Folder to send");
    if (d) {
      state.xboxFiles.push(d);
      setList("xbox-files", state.xboxFiles);
    }
  };
  $("xbox-from-library").onclick = () => openModal("library");
  $("xbox-test").onclick = () => job("Connecting…", async () => {
    const msg = await invoke("xbox_test", {
      host: $("xbox-host").value,
      port: Number($("xbox-port").value || 21),
      user: $("xbox-user").value,
      pass: $("xbox-pass").value,
    });
    log(msg);
  });
  $("xbox-save").onclick = remember;
  $("run-send").onclick = () => job("Sending to Xbox…", async () => {
    const report = await invoke("send_to_xbox", {
      host: $("xbox-host").value,
      port: Number($("xbox-port").value || 21),
      user: $("xbox-user").value,
      pass: $("xbox-pass").value,
      dest: "/Hdd1/Games",
      paths: state.xboxFiles,
      includeLaunchIni: $("xbox-launchini").checked,
    });
    const extra = report.resumed ? ` ${report.resumed} picked up after a drop.` : "";
    log(`Sent ${report.uploaded} file(s) to ${report.host}.${extra}`);
    (report.warnings || []).forEach(log);
  });

  $("pick-archive-inputs").onclick = async () => {
    state.packFiles = await pickMany("Files to pack");
    setList("pack-files", state.packFiles);
  };
  $("pick-archive-out").onclick = async () => {
    state.packOut = await invoke("pick_save", { title: "Save archive", defaultName: "archive.7z" });
    if (state.packOut) $("pack-out").textContent = state.packOut;
  };
  $("run-pack").onclick = () => job("Creating archive…", async () => {
    if (!state.packFiles.length || !state.packOut) throw new Error("Choose files and an archive name.");
    const result = await invoke("create_archive", { inputs: state.packFiles, dest: state.packOut });
    log(`Created ${result.archive}`);
  });

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
      li.innerHTML = `<span>${item.name || path}</span><span>${item.detail}</span>`;
      el.appendChild(li);
    }
  });

  $("max-jobs").onchange = remember;
  $("extract-nested").onchange = remember;
  $("discord-presence").onchange = async () => {
    if (!native()) return;
    const enabled = $("discord-presence").checked;
    await invoke("discord_set_enabled", { enabled }).catch(() => {});
    await remember();
  };
  $("save-settings").onclick = remember;

  document.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "o") {
      e.preventDefault();
      $("drop-zone").click();
    }
    if (e.key === "Escape") closeModal();
  });

  syncIsoBadge();
  const preview = new URLSearchParams(location.search).get("modal");
  if (preview) {
    openModal(preview);
    if (preview === "library") refreshLibrary();
    if (preview === "homebrew") renderHomebrew();
  }
}

init();
