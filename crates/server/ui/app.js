"use strict";

/* ===== constants ===== */
// Category names come from TRACKER_CATEGORIES in .env (via /api/summary).
// Without it the category panel and tags stay hidden.
let CATS = {};
const catsOn = () => Object.keys(CATS).length > 0;
const hue = id => (Number(id) * 137) % 360;
function setCats(list) {
  CATS = Object.fromEntries((list ?? []).map(c => [c.id, c.name]));
  $("#cats").hidden = !catsOn();
}
const MARKS = {
  viewed:         {label:"Viewed",         key:"1", c:"#B7ABC4",        hint:"Seen it, skip next time"},
  view_later:     {label:"View later",     key:"2", c:"var(--watch)",   hint:"Look at it properly later"},
  download_later: {label:"Download later", key:"3", c:"var(--get)",     hint:"Grab the .torrent later"},
  downloaded:     {label:"Downloaded",     key:"4", c:"var(--got)",     hint:"Already have it"},
};
const VIEWS = [
  ["new","New","var(--new)"],["view_later","View later","var(--watch)"],["download_later","Download later","var(--get)"],
  ["downloaded","Downloaded","var(--got)"],["viewed","Viewed","#B7ABC4"],["all","Everything","var(--faint)"],
];
const DAY = 86400, ICT = 7 * 3600;
const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;

/* ===== state ===== */
function loadCats() {
  try {
    const saved = JSON.parse(localStorage.getItem("bbt_cats"));
    if (Array.isArray(saved)) return new Set(saved.map(Number).filter(c => c in CATS));
  } catch { /* fall through */ }
  return new Set(Object.keys(CATS).map(Number));
}
function saveCats() {
  const categories = [...S.cats];
  localStorage.setItem("bbt_cats", JSON.stringify(categories));
  api("PUT", "/api/prefs", {categories}).catch(e => toast(`Couldn't save categories: ${e.message}`, false));
}
const S = {
  view:"new", cats:loadCats(), range:"0", from:"", sort:"new", q:"",
  grid:false, free:false, sticky:localStorage.getItem("bbt_sticky")==="1",
  items:[], summary:null, lastVisit:null, focus:null, sel:new Set(), undo:null, drawer:null,
  syncing:false, loadSeq:0,
};
const $ = s => document.querySelector(s);
const touch = () => matchMedia("(pointer: coarse)").matches;
const main = $("#main");

/* ===== api ===== */
async function api(method, path, body) {
  const res = await fetch(path, {method, headers: body ? {"content-type":"application/json"} : {}, body: body ? JSON.stringify(body) : undefined});
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `${res.status} ${res.statusText}`);
  return data;
}

/* ===== time ===== */
const nowSec = () => Math.floor(Date.now() / 1000);
const startOfDayIct = t => Math.floor((t + ICT) / DAY) * DAY - ICT;
function cutoff() {
  if (S.range === "last") return S.lastVisit ?? nowSec() - DAY;
  if (S.range === "custom") return S.from ? Math.floor(Date.parse(S.from + "T00:00:00+07:00") / 1000) : startOfDayIct(nowSec());
  return startOfDayIct(nowSec()) - Number(S.range) * DAY;
}
const dayKey = t => new Date((t + ICT) * 1000).toISOString().slice(0, 10);
function dayLabel(k) {
  const today = dayKey(nowSec()), yest = dayKey(nowSec() - DAY);
  if (k === today) return "Today";
  if (k === yest) return "Yesterday";
  return new Date(k + "T12:00:00+07:00").toLocaleDateString("en-GB", {weekday:"short", day:"numeric", month:"short"});
}
const fmtTime = t => new Date(t * 1000).toLocaleTimeString("en-GB", {hour:"2-digit", minute:"2-digit", timeZone:"Asia/Bangkok"});
const fmtWhen = t => new Date(t * 1000).toLocaleString("en-GB", {day:"numeric", month:"short", hour:"2-digit", minute:"2-digit", timeZone:"Asia/Bangkok"});
function fmtAgo(t) {
  const m = (nowSec() - t) / 60;
  return m < 60 ? Math.max(1, Math.round(m)) + " min ago" : m < 1440 ? Math.round(m / 60) + " h ago" : Math.round(m / 1440) + " d ago";
}
function fmtSize(b) {
  const gb = b / 1073741824;
  return gb >= 1 ? gb.toFixed(2) + " GB" : (b / 1048576).toFixed(0) + " MB";
}

/* ===== text ===== */
const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const siteUrl = it => S.summary?.details_url?.replace("{id}", it.id).replace("{hash_id}", it.hash_id) ?? null;
const clean = t => t.replace(/\s*@[A-Za-z]+\s*$/, ""); // drop the site tag at the end
const code = it => (it.title.match(/^[A-Z0-9]{2,}-\d+/) || [clean(it.title).split(/\s+/).slice(0, 3).join(" ")])[0];
const grad = it => `linear-gradient(160deg,hsl(${hue(it.category)} 45% 38%),hsl(${(hue(it.category) + 40) % 360} 35% 18%))`;
function poster(it) {
  const img = it.poster_url ? `<img src="${esc(it.poster_url)}" alt="" loading="lazy" referrerpolicy="no-referrer" onerror="this.remove()">` : "";
  return `<div class="poster" style="--g:${grad(it)}"><span>${esc(code(it))}</span>${img}</div>`;
}

// Only New and Everything are filtered by date ("From"); the sorted lists hold everything.
const dated = () => S.view === "new" || S.view === "all";
function rangeText() {
  if (S.range === "last") return S.lastVisit ? "since your last visit" : "in the last day";
  if (S.range === "custom") return S.from ? `since ${new Date(S.from + "T12:00:00+07:00").toLocaleDateString("en-GB", {day:"numeric", month:"short"})}` : "today";
  return {"0":"today", "2":"in the last 3 days", "6":"in the last 7 days"}[S.range] ?? "in this date range";
}
function syncRange() {
  const on = dated();
  $("#range").disabled = !on; $("#from").disabled = !on;
  $("#rangeField").title = on ? "" : "From only filters New and Everything";
}

/* ===== loading ===== */
function listQuery() {
  const p = new URLSearchParams({view: S.view, sort: S.sort, limit: "5000"});
  if (S.view === "new" || S.view === "all") p.set("from", cutoff());
  if (catsOn() && S.cats.size !== Object.keys(CATS).length) p.set("cats", [...S.cats].join(",") || "0");
  if (S.q) p.set("q", S.q);
  if (S.free) p.set("free", "true");
  if (S.sticky) p.set("sticky", "true");
  return p;
}
async function load() {
  const seq = ++S.loadSeq;
  try {
    const [sum, list] = await Promise.all([api("GET", `/api/summary?from=${cutoff()}`), api("GET", `/api/torrents?${listQuery()}`)]);
    if (seq !== S.loadSeq) return;
    S.summary = sum; S.items = list.torrents;
    banner(null);
    render();
  } catch (e) {
    banner(`Can't load torrents: ${e.message}. Is bbt-server running?`);
  }
}
async function refreshSummary() {
  try { S.summary = await api("GET", `/api/summary?from=${cutoff()}`); renderNav(); renderBins(); renderSyncText(); } catch { /* list still usable */ }
}
function banner(msg) {
  let b = $(".banner");
  if (!msg) { b?.remove(); return; }
  if (!b) { b = document.createElement("div"); b.className = "banner"; b.setAttribute("role", "alert"); main.prepend(b); }
  b.textContent = msg;
}

/* ===== grouping ===== */
let flat = [];
function groups() {
  if (S.sort !== "new") return [{key:"all", label:VIEWS.find(v => v[0] === S.view)[1], items:S.items}];
  const m = new Map();
  for (const it of S.items) { const k = dayKey(it.uploaded_at); if (!m.has(k)) m.set(k, []); m.get(k).push(it); }
  return [...m].map(([k, items]) => ({key:k, label:dayLabel(k), items}));
}

/* ===== render ===== */
function tags(it) {
  const mark = S.view === "all" && it.mark ? `<span class="tag state" style="--state:${MARKS[it.mark].c}">${MARKS[it.mark].label}</span>` : "";
  return `${catsOn() ? `<span class="tag">${esc(CATS[it.category] ?? `Category ${it.category}`)}</span>` : ""}${it.sticky ? '<span class="tag sticky">Sticky</span>' : ""}${it.free ? '<span class="tag free">Free</span>' : ""}${it.multiplier ? `<span class="tag free">${esc(it.multiplier)} up</span>` : ""}${mark}${it.rating ? `<span class="stars" title="Your rating: ${it.rating} of 5">★${it.rating}</span>` : ""}<span>${esc(it.uploader ?? "")}</span>`;
}
const stateOf = it => it.mark ?? "new";
const stateAttr = it => ({viewed:"viewed", view_later:"watch", download_later:"get", downloaded:"got", new:"new"})[stateOf(it)];
// One number column, and it follows the sort: seeders by default, else what you sorted by.
const STATS = {seed:["Seeds", "seeders"], leech:["Leech", "leechers"], snatch:["Snatched", "snatched"]};
const stat = () => STATS[S.sort] ?? STATS.seed;
function rowHTML(it, byDay) {
  const [, key] = stat();
  return `<div class="row${S.focus === it.id ? " focus" : ""}" data-id="${it.id}" data-state="${stateAttr(it)}" role="option" aria-selected="${S.focus === it.id}">
  <i class="bar"></i><input class="sel" type="checkbox" aria-label="Select" ${S.sel.has(it.id) ? "checked" : ""}>
  ${poster(it)}
  <div class="meta"><div class="title" title="${esc(it.title)}">${esc(clean(it.title))}</div><div class="tags">${tags(it)}</div></div>
  <div class="num">${fmtSize(it.size_bytes)}<small>${it.files} file${it.files === 1 ? "" : "s"}</small></div>
  <div class="num c-stat${key === "seeders" && it.seeders > 200 ? " hot" : ""}">${it[key]}</div>
  <div class="time c-time">${byDay ? fmtTime(it.uploaded_at) : fmtAgo(it.uploaded_at)}</div></div>`;
}
function cardHTML(it) {
  return `<div class="card${S.focus === it.id ? " focus" : ""}" data-id="${it.id}" data-state="${stateAttr(it)}" role="option">
  ${poster(it)}<i class="flag"></i><input class="sel" type="checkbox" aria-label="Select" ${S.sel.has(it.id) ? "checked" : ""}>
  <div class="title">${esc(clean(it.title))}</div><div class="tags">${it.rating ? `<span class="stars">★${it.rating}</span>` : ""}<span>${fmtSize(it.size_bytes)}</span><span>${it.seeders} seeding</span></div></div>`;
}
let shown = [];
function render() {
  const gs = groups(); shown = gs; flat = gs.flatMap(g => g.items);
  if (S.focus && !flat.some(i => i.id === S.focus)) S.focus = null;
  if (!S.focus && flat.length) S.focus = flat[0].id;
  const byDay = S.sort === "new";
  const keepBanner = $(".banner");
  if (!flat.length) {
    const total = S.summary?.counts.total ?? 0;
    const label = VIEWS.find(v => v[0] === S.view)[1];
    let msg;
    if (catsOn() && !S.cats.size) msg = `<h3>No categories selected</h3><p>Tick at least one category on the left to see torrents.</p><button data-act="allcats">Select all categories</button>`;
    else if (S.q || S.free) msg = `<h3>No matches</h3><p>Nothing in ${label}${S.q ? ` matches “${esc(S.q)}”` : ""}${S.free ? `${S.q ? " and is" : " is"} free` : ""}${dated() ? ` ${rangeText()}` : ""}.</p><button data-act="clearfilters">Clear ${S.q && S.free ? "search and Free only" : S.q ? "search" : "Free only"}</button>`;
    else if (!total) msg = `<h3>No torrents yet</h3><p>Sync to fetch the latest uploads from tracker.</p><button data-act="sync">Sync now</button>`;
    else if (S.view === "new") msg = `<h3>All caught up</h3>${tallyHTML()}<p>Nothing new ${rangeText()}. Sync to check the tracker for new uploads.</p><button data-act="sync">Sync now</button>`;
    else msg = `<h3>Nothing in ${label}</h3><p>${MARKS[S.view] ? `Press ${MARKS[S.view].key} on a torrent to add it here.` : "Try a wider date range."}</p>`;
    main.innerHTML = `<div class="empty">${msg}</div>`;
  } else {
    const total = S.summary?.counts.total ?? 0;
    const head = S.grid ? "" : `<div class="colhead"><span></span><span></span><span></span><span class="l">${S.view === "all" && total > flat.length && flat.length < 5000 ? `${flat.length} uploaded ${rangeText()}, of ${total} in bbt` : plural(flat.length, "torrent") + (flat.length >= 5000 ? " (first 5000)" : "")}</span><span>Size</span><span class="c-stat">${stat()[0]}</span><span class="c-time">${byDay ? "Time" : "Age"}</span></div>`;
    main.classList.toggle("has-colhead", !S.grid);
    main.classList.toggle("mixed", S.view === "all"); // the only list where states differ
    main.innerHTML = head + gs.map(g => {
      const nNew = g.items.filter(i => !i.mark).length;
      return `<section class="group" data-g="${g.key}"><div class="ghead"><h3>${esc(g.label)}</h3>
      <span class="sub">${plural(g.items.length, "torrent")}${nNew && S.view !== "new" ? `, <b>${nNew} new</b>` : ""}</span>
      <span class="act">${bulkable() && nNew ? bulkButton(g.key, nNew) : ""}</span></div>
      ${S.grid ? `<div class="cards" role="listbox">${g.items.map(cardHTML).join("")}</div>` : `<div role="listbox">${g.items.map(it => rowHTML(it, byDay)).join("")}</div>`}</section>`;
    }).join("");
  }
  const oldest = S.summary?.coverage_from;
  if (S.view === "all" && S.sort === "new" && S.summary?.counts.total) {
    main.insertAdjacentHTML("beforeend", olderHTML());
    const el = $("#older"); if (el) olderObserver.observe(el);
  } else if (oldest && S.view === "new" && cutoff() < oldest - 3600) {
    main.insertAdjacentHTML("beforeend", `<p class="coverage">bbt has every upload back to ${fmtWhen(oldest)}. Earlier ones aren't loaded yet, so this list can't go further back. Open Everything and scroll to the end to load older pages.</p>`);
  }
  if (keepBanner) main.prepend(keepBanner);
  renderNav(); renderBins(); renderTarget(); renderSyncText(); syncRange();
  if (S.drawer) renderDrawer();
}
function renderNav() {
  const c = S.summary?.counts;
  const n = k => !c ? "" : k === "new" ? c.new : k === "all" ? c.total : c[k];
  const [, label, col] = VIEWS.find(v => v[0] === S.view);
  $("#railBtn").innerHTML = `<i class="dot" style="background:${col}"></i>${label}<span class="n">${n(S.view)}</span>`;
  $("#views").innerHTML = "<h2>Lists</h2>" + VIEWS.map(([k, l, col]) => `<button data-view="${k}" aria-current="${S.view === k}" style="--c:${col}"><i class="dot"></i>${l}<span class="n">${n(k)}</span></button>`).join("");
  const byCat = c?.new_by_category ?? {};
  const all = S.cats.size === Object.keys(CATS).length, none = S.cats.size === 0;
  $("#cats").innerHTML = `<div class="cats-head"><h2>Categories</h2><button data-cats="all" ${all ? "disabled" : ""}>All</button><button data-cats="none" ${none ? "disabled" : ""}>None</button></div>` + Object.entries(CATS).map(([k, l]) => `<label><input type="checkbox" data-cat="${k}" ${S.cats.has(+k) ? "checked" : ""}><span>${esc(l)}</span><span class="n">${byCat[k] || ""}</span></label>`).join("");
}
// On touch the first row is focused but unseen, so the bins wait for a tap on a torrent.
const blind = () => touch() && !S.picked;
function pick() { if (!S.picked) { S.picked = true; renderBins(); } }
function renderBins() {
  const c = S.summary?.counts ?? {};
  $("#bins").innerHTML = Object.entries(MARKS).map(([k, m]) => `<button class="bin" data-s="${{viewed:"viewed", view_later:"watch", download_later:"get", downloaded:"got"}[k]}" data-mark="${k}" ${blind() ? "disabled" : ""}><kbd>${m.key}</kbd><span class="lbl">${m.label}<small>${m.hint}</small></span><span class="count">${c[k] ?? 0}</span></button>`).join("");
}
function renderTarget() {
  const t = $("#target"), it = flat.find(i => i.id === S.focus);
  const what = S.sel.size ? `${S.sel.size} selected` : it ? esc(code(it)) : "—";
  t.classList.toggle("rating", !!rateArmed);
  t.innerHTML = rateArmed ? `${S.sel.size ? `Rate ${S.sel.size} selected` : "Rate it"}<b>1–5, or 0 to clear</b>` : blind() && flat.length ? `Applies to<b>Tap a torrent first</b>` : `Applies to<b>${what}</b>`;
}
function renderSyncText() {
  if (S.syncing) return;
  const ls = S.summary?.last_sync;
  $("#syncText").textContent = !ls ? "Not synced yet"
    : `Synced ${fmtWhen(ls.finished_at)}, ${ls.new_count} new${/page cap/.test(ls.stop_reason ?? "") ? ". Older uploads were skipped." : /error/.test(ls.stop_reason ?? "") ? ". Last sync failed." : ""}`;
  $("#sLast").textContent = ls ? `${fmtWhen(ls.finished_at)}: ${ls.pages} page(s), ${ls.new_count} new, ${ls.stop_reason}` : "Never";
}

/* ===== drawer ===== */
// Screenshots come from the torrent's details page: a tracker request unless bbt has
// them cached. So they load only on a deliberate open, or once focus rests on a torrent.
const detailsCache = new Map(), detailsBusy = new Map(); // id -> "load" | "tracker"
const FOLLOW_MS = 600, SLOW_MS = 400;
let followT;
function shotsNote(id) {
  const busy = detailsBusy.get(id);
  if (busy === "tracker") return `<p class="note fetching">Fetching screenshots from the tracker…</p>`;
  if (busy === "load") return `<p class="note">Loading screenshots…</p>`;
  return `<p class="note">Screenshots load when you stay on this torrent.</p>`;
}
function renderDrawer() {
  const it = S.items.find(i => i.id === S.drawer);
  if (!it) return closeDrawer();
  const d = detailsCache.get(it.id);
  const shots = !d ? shotsNote(it.id)
    : d.error ? `<p class="err" title="${esc(d.error)}">Couldn't get screenshots from the tracker${/disabled/.test(d.error) ? " because tracker access is off (check .env)" : /not in the database/.test(d.error) ? " because this torrent is no longer in bbt" : ""}. <button class="linkbtn" data-act="retry">Try again</button></p>`
    : d.images.length ? `<div class="shots">${d.images.map((u, i) => `<a href="${esc(u)}" data-shot="${i}" aria-label="View screenshot ${i + 1} of ${d.images.length}"><img src="${esc(u)}" alt="" loading="lazy" referrerpolicy="no-referrer"></a>`).join("")}</div>`
    : `<p class="note">No screenshots on tracker for this torrent.</p>`;
  $("#drawerIn").innerHTML = `<header><span>${catsOn() ? esc(CATS[it.category] ?? `Category ${it.category}`) + " " : ""}<span style="color:var(--faint)">#${it.id}</span></span><button class="close" aria-label="Close details" data-act="close">×</button></header>
  <div class="big">${poster(it)}<div style="display:flex;flex-direction:column;gap:10px"><h4>${esc(clean(it.title))}</h4>
  <dl><dt>Uploaded</dt><dd>${fmtWhen(it.uploaded_at)}</dd><dt>By</dt><dd>${esc(it.uploader ?? "Anonymous")}</dd><dt>Size</dt><dd>${fmtSize(it.size_bytes)}, ${it.files} file${it.files === 1 ? "" : "s"}</dd></dl></div></div>
  <dl><dt>Seeding</dt><dd>${it.seeders}</dd><dt>Leeching</dt><dd>${it.leechers}</dd><dt>Snatched</dt><dd>${it.snatched}</dd><dt>Free download</dt><dd>${it.free ? esc(it.free) : "No"}</dd><dt>Counts updated</dt><dd>${fmtAgo(it.stats_at)}</dd></dl>
  ${shots}
  <div class="rate-row"><div class="rate" role="group" aria-label="Your rating">${[5, 4, 3, 2, 1].map(n => `<button data-rate="${n}" class="${it.rating >= n ? "on" : ""}" aria-pressed="${it.rating === n}" aria-label="${n} star${n === 1 ? "" : "s"}">★</button>`).join("")}</div><span class="note">${it.rating ? `${it.rating} of 5. ${touch() ? "Tap" : "Click"} it again to clear.` : touch() ? "Not rated." : "Not rated. Press r then 1–5."}</span></div>
  <div class="states">${Object.entries(MARKS).map(([k, m]) => `<button data-set="${k}" style="--c:${m.c}" aria-pressed="${it.mark === k}">${m.label}</button>`).join("")}</div>
  <div class="acts"><button class="primary" data-act="download" ${S.downloading ? "disabled" : ""}>${S.downloading ? "Downloading…" : "Download .torrent"}</button>${siteUrl(it) ? `<a href="${siteUrl(it)}" target="_blank" rel="noreferrer">Open on the tracker</a>` : ""}</div>
  <p class="note">Download fetches a fresh link from tracker and marks the torrent Downloaded.</p>`;
}
function openDrawer(id, {delay = 0} = {}) {
  S.drawer = id; $("#drawer").classList.add("open");
  clearTimeout(followT);
  if (!detailsCache.has(id) && !detailsBusy.has(id)) {
    if (delay) { followT = setTimeout(() => { if (S.drawer === id) fetchDetails(id); }, delay); }
    else fetchDetails(id);
  }
  renderDrawer();
}
async function fetchDetails(id) {
  detailsBusy.set(id, "load");
  // A cache hit answers at once; anything slower is a trip to the tracker.
  const slow = setTimeout(() => { detailsBusy.set(id, "tracker"); if (S.drawer === id) renderDrawer(); }, SLOW_MS);
  try { detailsCache.set(id, await api("GET", `/api/torrents/${id}/details`)); }
  catch (e) { detailsCache.set(id, {error: e.message}); }
  clearTimeout(slow); detailsBusy.delete(id);
  if (S.drawer === id) renderDrawer();
}
function closeDrawer() { clearTimeout(followT); S.drawer = null; $("#drawer").classList.remove("open"); }

async function download(id) {
  if (S.downloading) return;
  S.downloading = true; renderDrawer();
  try {
    const res = await fetch(`/api/torrents/${id}/download`);
    if (!res.ok) throw new Error((await res.json().catch(() => ({}))).error || res.statusText);
    const cd = res.headers.get("content-disposition") || "";
    const star = cd.match(/filename\*=UTF-8''([^;]+)/);
    const name = star ? decodeURIComponent(star[1]) : `tracker-${id}.torrent`;
    const url = URL.createObjectURL(await res.blob());
    const a = Object.assign(document.createElement("a"), {href: url, download: name});
    document.body.append(a); a.click(); a.remove(); URL.revokeObjectURL(url);
    toast(`Saved ${name}`, false);
    await setMark([id], "downloaded");
  } catch (e) {
    toast(`Download failed: ${e.message}`, false);
  } finally {
    S.downloading = false; if (S.drawer === id) renderDrawer();
  }
}

/* ===== screenshot viewer ===== */
const V = {images: [], i: 0, opener: null, full: false, dragged: false};
const viewerOpen = () => !$("#lightbox").hidden;
function showShot() {
  const n = V.images.length, url = V.images[V.i];
  $("#lbImg").src = url;
  $("#lbStage").scrollTo(0, 0);
  $("#lbOpen").href = url;
  $("#lbCount").textContent = `${V.i + 1} / ${n}`;
  $(".lb-prev").hidden = $(".lb-next").hidden = n < 2;
  for (const d of [-1, 1]) new Image().src = V.images[(V.i + d + n) % n]; // preload neighbours
}
// With Blur posters on, screenshots stay blurred until shown on purpose (v or a click).
const viewerBlurred = () => document.body.classList.contains("blur") && !$("#lightbox").classList.contains("reveal");
function revealShots(on) {
  $("#lightbox").classList.toggle("reveal", on);
  $("#lbReveal").hidden = !viewerBlurred();
  $("#lbImg").title = viewerBlurred() ? "Click to show (v)" : V.full ? "Drag to move, click to fit (f)" : "Click for full size (f)";
}
function openViewer(images, i) {
  if (!images.length) return;
  V.images = images; V.i = i; V.opener = document.activeElement;
  $("#lightbox").hidden = false; revealShots(false); showShot(); $(".lb-close").focus();
}
function closeViewer() { setFull(false); revealShots(false); $("#lightbox").hidden = true; $("#lbImg").removeAttribute("src"); V.opener?.focus?.(); }
function setFull(on) {
  V.full = on; V.dragged = false; $("#lightbox").classList.toggle("full", on);
  $("#lbSize").setAttribute("aria-pressed", on); $("#lbSize").textContent = on ? "Fit to screen" : "Full size";
  revealShots($("#lightbox").classList.contains("reveal"));
}
function stepViewer(d) { const n = V.images.length; if (n > 1) { V.i = (V.i + d + n) % n; showShot(); } }
$("#lightbox").addEventListener("click", e => {
  const act = e.target.closest("[data-lb]")?.dataset.lb;
  if (act === "prev") stepViewer(-1);
  else if (act === "next") stepViewer(1);
  else if (act === "size") setFull(!V.full);
  else if (act === "reveal") revealShots(true);
  else if (e.target.id === "lbImg") { if (viewerBlurred()) revealShots(true); else if (!V.dragged) setFull(!V.full); }
  else if (act === "close" || e.target.id === "lightbox" || (e.target.id === "lbStage" && !V.full)) closeViewer();
});
// drag to move a full-size image; a click without movement toggles the size
$("#lbStage").addEventListener("mousedown", e => {
  if (!V.full || e.button !== 0) return;
  e.preventDefault();
  const st = $("#lbStage"), x = e.clientX, y = e.clientY, sl = st.scrollLeft, stp = st.scrollTop;
  V.dragged = false; st.classList.add("drag");
  const move = m => {
    if (Math.abs(m.clientX - x) + Math.abs(m.clientY - y) > 4) V.dragged = true;
    st.scrollLeft = sl - (m.clientX - x); st.scrollTop = stp - (m.clientY - y);
  };
  const up = () => { st.classList.remove("drag"); removeEventListener("mousemove", move); removeEventListener("mouseup", up); };
  addEventListener("mousemove", move); addEventListener("mouseup", up);
});
let touchX = null;
$("#lightbox").addEventListener("touchstart", e => { touchX = e.touches[0].clientX; }, {passive: true});
$("#lightbox").addEventListener("touchend", e => {
  if (touchX === null || V.full) return; // full size: swipes scroll the image
  const dx = e.changedTouches[0].clientX - touchX; touchX = null;
  if (Math.abs(dx) > 40) stepViewer(dx < 0 ? 1 : -1);
});

/* ===== session tally: what you sorted since opening bbt ===== */
const tally = (() => { try { return JSON.parse(sessionStorage.getItem("bbt_tally")) ?? {}; } catch { return {}; } })();
function count(mark, n) {
  if (!mark) return;
  tally[mark] = Math.max(0, (tally[mark] ?? 0) + n);
  sessionStorage.setItem("bbt_tally", JSON.stringify(tally));
}
function tallyHTML() {
  const done = Object.keys(MARKS).filter(k => tally[k]);
  const sum = done.reduce((a, k) => a + tally[k], 0);
  if (!sum) return "";
  const words = {viewed:"viewed", view_later:"to view later", download_later:"to download", downloaded:"downloaded"};
  const jump = tally.download_later ? `<button data-view="download_later" class="ghost">Open Download later</button>` : "";
  return `<p class="tally">You sorted <b>${plural(sum, "torrent")}</b> this session: ${done.map(k => `<span style="color:${MARKS[k].c}">${tally[k]} ${words[k]}</span>`).join(", ")}.</p>${jump}`;
}

/* ===== marking ===== */
let toastT;
function hideToast() { clearTimeout(toastT); $("#toast").classList.remove("show"); }
function toast(msg, undo = true) {
  $("#toastMsg").textContent = msg; $("#undoBtn").hidden = !undo;
  $("#toast").classList.add("show"); clearTimeout(toastT);
  toastT = setTimeout(() => $("#toast").classList.remove("show"), 4500);
}
const leaves = mark => S.view !== "all" && (S.view === "new" ? mark !== null : mark !== S.view);
async function setMark(ids, mark, {advance = false} = {}) {
  ids = ids.filter(id => S.items.some(i => i.id === id));
  if (!ids.length) return;
  const idx = flat.findIndex(i => i.id === S.focus);
  const leaving = leaves(mark);
  if (leaving || advance) {
    const rest = flat.slice(idx + 1).find(i => !ids.includes(i.id)) || flat.slice(0, Math.max(idx, 0)).reverse().find(i => !ids.includes(i.id));
    S.focus = leaving ? rest?.id ?? null : (flat[idx + 1]?.id ?? S.focus);
  }
  S.sel.clear();
  let res;
  try { res = await api("POST", "/api/marks", {ids, mark}); }
  catch (e) { toast(`Couldn't save: ${e.message}`, false); return load(); }
  S.undo = {kind: "mark", previous: res.previous, mark};
  count(mark, ids.length);
  const nodes = leaving ? ids.map(id => main.querySelector(`[data-id="${id}"]`)).filter(Boolean) : [];
  const apply = () => {
    if (leaving) S.items = S.items.filter(i => !ids.includes(i.id));
    else S.items.forEach(i => { if (ids.includes(i.id)) i.mark = mark; });
    render(); scrollFocus(); refreshSummary().then(() => document.querySelector(`.bin[data-mark="${mark}"]`)?.classList.add("bump"));
  };
  if (nodes.length && nodes.length < 60 && !matchMedia("(prefers-reduced-motion: reduce)").matches) { nodes.forEach(n => n.classList.add("leaving")); setTimeout(apply, 260); }
  else apply();
  const where = mark ? MARKS[mark].label : "New";
  toast(`${plural(ids.length, "torrent")} moved to ${where}`);
}
async function undo() {
  if (!S.undo?.previous.length) return;
  const {kind, previous: prev, mark} = S.undo; S.undo = null;
  if (kind === "mark") count(mark, -prev.length);
  const key = kind === "mark" ? "mark" : "stars";
  const byValue = new Map();
  for (const p of prev) { const k = p[key] ?? ""; if (!byValue.has(k)) byValue.set(k, []); byValue.get(k).push(p.id); }
  try { for (const [v, ids] of byValue) await api("POST", kind === "mark" ? "/api/marks" : "/api/ratings", {ids, [key]: v === "" ? null : v}); }
  catch (e) { toast(`Undo failed: ${e.message}`, false); }
  S.focus = prev[0].id;
  await load(); scrollFocus(); toast("Change undone", false);
}
const targets = () => S.sel.size ? [...S.sel] : S.focus ? [S.focus] : [];
function scrollFocus() { main.querySelector(`[data-id="${S.focus}"]`)?.scrollIntoView({block:"nearest"}); }
function gridCols() {
  const cards = [...main.querySelectorAll(".card")];
  if (!cards.length) return 1;
  const top = cards[0].offsetTop;
  return Math.max(1, cards.findIndex(c => c.offsetTop !== top) === -1 ? cards.length : cards.findIndex(c => c.offsetTop !== top));
}
function move(d) {
  const i = flat.findIndex(x => x.id === S.focus);
  const n = flat[Math.max(0, Math.min(flat.length - 1, i + d))];
  if (!n) return;
  main.querySelector(".focus")?.classList.remove("focus");
  S.focus = n.id;
  const el = main.querySelector(`[data-id="${n.id}"]`);
  el?.classList.add("focus"); el?.scrollIntoView({block:"nearest"});
  pick(); renderTarget();
  if (S.drawer) openDrawer(n.id, {delay: FOLLOW_MS});
}
/* ===== bulk "mark viewed" =====
   Only unmarked torrents, and only in the lists that hold them (New, Everything),
   so a curated list can never be swept into Viewed. Big batches need a second press. */
const BULK_CONFIRM = 20;
let bulkArm = null; // {g, timer}
const bulkable = () => S.view === "new" || S.view === "all";
const bulkIdsOf = g => shown.find(x => x.key === g)?.items.filter(i => !i.mark).map(i => i.id) ?? [];
const groupOf = id => shown.find(g => g.items.some(i => i.id === id))?.key;
function bulkButton(g, n) {
  const armed = bulkArm?.g === g;
  return `<button data-bulk="${esc(g)}" class="${armed ? "armed" : ""}">${armed ? `Confirm: mark ${n} viewed` : `Mark ${n} viewed`}</button>`;
}
function disarmBulk() {
  if (!bulkArm) return;
  const {g} = bulkArm; clearTimeout(bulkArm.timer); bulkArm = null; hideToast();
  const b = main.querySelector(`[data-bulk="${CSS.escape(g)}"]`);
  if (b) b.outerHTML = bulkButton(g, bulkIdsOf(g).length);
}
function bulkViewed(g) {
  if (!bulkable() || g == null) return;
  const ids = bulkIdsOf(g);
  if (!ids.length) return;
  if (ids.length > BULK_CONFIRM && bulkArm?.g !== g) {
    disarmBulk();
    bulkArm = {g, timer: setTimeout(disarmBulk, 5000)};
    const b = main.querySelector(`[data-bulk="${CSS.escape(g)}"]`);
    if (b) b.outerHTML = bulkButton(g, ids.length);
    return toast(`Move ${plural(ids.length, "torrent")} to Viewed? Press Shift 1 again or click Confirm.`, false);
  }
  disarmBulk();
  setMark(ids, "viewed"); // its own toast (with Undo) replaces the confirm prompt
}

/* ===== older pages (Everything, scroll to the end) ===== */
const O = {busy:false, msg:"", budgetLeft:null, error:false};
function olderBlocked() {
  const oldest = S.summary?.coverage_from;
  if (oldest && oldest <= cutoff()) return `Reached the start of your From date (${fmtWhen(cutoff())}). Pick an earlier date to load more.`;
  if (O.budgetLeft === 0) return "Daily limit reached: 40 tracker pages. More tomorrow.";
  return null;
}
function olderHTML() {
  const blocked = olderBlocked();
  const text = O.busy ? "Loading one older page from tracker…" : S.syncing ? "Waiting for the sync to finish…" : blocked ?? (O.msg || "Scroll here to load older uploads, one tracker page at a time.");
  const btn = !O.busy && !S.syncing && !blocked ? `<button data-act="older">Load older</button>` : "";
  return `<div class="older" id="older" aria-live="polite"><span>${esc(text)}</span>${btn}</div>`;
}
const olderObserver = new IntersectionObserver(entries => {
  if (entries.some(e => e.isIntersecting) && !O.error) loadOlder();
}, {root: main, rootMargin: "200px"});
async function loadOlder() {
  if (O.busy || S.syncing || olderBlocked()) return;
  O.busy = true; O.error = false;
  const el = $("#older"); if (el) el.outerHTML = olderHTML();
  try {
    const r = await api("POST", "/api/older");
    O.budgetLeft = r.budget_left;
    if (r.status === "done") O.msg = `Read tracker page ${r.page + 1}: ${r.added} older upload${r.added === 1 ? "" : "s"} added. ${r.budget_left} page${r.budget_left === 1 ? "" : "s"} left today.`;
  } catch (e) {
    O.msg = `Couldn't load older uploads: ${e.message}`; O.error = true;
  } finally {
    O.busy = false;
  }
  await load();
}

/* ===== ratings ===== */
async function setRating(ids, stars) {
  ids = ids.filter(id => S.items.some(i => i.id === id));
  if (!ids.length) return;
  let res;
  try { res = await api("POST", "/api/ratings", {ids, stars}); }
  catch (e) { return toast(`Couldn't save rating: ${e.message}`, false); }
  S.undo = {kind: "rating", previous: res.previous};
  S.items.forEach(i => { if (ids.includes(i.id)) i.rating = stars; });
  S.sel.clear();
  if (S.sort === "rating") await load(); else render();
  const what = ids.length === 1 ? "" : ` ${ids.length} torrents`;
  toast(stars ? `Rated${what} ${stars} star${stars === 1 ? "" : "s"}` : `Rating cleared${what ? " on" + what : ""}`);
}
// `r` arms rating; the decision bar shows the prompt for exactly as long as it stays armed.
const RATE_MS = 4000;
let rateArmed = 0, rateT;
function armRate(on) {
  rateArmed = on ? Date.now() : 0; clearTimeout(rateT);
  if (on) rateT = setTimeout(() => armRate(false), RATE_MS);
  renderTarget();
}

/* ===== sync ===== */
async function doSync() {
  if (S.syncing) return;
  S.syncing = true;
  const prog = $("#syncProg"), text = $("#syncText"), btn = $("#syncBtn");
  prog.hidden = false; prog.classList.add("busy"); btn.disabled = true;
  text.textContent = "Syncing: reading up to 12 pages, about 40 seconds…";
  const older = $("#older"); if (older) older.outerHTML = olderHTML();
  let msg;
  try {
    const r = await api("POST", "/api/sync");
    msg = r.status === "cooling_down"
      ? `Synced recently. Try again in ${Math.ceil(r.seconds_left / 60)} min.`
      : `Synced just now: ${r.new_count} new from ${r.pages} page(s).${r.complete ? "" : ` Stopped early: ${r.stop}.`}`;
  } catch (e) {
    msg = `Sync failed: ${e.message}`;
  } finally {
    S.syncing = false; prog.hidden = true; prog.classList.remove("busy"); btn.disabled = false;
  }
  await load();
  text.textContent = msg;
}
async function maybeSyncOnOpen() {
  if (localStorage.getItem("bbt_sync_on_open") !== "1") return; // off unless turned on
  try {
    const st = await api("GET", "/api/sync");
    const stale = !st.last_sync || nowSec() - st.last_sync.finished_at > 3600;
    if (stale && !st.running && st.cooldown_seconds === 0) doSync();
  } catch { /* ignore */ }
}

/* ===== events ===== */
main.addEventListener("click", e => {
  if (e.target.closest("[data-act=sync]")) return doSync();
  const jump = e.target.closest("[data-view]"); if (jump) return showView(jump.dataset.view);
  if (e.target.closest("[data-act=clearfilters]")) { S.q = ""; $("#q").value = ""; S.free = false; $("#freeOnly").checked = false; return load(); }
  if (e.target.closest("[data-act=allcats]")) { S.cats = new Set(Object.keys(CATS).map(Number)); saveCats(); return load(); }
  if (e.target.closest("[data-act=older]")) { O.error = false; return loadOlder(); }
  const bulk = e.target.closest("[data-bulk]"); if (bulk) return bulkViewed(bulk.dataset.bulk);
  const r = e.target.closest("[data-id]"); if (!r) return;
  const id = +r.dataset.id;
  if (e.target.classList.contains("sel")) { S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id); renderTarget(); return; }
  // First click focuses; clicking the focused torrent again opens its details.
  if (S.focus === id && S.drawer !== id) return openDrawer(id);
  main.querySelector(".focus")?.classList.remove("focus");
  S.focus = id; r.classList.add("focus"); pick(); renderTarget();
  if (S.drawer) openDrawer(id, {delay: FOLLOW_MS});
});
$("#bins").addEventListener("click", e => { const b = e.target.closest(".bin"); if (b) setMark(targets(), b.dataset.mark, {advance:true}); });
$("#drawerIn").addEventListener("click", e => {
  const star = e.target.closest("[data-rate]");
  if (star) { const it = S.items.find(i => i.id === S.drawer); const n = +star.dataset.rate; return setRating([S.drawer], it?.rating === n ? null : n); }
  const shot = e.target.closest("[data-shot]");
  if (shot) { e.preventDefault(); return openViewer(detailsCache.get(S.drawer)?.images ?? [], +shot.dataset.shot); }
  const b = e.target.closest("[data-set]"); if (b) return setMark([S.drawer], b.dataset.set);
  if (e.target.closest("[data-act=close]")) return closeDrawer();
  if (e.target.closest("[data-act=retry]")) { detailsCache.delete(S.drawer); return openDrawer(S.drawer); }
  if (e.target.closest("[data-act=download]")) return download(S.drawer);
});
function showView(v) { S.view = v; S.focus = null; S.picked = false; S.sel.clear(); main.scrollTop = 0; setRail(false); load(); }
// Below 760px the rail is a sheet and the toolbar controls fold into View.
function setRail(on) {
  $("#rail").classList.toggle("open", on); $("#scrim").hidden = !on; $("#railBtn").ariaExpanded = String(on);
  if (on) $("#views [aria-current=true]")?.focus();
}
$("#railBtn").onclick = () => setRail(true);
$("#scrim").onclick = () => setRail(false);
$("#moreBtn").onclick = () => { const on = $(".top").classList.toggle("more"); $("#moreBtn").ariaExpanded = String(on); };
$("#views").addEventListener("click", e => { const b = e.target.closest("[data-view]"); if (b) showView(b.dataset.view); });
$("#cats").addEventListener("change", e => { const c = +e.target.dataset.cat; e.target.checked ? S.cats.add(c) : S.cats.delete(c); saveCats(); load(); });
$("#cats").addEventListener("click", e => {
  const b = e.target.closest("[data-cats]"); if (!b) return;
  S.cats = b.dataset.cats === "all" ? new Set(Object.keys(CATS).map(Number)) : new Set();
  saveCats(); load();
});
// Sort, range and layout survive a reload.
const keep = (k, v) => localStorage.setItem(k, v);
$("#range").onchange = e => { S.range = e.target.value; keep("bbt_range", S.range); $("#customWrap").hidden = S.range !== "custom"; load(); };
$("#from").onchange = e => { S.from = e.target.value; keep("bbt_from", S.from); load(); };
$("#sort").onchange = e => { S.sort = e.target.value; keep("bbt_sort", S.sort); load().then(scrollFocus); };
let qT; $("#q").oninput = e => { clearTimeout(qT); qT = setTimeout(() => { S.q = e.target.value.trim(); load(); }, 250); };
$("#freeOnly").onchange = e => { S.free = e.target.checked; load(); };
const setBlur = on => {
  document.body.classList.toggle("blur", on); $("#blurT").checked = on;
  if (viewerOpen()) revealShots($("#lightbox").classList.contains("reveal"));
};
$("#blurT").onchange = e => setBlur(e.target.checked);
const setHideTitles = on => { document.body.classList.toggle("hide-titles", on); $("#hideT").checked = on; };
$("#hideT").onchange = e => setHideTitles(e.target.checked);
// hold Option/Alt to read blurred titles; let go to blur them again
const peek = on => document.body.classList.toggle("peek", on);
addEventListener("keydown", e => { if (e.key === "Alt") peek(true); });
addEventListener("keyup", e => { if (e.key === "Alt") peek(false); });
addEventListener("blur", () => peek(false));
function setGrid(on) {
  S.grid = on; keep("bbt_grid", on ? "1" : "0");
  $("#vGrid").ariaPressed = String(on); $("#vList").ariaPressed = String(!on);
}
$("#vList").onclick = () => { setGrid(false); render(); scrollFocus(); };
$("#vGrid").onclick = () => { setGrid(true); render(); scrollFocus(); };
$("#syncBtn").onclick = doSync;
$("#helpBtn").onclick = () => $("#help").showModal();
$("#setBtn").onclick = () => $("#settings").showModal();
$("#undoBtn").onclick = undo;
$("#sOpen").onchange = e => localStorage.setItem("bbt_sync_on_open", e.target.checked ? "1" : "0");
$("#sSticky").onchange = e => { S.sticky = e.target.checked; localStorage.setItem("bbt_sticky", S.sticky ? "1" : "0"); load(); };
$("#sBlur").onchange = e => localStorage.setItem("bbt_blur", e.target.checked ? "1" : "0");
$("#sHide").onchange = e => localStorage.setItem("bbt_hide_titles", e.target.checked ? "1" : "0");
document.addEventListener("keydown", e => {
  if (viewerOpen()) {
    if (e.key === "ArrowRight" || e.key === "l") { e.preventDefault(); stepViewer(1); }
    else if (e.key === "ArrowLeft" || e.key === "h") { e.preventDefault(); stepViewer(-1); }
    else if (e.key === "f") { e.preventDefault(); setFull(!V.full); }
    else if (e.key === "v") { e.preventDefault(); revealShots(true); }
    else if (e.key === "Escape") closeViewer();
    return;
  }
  if (e.target.matches?.("input[type=search],input[type=text],input[type=number],input[type=date],select")) { if (e.key === "Escape") e.target.blur(); return; }
  if (document.querySelector("dialog[open]") || e.metaKey || e.ctrlKey || e.altKey) return;
  const k = e.key, step = S.grid ? gridCols() : 1;
  if (rateArmed && /^[0-5]$/.test(k)) {
    armRate(false); e.preventDefault();
    return setRating(targets(), k === "0" ? null : +k);
  }
  if (rateArmed) armRate(false);
  if (k === "r" && targets().length) return armRate(true);
  if (k === "j" || k === "ArrowDown") { e.preventDefault(); move(k === "ArrowDown" ? step : 1); }
  else if (k === "k" || k === "ArrowUp") { e.preventDefault(); move(k === "ArrowUp" ? -step : -1); }
  else if (S.grid && k === "ArrowRight") move(1);
  else if (S.grid && k === "ArrowLeft") move(-1);
  else if (k === "!" || (e.shiftKey && e.code === "Digit1")) bulkViewed(groupOf(S.focus));
  else if (k === "1") setMark(targets(), "viewed", {advance:true});
  else if (k === "2") setMark(targets(), "view_later", {advance:true});
  else if (k === "3") setMark(targets(), "download_later", {advance:true});
  else if (k === "4") setMark(targets(), "downloaded", {advance:true});
  else if (k === "0") setMark(targets(), null);
  else if (k === "x" && S.focus) { const id = S.focus; S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id); const c = main.querySelector(`[data-id="${id}"] .sel`); if (c) c.checked = S.sel.has(id); renderTarget(); }
  else if (k === "Enter" && S.focus) openDrawer(S.focus);
  else if (k === "Escape") { disarmBulk(); setRail(false); closeDrawer(); if (S.sel.size) { S.sel.clear(); render(); } }
  else if (k === "o" && S.focus) { const it = S.items.find(i => i.id === S.focus); if (it && siteUrl(it)) window.open(siteUrl(it), "_blank", "noreferrer"); }
  else if (k === "u") undo();
  else if (k === "b") setBlur(!document.body.classList.contains("blur"));
  else if (k === "t") setHideTitles(!document.body.classList.contains("hide-titles"));
  else if (k === "/") { e.preventDefault(); $("#q").focus(); }
  else if (k === "?") $("#help").showModal();
});

/* ===== boot ===== */
(async function boot() {
  setBlur(localStorage.getItem("bbt_blur") !== "0");
  $("#sOpen").checked = localStorage.getItem("bbt_sync_on_open") === "1";
  $("#sSticky").checked = S.sticky;
  $("#sBlur").checked = localStorage.getItem("bbt_blur") !== "0";
  setHideTitles(localStorage.getItem("bbt_hide_titles") === "1");
  $("#sHide").checked = localStorage.getItem("bbt_hide_titles") === "1";
  $("#from").max = dayKey(nowSec());
  const sort = localStorage.getItem("bbt_sort"), range = localStorage.getItem("bbt_range");
  if (sort && $(`#sort option[value="${sort}"]`)) S.sort = $("#sort").value = sort;
  if (range && $(`#range option[value="${range}"]`)) S.range = $("#range").value = range;
  S.from = $("#from").value = localStorage.getItem("bbt_from") || dayKey(nowSec() - 2 * DAY);
  $("#customWrap").hidden = S.range !== "custom";
  setGrid(localStorage.getItem("bbt_grid") === "1");
  // "Since last visit" = the visit before this browser session, so reloads don't reset it.
  let lv = sessionStorage.getItem("bbt_last_visit");
  if (lv === null) {
    try { const r = await api("POST", "/api/visit"); lv = r.previous == null ? "" : String(r.previous); }
    catch { lv = ""; }
    sessionStorage.setItem("bbt_last_visit", lv);
  }
  S.lastVisit = lv ? Number(lv) : null;
  try { setCats((await api("GET", `/api/summary?from=${cutoff()}`)).categories); } catch { setCats([]); }
  S.cats = loadCats();
  // The database copy wins: it survives browser storage being cleared.
  try {
    const prefs = await api("GET", "/api/prefs");
    if (Array.isArray(prefs.categories)) {
      S.cats = new Set(prefs.categories.filter(c => c in CATS));
      localStorage.setItem("bbt_cats", JSON.stringify([...S.cats]));
    } else if (localStorage.getItem("bbt_cats")) {
      saveCats(); // first run with server prefs: keep what this browser had
    }
  } catch { /* keep the local copy */ }
  await load();
  maybeSyncOnOpen();
})();
