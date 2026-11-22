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
  viewed:         {label:"Viewed",         c:"var(--viewed)",        hint:"Seen it, skip next time"},
  view_later:     {label:"View later",     c:"var(--watch)",   hint:"Look at it properly later"},
  download_later: {label:"Download later", c:"var(--get)",     hint:"Grab the .torrent later"},
  downloaded:     {label:"Downloaded",     c:"var(--got)",     hint:"Already have it"},
};
/* ===== key bindings ===== */
// Single-character keys only. Enter, Esc, the arrows and Option stay fixed.
const KEY_DEFAULTS = {
  next:"j", prev:"k", open:"l", site:"o", search:"/",
  viewed:"1", view_later:"2", download_later:"3", downloaded:"4", unmark:"0", bulk:"!", undo:"u",
  rate:"r", select:"x", blur:"b", titles:"t", help:"?", swipe:"s",
  lbPrev:"h", lbNext:"l", lbReveal:"v", lbFull:"f",
};
// The viewer has its own keys, so a key can mean one thing there and another in the list.
const inViewer = a => a.startsWith("lb");
function loadKeys() {
  const keys = {...KEY_DEFAULTS};
  try {
    const saved = JSON.parse(localStorage.getItem("bbt_keymap")) ?? {};
    for (const [a, k] of Object.entries(saved)) if (a in keys && typeof k === "string" && k.length === 1) keys[a] = k;
  } catch { /* defaults */ }
  return keys;
}
let KEYS = loadKeys();
const keyName = k => k === " " ? "Space" : k === "!" ? "Shift 1" : k;
const keyOf = a => keyName(KEYS[a]);
// Shift 1 is "!" on US keyboards; match the physical key so other layouts work too.
const hit = (a, e) => e.key === KEYS[a] || (KEYS[a] === "!" && e.shiftKey && e.code === "Digit1");
const VIEWS = [
  ["new","New","var(--new)"],["top","Most seeded","var(--top)"],["view_later","View later","var(--watch)"],["download_later","Download later","var(--get)"],
  ["downloaded","Downloaded","var(--got)"],["viewed","Viewed","var(--viewed)"],["all","Everything","var(--faint)"],
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
  api("PUT", "/api/prefs", {categories}).catch(e => toast(`Couldn't save categories. ${e.message}`, false));
}
const S = {
  view:"new", cats:loadCats(), range:"0", from:"", sort:"new", q:"",
  grid:false, free:false, sticky:localStorage.getItem("bbt_sticky")==="1",
  items:[], summary:null, lastVisit:null, focus:null, sel:new Set(), undos:[], drawer:null,
  syncing:false, downloading:new Set(), refreshing:new Set(), loadSeq:0, keys:localStorage.getItem("bbt_keys") !== "0",
};
const $ = s => document.querySelector(s);
const touch = () => matchMedia("(pointer: coarse)").matches;
// Touch screens get touch words and no key hints; a keyboard on a tablet brings them back.
function markTouch() { document.documentElement.classList.toggle("touch", touch()); }
markTouch(); matchMedia("(pointer: coarse)").addEventListener("change", () => { markTouch(); renderKeys(); });
const main = $("#main");

/* ===== api ===== */
// Posters and screenshots come through bbt's saved copies, which the browser keeps.
const pic = u => `/img?u=${encodeURIComponent(u)}`;
async function api(method, path, body) {
  let res;
  try { res = await fetch(path, {method, headers: body ? {"content-type":"application/json"} : {}, body: body ? JSON.stringify(body) : undefined}); }
  catch { throw new Error("Can't reach bbt. Check that the server is running."); }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(plainError(data.error || `${res.status} ${res.statusText}`));
  return data;
}
// Server and tracker errors in plain words: no URLs or library text on screen.
function plainError(raw) {
  const m = String(raw ?? "");
  if (/timed out|timeout/i.test(m)) return "The tracker didn't answer in time. Try again in a minute.";
  if (/error sending request|connection|connect error|dns|refused|unreachable/i.test(m)) return "Can't reach the tracker. Check the connection, then try again.";
  if (/not logged in|login failed/i.test(m)) return "Couldn't log in to the tracker. Check the username and password in .env.";
  if (/unexpected response|layout not recognised/i.test(m)) return "The tracker sent a page bbt doesn't recognise. Try again later.";
  const t = m.replace(/\s*\(?(for url\s*)?\(?https?:\/\/[^\s)]+\)?/gi, "").replace(/^(tracker|error):\s*/i, "").trim();
  return t ? t[0].toUpperCase() + t.slice(1) + (/[.!?]$/.test(t) ? "" : ".") : "Something went wrong.";
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
  return m < 1 ? "just now" : m < 60 ? Math.round(m) + " min ago" : m < 1440 ? Math.round(m / 60) + " h ago" : Math.round(m / 1440) + " d ago";
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
  const img = it.poster_url ? `<img src="${esc(pic(it.poster_url))}" alt="" loading="lazy" referrerpolicy="no-referrer" onerror="this.remove()">` : "";
  return `<div class="poster" style="--g:${grad(it)}"><span>${esc(code(it))}</span>${img}</div>`;
}

// Only New and Everything are filtered by date ("From"); the sorted lists hold everything.
const dated = () => S.view === "new" || S.view === "all";
// Lists that keep every torrent whatever its mark, so marks show as tags and rows stay put.
const mixedView = () => S.view === "all";
// Lists of unmarked torrents only: any mark takes a row out.
const unmarkedView = () => S.view === "new" || S.view === "top";
function rangeText() {
  if (S.range === "last") return S.lastVisit ? "since your last visit" : "in the last day";
  if (S.range === "custom") return S.from ? `since ${new Date(S.from + "T12:00:00+07:00").toLocaleDateString("en-GB", {day:"numeric", month:"short"})}` : "today";
  return {"0":"today", "2":"in the last 3 days", "6":"in the last 7 days"}[S.range] ?? "in this date range";
}
// Unsorted torrents the date range hides, in the selected categories.
function olderNew() {
  const by = S.summary?.counts.older_new_by_category ?? {};
  return Object.entries(by).reduce((a, [k, n]) => a + (!catsOn() || S.cats.has(+k) ? n : 0), 0);
}
// "matching “x”, free" etc.: the filters that narrow a list beyond its date range.
function filterText() {
  const parts = [];
  if (S.q) parts.push(`matching “${esc(S.q)}”`);
  if (S.free) parts.push("free");
  if (catsOn() && S.cats.size < Object.keys(CATS).length) parts.push(`in ${S.cats.size} ${S.cats.size === 1 ? "category" : "categories"}`);
  return parts.join(", ");
}
function backlogHTML() {
  const n = olderNew(), from = S.summary?.counts.older_new_from;
  if (!n || !from) return "";
  const day = new Date((from + ICT) * 1000).toLocaleDateString("en-GB", {day: "numeric", month: "short", timeZone: "UTC"});
  return `<p class="backlog"><b>${plural(n, "older torrent")}</b> ${n === 1 ? "is" : "are"} still unsorted, back to ${day}.</p><button data-act="showolder">Show ${n === 1 ? "it" : `all ${n}`}</button>`;
}
// Widen the range to the oldest unsorted torrent, so nothing waits out of sight.
function showOlder() {
  const from = S.summary?.counts.older_new_from;
  if (!from) return;
  S.range = $("#range").value = "custom"; S.from = $("#from").value = dayKey(from); $("#customWrap").hidden = false;
  showView("new");
}
function syncRange() {
  const on = dated();
  $("#range").disabled = !on; $("#from").disabled = !on;
  $("#rangeField").title = on ? "" : "From only filters New and Everything";
  const top = S.view === "top";
  $("#sort").disabled = top; $("#sort").title = top ? "Most seeded keeps the tracker's order" : "";
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
    banner(`Can't load torrents. ${e.message}`);
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
  if (S.view === "top") {
    const at = S.summary?.top?.fetched_at;
    return [{key:"top", label:`On the tracker${at ? `, read ${fmtAgo(at)}` : ""}`, items:S.items}];
  }
  if (S.sort !== "new") return [{key:"all", label:VIEWS.find(v => v[0] === S.view)[1], items:S.items}];
  const m = new Map();
  for (const it of S.items) { const k = dayKey(it.uploaded_at); if (!m.has(k)) m.set(k, []); m.get(k).push(it); }
  return [...m].map(([k, items]) => ({key:k, label:dayLabel(k), items}));
}

/* ===== render ===== */
// Another upload of the same release: show how you sorted it (or that it exists).
function otherTag(it) {
  if (!it.others?.length) return "";
  const o = it.others.find(o => o.mark) ?? it.others[0];
  const label = o.mark ? `Other upload ${MARKS[o.mark].label.toLowerCase()}` : "Also uploaded";
  const tip = it.others.map(o => `${o.title} (${o.mark ? MARKS[o.mark].label : "New"})`).join("\n");
  return `<span class="tag ${o.mark ? "state" : ""}" ${o.mark ? `style="--state:${MARKS[o.mark].c}"` : ""} title="${esc(tip)}">${label}</span>`;
}
function tags(it) {
  const mark = mixedView() && it.mark ? `<span class="tag state" style="--state:${MARKS[it.mark].c}">${MARKS[it.mark].label}</span>` : "";
  return `${catsOn() ? `<span class="tag">${esc(CATS[it.category] ?? `Category ${it.category}`)}</span>` : ""}${it.sticky ? '<span class="tag sticky">Sticky</span>' : ""}${it.free ? '<span class="tag free">Free</span>' : ""}${it.multiplier ? `<span class="tag free">${esc(it.multiplier)} up</span>` : ""}${mark}${otherTag(it)}${it.rating ? `<span class="stars" title="Your rating: ${it.rating} of 5">★${it.rating}</span>` : ""}<span>${esc(it.uploader ?? "")}</span>`;
}
const stateOf = it => it.mark ?? "new";
const stateAttr = it => ({viewed:"viewed", view_later:"watch", download_later:"get", downloaded:"got", new:"new"})[stateOf(it)];
// One number column, and it follows the sort: seeders by default, else what you sorted by.
const STATS = {seed:["Seeds", "seeders"], leech:["Leech", "leechers"], snatch:["Snatched", "snatched"]};
const stat = () => S.view === "top" ? STATS.seed : STATS[S.sort] ?? STATS.seed;
function rowHTML(it, byDay) {
  const [, key] = stat();
  return `<div class="row${S.focus === it.id ? " focus" : ""}" data-id="${it.id}" data-state="${stateAttr(it)}" role="option" aria-selected="${S.focus === it.id}">
  <i class="bar"></i><label class="selbox"><input class="sel" type="checkbox" aria-label="Select ${esc(clean(it.title))}" ${S.sel.has(it.id) ? "checked" : ""}></label>
  ${poster(it)}
  <div class="meta"><div class="title" title="${esc(it.title)}">${esc(clean(it.title))}</div><div class="tags">${tags(it)}</div></div>
  <div class="num">${fmtSize(it.size_bytes)}<small>${it.files} file${it.files === 1 ? "" : "s"}</small></div>
  <div class="num c-stat${key === "seeders" && it.seeders > 200 ? " hot" : ""}">${it[key]}</div>
  <div class="time c-time">${byDay ? fmtTime(it.uploaded_at) : fmtAgo(it.uploaded_at)}</div></div>`;
}
function cardHTML(it) {
  return `<div class="card${S.focus === it.id ? " focus" : ""}" data-id="${it.id}" data-state="${stateAttr(it)}" role="option">
  ${poster(it)}<i class="flag"></i><label class="selbox"><input class="sel" type="checkbox" aria-label="Select ${esc(clean(it.title))}" ${S.sel.has(it.id) ? "checked" : ""}></label>
  <div class="title">${esc(clean(it.title))}</div><div class="tags">${it.rating ? `<span class="stars">★${it.rating}</span>` : ""}<span>${fmtSize(it.size_bytes)}</span><span>${it.seeders} seeding</span></div></div>`;
}
let shown = [];
function render() {
  const gs = groups(); shown = gs; flat = gs.flatMap(g => g.items);
  if (S.focus && !flat.some(i => i.id === S.focus)) S.focus = null;
  if (!S.focus && flat.length) S.focus = flat[0].id;
  const byDay = S.sort === "new" && S.view !== "top";
  const keepBanner = $(".banner");
  if (!flat.length) {
    const total = S.summary?.counts.total ?? 0;
    const label = VIEWS.find(v => v[0] === S.view)[1];
    let msg;
    if (catsOn() && !S.cats.size) msg = `<h3>No categories selected</h3><p>Tick at least one category on the left to see torrents.</p><button data-act="allcats">Select all categories</button>`;
    else if (S.q || S.free) msg = `<h3>No matches</h3><p>Nothing in ${label}${S.q ? ` matches “${esc(S.q)}”` : ""}${S.free ? `${S.q ? " and is" : " is"} free` : ""}${dated() ? ` ${rangeText()}` : ""}.</p><button data-act="clearfilters">Clear ${S.q && S.free ? "search and Free only" : S.q ? "search" : "Free only"}</button>`;
    else if (S.view === "top") msg = TOP.busy ? `<h3>Reading the tracker…</h3><p>Loading the ${TOP_PAGES} most seeded pages, a few seconds apart.</p>`
      : S.summary?.top?.fetched_at && !TOP.msg ? `<h3>All caught up</h3><p>Every torrent on the tracker's most seeded pages is marked.</p>`
      : `<h3>Nothing in Most seeded yet</h3><p>${esc(TOP.msg || "Open this list again to read it from the tracker.")}</p><button data-act="top">Read it now</button>`;
    else if (!total) msg = `<h3>No torrents yet</h3><p>Sync to fetch the latest uploads from tracker.</p><button data-act="sync">Sync now</button>`;
    else if (S.view === "new") msg = olderNew()
      ? `<h3>All caught up ${rangeText()}</h3>${tallyHTML()}${ACTIVITY}${backlogHTML()}<button data-act="sync" class="ghost">Sync now</button>`
      : `<h3>All caught up</h3>${tallyHTML()}${ACTIVITY}<p>Nothing new ${rangeText()}. Sync to check the tracker for new uploads.</p><button data-act="sync">Sync now</button>`;
    else msg = `<h3>Nothing in ${label}</h3><p>${MARKS[S.view] ? `Press ${keyOf(S.view)} on a torrent to add it here.` : "Try a wider date range."}</p>`;
    main.innerHTML = `<div class="empty">${msg}</div>`;
    fillActivity();
  } else {
    const total = S.summary?.counts.total ?? 0;
    const head = S.grid ? "" : `<div class="colhead"><span></span><span></span><span></span><span class="l">${S.view === "all" && total > flat.length && flat.length < 5000 ? `Title · ${flat.length} uploaded ${rangeText()}, of ${total} in bbt` : flat.length >= 5000 ? "Title · first 5000 shown" : "Title"}</span><span>Size</span><span class="c-stat">${stat()[0]}</span><span class="c-time">${byDay ? "Time" : "Age"}</span></div>`;
    main.classList.toggle("has-colhead", !S.grid);
    main.classList.toggle("mixed", mixedView()); // the lists where states differ
    setHTML(main, head + gs.map(g => {
      const nNew = g.items.filter(i => !i.mark).length;
      return `<section class="group" data-g="${g.key}"><div class="ghead"><h3>${esc(g.label)}</h3>
      <span class="sub">${plural(g.items.length, "torrent")}${nNew && S.view !== "new" ? `, <b>${nNew} new</b>` : ""}</span>
      <span class="act">${bulkable() && nNew ? bulkButton(g.key, nNew) : ""}</span></div>
      ${S.grid ? `<div class="cards" role="listbox">${g.items.map(cardHTML).join("")}</div>` : `<div role="listbox">${g.items.map(it => rowHTML(it, byDay)).join("")}</div>`}</section>`;
    }).join(""));
  }
  const oldest = S.summary?.coverage_from;
  if (S.view === "all" && S.sort === "new" && S.summary?.counts.total) {
    main.insertAdjacentHTML("beforeend", olderHTML());
    const el = $("#older"); if (el) olderObserver.observe(el);
  } else if (oldest && S.view === "new" && flat.length && cutoff() < oldest - 3600) {
    main.insertAdjacentHTML("beforeend", `<p class="coverage">bbt has every upload back to ${fmtWhen(oldest)}. Earlier ones aren't loaded yet, so this list can't go further back. Open Everything and scroll to the end to load older pages.</p>`);
  }
  if (keepBanner) main.prepend(keepBanner);
  renderNav(); renderBins(); renderTarget(); renderSyncText(); syncRange();
  if (S.drawer) renderDrawer();
  if (SW.open) renderSwipe();
}
function renderNav() {
  const c = S.summary?.counts;
  const n = k => !c ? "" : k === "new" ? c.new : k === "all" ? c.total : k === "top" ? S.summary.top?.count || "" : c[k];
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
  $("#bins").innerHTML = Object.entries(MARKS).map(([k, m]) => `<button class="bin" data-s="${{viewed:"viewed", view_later:"watch", download_later:"get", downloaded:"got"}[k]}" data-mark="${k}" ${blind() ? "disabled" : ""}><kbd>${keyOf(k)}</kbd><span class="lbl">${m.label}<small>${m.hint}</small></span><span class="count">${c[k] ?? 0}</span></button>`).join("");
}
function renderTarget() {
  const t = $("#target"), it = flat.find(i => i.id === S.focus);
  const what = S.sel.size ? `${S.sel.size} selected` : it ? esc(code(it)) : "—";
  t.classList.toggle("rating", !!rateArmed);
  t.innerHTML = rateArmed ? `${S.sel.size ? `Rate ${S.sel.size} selected` : "Rate it"}<b>1–5, or 0 to clear</b>` : blind() && flat.length ? `Applies to<b>Tap a torrent first</b>`
    : `Applies to<b>${what}</b>${touch() && it && !S.sel.size ? `<small>Tap it again for details</small>` : ""}`;
  main.classList.toggle("blind", blind());
}
const syncFailed = ls => /^error/.test(ls?.stop_reason ?? "");
function stopText(reason) {
  if (/^error/.test(reason ?? "")) return `failed. ${plainError(reason)}`;
  return {"reached cutoff": "caught up with the previous sync", "page cap reached, older items skipped": "stopped at the 12-page limit, older uploads skipped",
    "empty page": "stopped at an empty page", "page layout not recognised": "stopped: the tracker page looked different"}[reason] ?? reason ?? "";
}
function renderSyncText() {
  if (S.syncing) return;
  const ls = S.summary?.last_sync;
  $("#syncText").textContent = !ls ? "Not synced yet"
    : syncFailed(ls) ? `Sync failed at ${fmtWhen(ls.finished_at)}. ${plainError(ls.stop_reason)}`
    : `Synced ${fmtWhen(ls.finished_at)}, ${ls.new_count} new${/page cap/.test(ls.stop_reason ?? "") ? ". Older uploads were skipped." : ""}`;
  $("#sLast").textContent = !ls ? "Never" : syncFailed(ls) ? `${fmtWhen(ls.finished_at)}: ${stopText(ls.stop_reason)}`
    : `${fmtWhen(ls.finished_at)}: ${plural(ls.pages, "page")}, ${ls.new_count} new, ${stopText(ls.stop_reason)}`;
  renderSyncBtn();
}
// bbt waits 10 minutes between syncs; the button counts down instead of failing.
let syncTick;
async function checkSyncWait() {
  try { const st = await api("GET", "/api/sync"); S.syncUntil = nowSec() + st.cooldown_seconds; } catch { S.syncUntil = 0; }
  renderSyncBtn();
}
const syncWaitMin = () => Math.ceil(Math.max(0, (S.syncUntil ?? 0) - nowSec()) / 60);
function renderSyncBtn() {
  const btn = $("#syncBtn"), min = syncWaitMin();
  clearTimeout(syncTick);
  if (S.syncing) return;
  btn.disabled = min > 0;
  btn.textContent = min > 0 ? `Sync again in ${min} min` : "Sync now";
  btn.title = min > 0 ? "bbt waits 10 minutes between syncs to go easy on the tracker" : "";
  if (min > 0) syncTick = setTimeout(renderSyncBtn, 15000);
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
// Rebuilding a panel recreates its images, which then flash and shift the layout while
// they load again: the ones already showing are put back instead.
function setHTML(el, html) {
  const old = new Map([...el.querySelectorAll("img")].map(i => [i.src, i]));
  el.innerHTML = html;
  for (const img of el.querySelectorAll("img")) {
    const o = old.get(img.src);
    if (o) { img.replaceWith(o); old.delete(img.src); }
  }
}
function renderDrawer() {
  const it = S.items.find(i => i.id === S.drawer);
  if (!it) return closeDrawer();
  const hadFocus = $("#drawer").contains(document.activeElement); // re-rendering would drop it
  const d = detailsCache.get(it.id);
  const shots = !d ? shotsNote(it.id)
    : d.error ? `<p class="err" title="${esc(d.error)}">Couldn't get screenshots${/disabled/.test(d.error) ? " because tracker access is off (check .env)." : /not in the database/.test(d.error) ? " because this torrent is no longer in bbt." : `. ${esc(d.error)}`} <button class="linkbtn" data-act="retry">Try again</button></p>`
    : d.images.length ? `<div class="shots">${d.images.map((u, i) => `<a href="${esc(u)}" data-shot="${i}" aria-label="View screenshot ${i + 1} of ${d.images.length}"><img src="${esc(pic(u))}" alt="" loading="lazy" referrerpolicy="no-referrer"></a>`).join("")}</div>`
    : `<p class="note">No screenshots on tracker for this torrent.</p>`;
  setHTML($("#drawerIn"), `<header><span>${catsOn() ? esc(CATS[it.category] ?? `Category ${it.category}`) + " " : ""}<span style="color:var(--faint)">#${it.id}</span></span><button class="close" aria-label="Close details" data-act="close"><svg class="ico" viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg></button></header>
  <div class="big">${poster(it)}<div style="display:flex;flex-direction:column;gap:10px"><h4 tabindex="-1">${esc(clean(it.title))}</h4>
  <dl><dt>Uploaded</dt><dd>${fmtWhen(it.uploaded_at)}</dd><dt>By</dt><dd>${esc(it.uploader ?? "Anonymous")}</dd><dt>Size</dt><dd>${fmtSize(it.size_bytes)}, ${it.files} file${it.files === 1 ? "" : "s"}</dd></dl></div></div>
  <dl><dt>Seeding</dt><dd>${it.seeders}</dd><dt>Leeching</dt><dd>${it.leechers}</dd><dt>Snatched</dt><dd>${it.snatched}</dd><dt>Free download</dt><dd>${it.free ? esc(it.free) : "No"}</dd><dt>Counts updated</dt><dd>${fmtAgo(it.stats_at)} <button class="linkbtn" data-act="refresh" ${S.refreshing.has(it.id) ? "disabled" : ""}>${S.refreshing.has(it.id) ? "Refreshing…" : "Refresh"}</button></dd></dl>
  ${othersHTML(it)}
  ${historyHTML(it)}
  <div class="shotwrap">${shots}</div>
  <div class="rate-row"><div class="rate" role="group" aria-label="Your rating">${[5, 4, 3, 2, 1].map(n => `<button data-rate="${n}" class="${it.rating >= n ? "on" : ""}" aria-pressed="${it.rating === n}" aria-label="${n} star${n === 1 ? "" : "s"}">★</button>`).join("")}</div><span class="note">${it.rating ? `${it.rating} of 5. ${touch() ? "Tap" : "Click"} it again to clear.` : touch() ? "Not rated." : `Not rated. Press ${keyOf("rate")} then 1–5.`}</span></div>
  <div class="states" role="group" aria-label="Sort into">${[["", {label:"New", c:"var(--new)"}], ...Object.entries(MARKS)].map(([k, m]) => `<button data-set="${k}" style="--c:${m.c}" aria-pressed="${(it.mark ?? "") === k}"><kbd>${keyOf(k || "unmark")}</kbd>${m.label}</button>`).join("")}</div>
  <div class="acts"><button class="primary" data-act="download" ${S.downloading.has(it.id) ? "disabled" : ""}>${S.downloading.has(it.id) ? "Downloading…" : "Download .torrent"}</button>${siteUrl(it) ? `<a href="${siteUrl(it)}" target="_blank" rel="noreferrer">Open on the tracker</a>` : ""}</div>
  <p class="note">Download fetches a fresh link from tracker and marks the torrent Downloaded.</p>`);
  if (hadFocus) $("#drawerIn h4").focus();
}
function othersHTML(it) {
  if (!it.others?.length) return "";
  return `<section class="others"><h5>Other uploads of ${esc(it.release)}</h5><ul>${it.others.map(o => `<li><button class="linkbtn" data-other="${o.id}">${esc(clean(o.title))}</button><span style="color:${o.mark ? MARKS[o.mark].c : "var(--new)"}">${o.mark ? MARKS[o.mark].label : "New"}</span></li>`).join("")}</ul>
  <button class="linkbtn" data-act="allreleases">Show every upload of ${esc(it.release)}</button></section>`;
}
// Every mark change, from the server's log; loaded once per torrent until marks change.
const historyCache = new Map();
const ACTIVITY = `<div class="activity" data-activity></div>`;
let activity = null; // {today, days: Map(day -> n)}, cleared when marks change
function historyHTML(it) {
  const h = historyCache.get(it.id);
  if (!h) { loadHistory(it.id); return ""; }
  if (h.loading || h.error || !h.changes.length) return "";
  const name = m => m ? MARKS[m].label : "New";
  return `<section class="history"><h5>Sorting history, newest first</h5><ol>${h.changes.slice(0, 8).map(c => `<li><span>${name(c.mark)}</span><time>${fmtWhen(c.at)}</time></li>`).join("")}</ol>${h.changes.length > 8 ? `<p class="note">and ${h.changes.length - 8} earlier</p>` : ""}</section>`;
}
async function loadHistory(id) {
  historyCache.set(id, {loading: true});
  try { historyCache.set(id, await api("GET", `/api/torrents/${id}/history`)); }
  catch (e) { historyCache.set(id, {error: e.message}); }
  if (S.drawer === id) renderDrawer();
}
// Marks changed: other uploads' labels and histories are out of date.
function marksChanged(ids, mark) {
  historyCache.clear(); activity = null;
  for (const it of S.items) for (const o of it.others ?? []) if (ids.includes(o.id)) o.mark = mark;
}
// Everything, searched for the release code, from the day of its oldest upload if that is outside the range.
function showRelease(it) {
  if (!it?.release) return;
  const oldest = Math.min(it.uploaded_at, ...it.others.map(o => o.uploaded_at));
  if (oldest < cutoff()) {
    S.range = $("#range").value = "custom"; S.from = $("#from").value = dayKey(oldest); $("#customWrap").hidden = false;
  }
  S.q = $("#q").value = it.release;
  showView("all");
}
function openDrawer(id, {delay = 0, focus = false} = {}) {
  S.drawer = id; $("#drawer").classList.add("open");
  clearTimeout(followT);
  peekDetails(id).then(hit => {
    if (hit || S.drawer !== id || detailsBusy.has(id)) return;
    if (delay) { clearTimeout(followT); followT = setTimeout(() => { if (S.drawer === id) fetchDetails(id); }, delay); }
    else fetchDetails(id);
  });
  renderDrawer();
  if (focus) $("#drawerIn h4")?.focus();
}
// Screenshots the server saved earlier: answered at once and never a tracker request.
const peekMiss = new Set();
async function peekDetails(id) {
  if (detailsCache.has(id)) return true;
  if (detailsBusy.has(id) || peekMiss.has(id)) return false;
  try {
    const d = await api("GET", `/api/torrents/${id}/details?saved=true`);
    if (!d.images) { peekMiss.add(id); return false; }
    if (!detailsCache.has(id)) { detailsCache.set(id, d); detailsChanged(id); }
    return true;
  } catch { return false; }
}
async function fetchDetails(id) {
  detailsBusy.set(id, "load");
  // A cache hit answers at once; anything slower is a trip to the tracker.
  const slow = setTimeout(() => { detailsBusy.set(id, "tracker"); detailsChanged(id); }, SLOW_MS);
  try {
    const d = await api("GET", `/api/torrents/${id}/details`);
    detailsCache.set(id, d);
    if (d.counts) applyCounts(id, d.counts); // a fresh page brings live counts
  } catch (e) { detailsCache.set(id, {error: e.message}); }
  clearTimeout(slow); detailsBusy.delete(id);
  detailsChanged(id);
}
function applyCounts(id, counts) {
  const it = S.items.find(i => i.id === id);
  if (it) { Object.assign(it, counts); render(); }
}
// Read the details page again for live counts: one tracker request, only when asked.
async function refreshCounts(id) {
  if (S.refreshing.has(id)) return;
  S.refreshing.add(id); if (S.drawer === id) renderDrawer();
  try {
    const d = await api("POST", `/api/torrents/${id}/refresh`);
    if (d.counts) applyCounts(id, d.counts);
    else toast("The tracker page didn't show counts.", false);
  } catch (e) { toast(`Couldn't refresh counts. ${e.message}`, false); }
  S.refreshing.delete(id);
  detailsChanged(id);
}
function detailsChanged(id) {
  if (S.drawer === id) renderDrawer();
  if (SW.open && SW.cur === id) renderSwMedia();
  else if (SW.open) swPrefetch();
}
function closeDrawer() {
  const inside = $("#drawer").contains(document.activeElement);
  clearTimeout(followT); S.drawer = null; $("#drawer").classList.remove("open");
  if (inside) main.focus({preventScroll: true});
}

async function download(id) {
  // Each torrent has its own button; the server queues their tracker requests.
  if (S.downloading.has(id)) return;
  S.downloading.add(id); if (S.drawer === id) renderDrawer();
  try {
    const res = await fetch(`/api/torrents/${id}/download`);
    if (!res.ok) throw new Error(plainError((await res.json().catch(() => ({}))).error || res.statusText));
    const cd = res.headers.get("content-disposition") || "";
    const star = cd.match(/filename\*=UTF-8''([^;]+)/);
    const name = star ? decodeURIComponent(star[1]) : `tracker-${id}.torrent`;
    const url = URL.createObjectURL(await res.blob());
    const a = Object.assign(document.createElement("a"), {href: url, download: name});
    document.body.append(a); a.click(); a.remove(); URL.revokeObjectURL(url);
    toast(`Saved ${name}`, false);
    await setMark([id], "downloaded");
  } catch (e) {
    toast(`Download failed. ${e.message}`, false);
  } finally {
    S.downloading.delete(id); if (S.drawer === id) renderDrawer();
  }
}

/* ===== screenshot viewer ===== */
const V = {images: [], i: 0, opener: null, full: false, dragged: false};
const viewerOpen = () => !$("#lightbox").hidden;
function showShot() {
  const n = V.images.length, url = V.images[V.i];
  zoomReset(false);
  $("#lbImg").src = pic(url);
  $("#lbStage").scrollTo(0, 0);
  $("#lbOpen").href = url;
  $("#lbCount").textContent = `${V.i + 1} / ${n}`;
  $(".lb-prev").hidden = $(".lb-next").hidden = n < 2;
  for (const d of [-1, 1]) new Image().src = pic(V.images[(V.i + d + n) % n]); // preload neighbours
}
// With Blur posters on, screenshots stay blurred until shown on purpose (v or a click).
const viewerBlurred = () => document.body.classList.contains("blur") && !$("#lightbox").classList.contains("reveal");
function revealShots(on) {
  $("#lightbox").classList.toggle("reveal", on);
  $("#lbReveal").hidden = !viewerBlurred();
  $("#lbImg").title = viewerBlurred() ? `Click to show (${keyOf("lbReveal")})` : V.full ? `Drag to move, click to fit (${keyOf("lbFull")})` : `Click for full size (${keyOf("lbFull")})`;
}
function openViewer(images, i) {
  if (!images.length) return;
  V.images = images; V.i = i; V.opener = document.activeElement;
  $("#lightbox").hidden = false; revealShots(false); showShot(); $(".lb-close").focus();
}
function closeViewer() { zoomReset(false); setFull(false); revealShots(false); $("#lightbox").hidden = true; $("#lbImg").removeAttribute("src"); V.opener?.focus?.(); }
function setFull(on) {
  if (on) zoomReset(false);
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
  else if (e.target.id === "lbImg") { if (viewerBlurred()) revealShots(true); else if (!V.dragged && !V.touch) setFull(!V.full); }
  else if (act === "close" || e.target.id === "lightbox" || (e.target.id === "lbStage" && !V.full && !V.dragged)) closeViewer();
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
// Touch: pinch to zoom, drag to pan while zoomed, double-tap to zoom in or back out.
// A one-finger swipe changes the screenshot only at normal size.
const Z = {s:1, x:0, y:0, pts:new Map(), start:null, pinched:false, tapT:0, tapX:0, tapY:0};
function zoomApply(animate) {
  const img = $("#lbImg");
  img.style.transition = animate ? "transform .2s ease" : "none";
  img.style.transform = Z.s === 1 ? "" : `translate(${Z.x}px, ${Z.y}px) scale(${Z.s})`;
  $("#lightbox").classList.toggle("zoomed", Z.s > 1);
}
function zoomReset(animate) { Z.s = 1; Z.x = Z.y = 0; zoomApply(animate); }
// Where the image sits unzoomed (transform-origin is its top-left corner).
function zoomBase() { const r = $("#lbImg").getBoundingClientRect(); return {left: r.left - Z.x, top: r.top - Z.y}; }
// Scale to s while the picture point under (fromX, fromY) moves to (toX, toY).
function zoomAround(s, from, to, st) {
  const ux = (from.x - st.base.left - st.x) / st.s, uy = (from.y - st.base.top - st.y) / st.s;
  Z.s = s; Z.x = to.x - st.base.left - s * ux; Z.y = to.y - st.base.top - s * uy;
}
const touchMid = () => {
  const p = [...Z.pts.values()];
  return p.length > 1 ? {x: (p[0].x + p[1].x) / 2, y: (p[0].y + p[1].y) / 2, d: Math.hypot(p[0].x - p[1].x, p[0].y - p[1].y)} : {...p[0], d: 0};
};
function zoomStart() { Z.start = {s: Z.s, x: Z.x, y: Z.y, base: zoomBase(), m: touchMid()}; }
const stage = $("#lbStage");
stage.addEventListener("pointerdown", e => {
  V.touch = e.pointerType === "touch"; // touch taps also fire mouse events, so ask the pointer
  if (!V.touch || V.full) return;
  if (!Z.pts.size) { V.dragged = false; Z.pinched = false; }
  Z.pts.set(e.pointerId, {x: e.clientX, y: e.clientY});
  if (Z.pts.size > 1) Z.pinched = true;
  zoomStart();
});
stage.addEventListener("pointermove", e => {
  if (!Z.pts.has(e.pointerId)) return;
  Z.pts.set(e.pointerId, {x: e.clientX, y: e.clientY});
  const st = Z.start, m = touchMid();
  if (Z.pts.size > 1 || Math.hypot(m.x - st.m.x, m.y - st.m.y) > 8) V.dragged = true;
  if (Z.pts.size > 1 && st.m.d) { zoomAround(Math.min(6, Math.max(1, st.s * m.d / st.m.d)), st.m, m, st); zoomApply(false); }
  else if (Z.s > 1) { Z.x = st.x + m.x - st.m.x; Z.y = st.y + m.y - st.m.y; zoomApply(false); }
});
const zoomEnd = e => {
  if (!Z.pts.has(e.pointerId)) return;
  const st = Z.start;
  Z.pts.delete(e.pointerId);
  if (Z.pts.size) return zoomStart(); // one finger lifted: carry on with the other
  if (Z.s < 1.05) zoomReset(true);
  if (e.type === "pointercancel" || Z.pinched) return;
  const dx = e.clientX - st.m.x, dy = e.clientY - st.m.y;
  if (Z.s === 1 && Math.abs(dx) > 40 && Math.abs(dx) > Math.abs(dy)) return stepViewer(dx < 0 ? 1 : -1);
  if (V.dragged || e.target.id !== "lbImg") return;
  const now = Date.now();
  if (now - Z.tapT < 300 && Math.hypot(e.clientX - Z.tapX, e.clientY - Z.tapY) < 30) {
    Z.tapT = 0;
    if (Z.s > 1) zoomReset(true);
    else { const p = {x: e.clientX, y: e.clientY}; zoomAround(2.5, p, p, {s: 1, x: 0, y: 0, base: zoomBase()}); zoomApply(true); }
  } else { Z.tapT = now; Z.tapX = e.clientX; Z.tapY = e.clientY; }
};
stage.addEventListener("pointerup", zoomEnd);
stage.addEventListener("pointercancel", zoomEnd);

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

/* ===== sorting calendar: torrents sorted per day over the last 12 weeks ===== */
async function fillActivity() {
  const els = document.querySelectorAll("[data-activity]");
  if (!els.length) return;
  if (!activity) {
    try {
      const r = await api("GET", `/api/activity?days=84&offset=${-new Date().getTimezoneOffset() * 60}`);
      activity = {today: r.today, days: new Map(r.days.map(d => [d.day, d.n]))};
    } catch { return; }
  }
  const html = activityHTML(activity);
  for (const el of document.querySelectorAll("[data-activity]")) el.innerHTML = html;
}
function activityHTML({today, days}) {
  const n = d => days.get(d) ?? 0;
  const date = d => new Date(d * 864e5); // read with getUTC*: day numbers already include the zone
  // Columns are weeks, Monday on top; the last column is this week.
  const monday = today - (date(today).getUTCDay() + 6) % 7;
  const first = monday - 11 * 7;
  const max = Math.max(1, ...days.values());
  // Square root, so one big day doesn't wash out the ordinary ones.
  const level = v => v ? Math.min(4, Math.ceil(Math.sqrt(v / max) * 4)) : 0;
  const fmt = d => date(d).toLocaleDateString(undefined, {weekday: "short", day: "numeric", month: "short", timeZone: "UTC"});
  let cells = "";
  for (let d = first; d < monday + 7; d++) {
    const v = n(d);
    cells += d > today ? `<i class="future"></i>` : `<i class="l${level(v)}${d === today ? " today" : ""}" title="${fmt(d)}: ${v ? `${v} sorted` : "nothing sorted"}"></i>`;
  }
  // A streak survives a day you haven't sorted yet: it counts back from yesterday then.
  let streak = 0;
  for (let d = n(today) ? today : today - 1; n(d); d--) streak++;
  let total = 0;
  for (const v of days.values()) total += v;
  const line = streak > 1 ? `<b>${streak}-day streak</b>${n(today) ? "" : ". Sort one today to keep it"}`
    : n(today) ? `<b>${plural(n(today), "torrent")}</b> sorted today` : `Nothing sorted today yet`;
  return `<div class="cal" role="img" aria-label="${plural(total, "torrent")} sorted in the last 12 weeks">${cells}</div><p>${line} · ${plural(total, "torrent")} in 12 weeks</p>`;
}

/* ===== marking ===== */
let toastT;
function hideToast() { clearTimeout(toastT); $("#toast").classList.remove("show"); }
// The toast sits just above whatever controls are at the bottom, so it never covers them.
function placeToast() {
  const sheet = $("#drawer").classList.contains("open") && matchMedia("(max-width:760px)").matches ? $("#drawer .states") : null;
  const anchor = SW.open ? $("#swActs") : sheet ?? $(".bar-wrap");
  const top = anchor?.getBoundingClientRect().top;
  $("#toast").style.bottom = top > 0 ? `${Math.round(innerHeight - top + 10)}px` : "";
}
function toast(msg, undo = true) {
  placeToast();
  $("#toastMsg").textContent = msg; $("#undoBtn").hidden = !undo;
  $("#toast").classList.add("show"); clearTimeout(toastT);
  toastT = setTimeout(() => $("#toast").classList.remove("show"), touch() ? 8000 : 4500);
}
const leaves = mark => !mixedView() && (unmarkedView() ? mark !== null : mark !== S.view);
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
  catch (e) { toast(`Couldn't save. ${e.message}`, false); return load(); }
  pushUndo({kind: "mark", previous: res.previous, mark});
  count(mark, ids.length);
  const nodes = leaving ? ids.map(id => main.querySelector(`[data-id="${id}"]`)).filter(Boolean) : [];
  const apply = () => {
    marksChanged(ids, mark);
    if (leaving) S.items = S.items.filter(i => !ids.includes(i.id));
    else S.items.forEach(i => { if (ids.includes(i.id)) i.mark = mark; });
    render(); scrollFocus(); refreshSummary().then(() => document.querySelector(`.bin[data-mark="${mark}"]`)?.classList.add("bump"));
  };
  if (nodes.length && nodes.length < 60 && !matchMedia("(prefers-reduced-motion: reduce)").matches) { nodes.forEach(n => n.classList.add("leaving")); setTimeout(apply, 260); }
  else apply();
  const where = mark ? MARKS[mark].label : "New";
  toast(`${plural(ids.length, "torrent")} moved to ${where}`);
}
// Every change can be undone, newest first, back through the last UNDO_MAX of them.
const UNDO_MAX = 50;
function pushUndo(u) { if (u.previous?.length) { S.undos.push(u); if (S.undos.length > UNDO_MAX) S.undos.shift(); } }
let undoChain = Promise.resolve();
const undo = () => (undoChain = undoChain.then(undoOne)); // presses in a row undo one change each, in order
async function undoOne() {
  const u = S.undos.pop();
  if (!u) return toast("Nothing left to undo", false);
  const {kind, previous: prev, mark} = u;
  if (kind === "mark") count(mark, -prev.length);
  const key = kind === "mark" ? "mark" : "stars";
  const byValue = new Map();
  for (const p of prev) { const k = p[key] ?? ""; if (!byValue.has(k)) byValue.set(k, []); byValue.get(k).push(p.id); }
  try { for (const [v, ids] of byValue) await api("POST", kind === "mark" ? "/api/marks" : "/api/ratings", {ids, [key]: v === "" ? null : v}); }
  catch (e) { toast(`Undo failed. ${e.message}`, false); }
  S.focus = prev[0].id; historyCache.clear();
  await load(); scrollFocus();
  const left = S.undos.length;
  toast(left ? `Change undone. ${plural(left, "more change")} to undo` : "Change undone. Nothing left to undo", left > 0);
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
  pick(); renderTarget(); announce(n);
  if (S.drawer) openDrawer(n.id, {delay: FOLLOW_MS});
}
/* ===== bulk "mark viewed" =====
   Only unmarked torrents, and only in the lists that hold them (New, Everything),
   so a curated list can never be swept into Viewed. Big batches need a second press. */
const BULK_CONFIRM = 20;
let bulkArm = null; // {g, timer}
const bulkable = () => S.view === "new" || S.view === "all";
const bulkIdsOf = g => shown.find(x => x.key === g)?.items.filter(i => !i.mark).map(i => i.id) ?? [];
// Rows are not tab stops (j/k move focus), so a live region reads the focused torrent.
function announce(it) {
  const i = flat.indexOf(it);
  $("#announce").textContent = `${clean(it.title)}. ${fmtSize(it.size_bytes)}. ${MARKS[it.mark]?.label ?? "New"}. ${i + 1} of ${flat.length}.`;
}
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
    return toast(touch() ? `Tap Confirm to move ${plural(ids.length, "torrent")} to Viewed.` : `Move ${plural(ids.length, "torrent")} to Viewed? Press ${keyOf("bulk")} again or click Confirm.`, false);
  }
  disarmBulk();
  setMark(ids, "viewed"); // its own toast (with Undo) replaces the confirm prompt
}

/* ===== swipe mode: one new torrent at a time =====
   Pass, Later and Keep are the Viewed, View later and Download later marks.
   The pile is the New list as filtered when swipe mode opens. A card is on the pile
   while it has no mark, so Undo puts it straight back on top. */
const SW = {open:false, order:[], pending:new Set(), cur:null, photo:0, revealed:false, fetchT:null, drag:false, leaving:false, pf:null, pfStop:false};
const SW_DIR = {viewed:[-1, 0], view_later:[0, -1], download_later:[1, 0], downloaded:[0, 1]};
const SW_STAMP = {viewed:"Pass", view_later:"Later", download_later:"Keep"};
const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
function swPile() {
  const byId = new Map(S.items.map(i => [i.id, i]));
  return SW.order.map(id => byId.get(id)).filter(it => it && !it.mark && !SW.pending.has(it.id));
}
const swCurrent = () => swPile()[0];
const swSlides = it => [...new Set([it.poster_url, ...(detailsCache.get(it.id)?.images ?? [])].filter(Boolean))];
const swBlurred = () => document.body.classList.contains("blur") && !SW.revealed;
function swInfo(it) {
  return `<h2 class="title" title="${esc(it.title)}">${esc(clean(it.title))}</h2><div class="tags">${tags(it)}</div>
  <div class="sw-stats"><span><b>${fmtSize(it.size_bytes)}</b> ${plural(it.files, "file")}</span><span><b>${it.seeders}</b> seeding</span><span><b>${it.leechers}</b> leeching</span><span><b>${it.snatched}</b> snatched</span><span>${fmtAgo(it.uploaded_at)}</span>${siteUrl(it) ? `<a class="sw-site" href="${esc(siteUrl(it))}" target="_blank" rel="noreferrer">Open on the tracker<svg class="ico" viewBox="0 0 24 24" aria-hidden="true"><path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/></svg></a>` : ""}</div>`;
}
function swCardHTML(it, under) {
  const url = under ? swSlides(it)[0] : null;
  return `<article class="sw-card${under ? " under" : ""}" ${under ? 'aria-hidden="true"' : 'id="swCard"'} data-id="${it.id}">
  <div class="sw-media" style="--g:${grad(it)}"><span class="sw-code">${esc(code(it))}</span>${url ? `<img src="${esc(pic(url))}" alt="" referrerpolicy="no-referrer" draggable="false" onerror="this.remove()">` : ""}</div>
  ${under ? "" : Object.entries(SW_STAMP).map(([k, l]) => `<span class="sw-stamp" data-stamp="${k}" aria-hidden="true">${l}</span>`).join("")}
  <div class="sw-info">${swInfo(it)}</div></article>`;
}
// Preload the next few cards: their details (one tracker page each, one at a time, after
// the current card's) and every screenshot (image hosts, not the tracker).
const swBuffer = () => Math.max(0, Math.min(5, +(localStorage.getItem("bbt_sw_buffer") ?? 2) || 0));
const preloaded = new Set();
function preload(url) { if (url && !preloaded.has(url)) { preloaded.add(url); new Image().src = pic(url); } }
function swPrefetch() {
  if (!SW.open) return;
  const ahead = swPile().slice(0, 1 + swBuffer());
  for (const it of ahead.slice(1)) { swSlides(it).forEach(preload); peekDetails(it.id); }
  const now = detailsCache.get(ahead[0]?.id);
  if (SW.pf || SW.pfStop || !now || now.error) return; // the card on screen goes first, and an error stops it
  const next = ahead.slice(1).find(it => !detailsCache.has(it.id) && !detailsBusy.has(it.id));
  if (!next) return;
  SW.pf = next.id;
  fetchDetails(next.id).then(() => {
    SW.pf = null;
    if (detailsCache.get(next.id)?.error) SW.pfStop = true; // a captcha or a warning: stop asking
    swPrefetch();
  });
}
function renderSwMedia() {
  const it = swCurrent(), m = $("#swCard .sw-media");
  if (!it || !m || it.id !== SW.cur) return;
  const slides = swSlides(it), d = detailsCache.get(it.id), busy = detailsBusy.get(it.id);
  SW.photo = Math.min(SW.photo, Math.max(0, slides.length - 1));
  const url = slides[SW.photo];
  const note = busy === "tracker" ? `<span class="sw-note fetching">Fetching screenshots from the tracker…</span>`
    : busy === "load" ? `<span class="sw-note">Loading screenshots…</span>`
    : d?.error ? `<span class="sw-note">Couldn't get screenshots</span>`
    : !d ? `<span class="sw-note">Screenshots load when you stay on this card</span>`
    : !d.images.length ? `<span class="sw-note">No screenshots on the tracker</span>` : "";
  m.classList.toggle("revealed", SW.revealed);
  setHTML(m, `<span class="sw-code">${esc(code(it))}</span>${url ? `<img src="${esc(pic(url))}" alt="" referrerpolicy="no-referrer" draggable="false" onerror="this.remove()">` : ""}
  ${slides.length > 1 ? `<div class="sw-bars" aria-hidden="true">${slides.map((_, i) => `<i class="${i === SW.photo ? "on" : ""}"></i>`).join("")}</div>` : ""}${note}
  ${url ? `<button class="sw-zoom" data-sw="zoom" aria-label="View full screen"><svg class="ico" viewBox="0 0 24 24" aria-hidden="true"><path d="M14 4h6v6M10 20H4v-6M20 4l-6 6M4 20l6-6"/></svg></button>` : ""}
  ${url && swBlurred() ? `<button class="sw-reveal" data-sw="reveal">${touch() ? "Show" : `Show (${esc(keyOf("lbReveal"))})`}</button>` : ""}`);
  slides.forEach(preload);
  swPrefetch();
}
function swHint() {
  const h = $("#swHint"); if (!h) return;
  h.classList.toggle("rating", !!rateArmed && SW.open);
  h.textContent = rateArmed && SW.open ? "Rate it: press 1–5, or 0 to clear."
    : touch() ? "Tap the sides to flip screenshots, or the corner button to zoom."
    : `Space: next screenshot. Enter: full size. ${keyOf("site")}: tracker page. ${keyOf("undo")}: undo. ${keyOf("rate")}: rate. Esc: leave.`;
}
function renderSwipe() {
  if (!SW.open || SW.drag || SW.leaving) return;
  const pile = swPile(), it = pile[0], total = SW.order.length;
  $("#swCount").textContent = it ? `${total - pile.length} sorted · ${pile.length} to go` : `All ${total} sorted`;
  $("#swProg").style.width = `${total ? (total - pile.length) / total * 100 : 0}%`;
  for (const b of $("#swActs").querySelectorAll("button")) b.disabled = !it;
  if (!it) {
    SW.cur = null; clearTimeout(SW.fetchT);
    const f = filterText();
    $("#swDeck").innerHTML = `<div class="empty sw-done"><h3>All caught up</h3>${tallyHTML()}${ACTIVITY}<p>You've sorted every new torrent ${f ? `${f} ` : ""}${rangeText()}.</p>${f && (S.q || S.free) ? `<button data-act="clearfilters">Clear ${S.q && S.free ? "search and Free only" : S.q ? "search" : "Free only"}</button>` : backlogHTML()}<button data-sw="close" class="ghost">Back to the list</button></div>`;
    fillActivity();
    return;
  }
  if (it.id !== SW.cur) {
    SW.cur = it.id; SW.photo = 0; SW.revealed = false;
    $("#swDeck").innerHTML = (pile[1] ? swCardHTML(pile[1], true) : "") + swCardHTML(it, false);
    $("#swAnnounce").textContent = `${clean(it.title)}. ${fmtSize(it.size_bytes)}. ${pile.length} left.`;
    // Screenshots are a tracker request, so only a card you stay on asks for them.
    clearTimeout(SW.fetchT);
    peekDetails(it.id).then(hit => {
      if (hit || SW.cur !== it.id || detailsBusy.has(it.id)) return;
      clearTimeout(SW.fetchT); SW.fetchT = setTimeout(() => { if (SW.cur === it.id) fetchDetails(it.id); }, FOLLOW_MS);
    });
  } else {
    $("#swCard .sw-info").innerHTML = swInfo(it); // a rating changed
  }
  renderSwMedia();
}
async function openSwipe() {
  if (S.view !== "new" && S.view !== "all") { S.view = "new"; S.focus = null; S.sel.clear(); setRail(false); await load(); }
  const fresh = flat.filter(i => !i.mark).map(i => i.id);
  // Start at the focused torrent, then wrap round to the ones above it.
  const at = Math.max(0, fresh.indexOf(S.focus));
  SW.order = [...fresh.slice(at), ...fresh.slice(0, at)];
  if (!SW.order.length) return toast(`Nothing new to swipe ${rangeText()}. Pick an earlier From date.`, false);
  closeDrawer(); armRate(false);
  Object.assign(SW, {open:true, cur:null, drag:false, leaving:false}); SW.pending.clear();
  $("#swipe").hidden = false; $("#vSwipe").ariaPressed = "true";
  renderSwipe(); swHint(); $("#swipe").focus();
}
function closeSwipe() {
  if (!SW.open) return;
  SW.open = false; clearTimeout(SW.fetchT); armRate(false);
  $("#swipe").hidden = true; $("#vSwipe").ariaPressed = "false"; $("#swDeck").innerHTML = "";
  $(".top").classList.remove("more"); $("#moreBtn").ariaExpanded = "false";
  const next = swPile()[0]; if (next) S.focus = next.id;
  render(); scrollFocus(); main.focus({preventScroll: true});
}
function swDecide(mark) {
  const it = swCurrent();
  if (!it || SW.leaving) return;
  const card = $("#swCard"), [dx, dy] = SW_DIR[mark];
  SW.pending.add(it.id); S.focus = it.id;
  setMark([it.id], mark).finally(() => { SW.pending.delete(it.id); renderSwipe(); });
  const next = () => { SW.leaving = false; renderSwipe(); };
  if (!card || reduced()) return next();
  SW.leaving = true;
  card.style.transition = "transform .22s ease-in, opacity .22s ease-in";
  card.style.transform = `translate(${dx * 110}vw, ${dy * 110}vh) rotate(${dx * 16}deg)`;
  card.style.opacity = "0";
  const s = card.querySelector(`[data-stamp="${mark}"]`); if (s) s.style.opacity = 1;
  setTimeout(next, 200);
}
function swStep(d) {
  const it = swCurrent(); if (!it) return;
  const n = swSlides(it).length, i = SW.photo + d;
  if (i >= 0 && i < n) { SW.photo = i; renderSwMedia(); }
}
function swReveal() { SW.revealed = true; renderSwMedia(); }
function swFull() {
  const it = swCurrent(), slides = it ? swSlides(it) : [];
  if (!slides.length) return;
  openViewer(slides, SW.photo);
  if (SW.revealed) revealShots(true);
}
function swipeKey(e) {
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  const k = e.key;
  if ((k === "Enter" || k === " ") && e.target.closest?.("button")) return; // a focused button handles these itself
  if (!S.keys && !["Escape", "Enter", " ", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(k)) return;
  const it = swCurrent();
  if (rateArmed && /^[0-5]$/.test(k)) { armRate(false); e.preventDefault(); if (it) setRating([it.id], k === "0" ? null : +k); return; }
  if (rateArmed) armRate(false);
  const mark = k === "ArrowLeft" ? "viewed" : k === "ArrowRight" ? "download_later" : k === "ArrowUp" ? "view_later" : Object.keys(MARKS).find(m => hit(m, e));
  if (mark) { e.preventDefault(); return swDecide(mark); }
  if (k === "Escape" || hit("swipe", e)) return closeSwipe();
  if (k === " " || hit("lbNext", e)) { e.preventDefault(); swStep(1); }
  else if (hit("lbPrev", e)) swStep(-1);
  else if (k === "Enter" || hit("open", e)) { e.preventDefault(); swFull(); }
  else if (hit("lbReveal", e)) swReveal();
  else if (hit("site", e) && it && siteUrl(it)) window.open(siteUrl(it), "_blank", "noreferrer");
  else if (hit("undo", e)) undo();
  else if (hit("rate", e) && it) armRate(true);
  else if (hit("blur", e)) { setBlur(!document.body.classList.contains("blur")); renderSwMedia(); }
  else if (hit("titles", e)) setHideTitles(!document.body.classList.contains("hide-titles"));
  else if (hit("help", e)) $("#help").showModal();
}
$("#swipe").addEventListener("click", e => {
  const v = e.target.closest("[data-view]");
  if (v) { closeSwipe(); return showView(v.dataset.view); }
  const a = e.target.closest("[data-sw]")?.dataset.sw;
  if (e.target.closest("[data-act=clearfilters]")) { closeSwipe(); S.q = ""; $("#q").value = ""; S.free = false; $("#freeOnly").checked = false; return load(); }
  if (e.target.closest("[data-act=showolder]")) { closeSwipe(); return showOlder(); }
  if (a === "close") closeSwipe();
  else if (a === "reveal") swReveal();
  else if (a === "zoom") swFull();
  else if (MARKS[a]) swDecide(a);
});
// Drag the card: past the line it's a decision, short of it the card springs back.
// A tap without a drag reveals a blurred image, or flips screenshots (left third back, the rest forward).
const SW_LINE = 110;
const clamp01 = v => Math.max(0, Math.min(1, v));
$("#swDeck").addEventListener("pointerdown", e => {
  const card = e.target.closest("#swCard");
  if (!card || e.button !== 0 || e.target.closest("button, a") || SW.leaving) return;
  const x0 = e.clientX, y0 = e.clientY;
  let dx = 0, dy = 0, moved = false;
  card.setPointerCapture(e.pointerId); card.style.transition = "none";
  const stamp = (k, v) => { const s = card.querySelector(`[data-stamp="${k}"]`); if (s) s.style.opacity = v; };
  const up = () => -dy > Math.abs(dx);
  const move = m => {
    dx = m.clientX - x0; dy = m.clientY - y0;
    if (!moved && Math.hypot(dx, dy) < 6) return;
    moved = true; SW.drag = true;
    card.style.transform = `translate(${dx}px, ${dy}px) rotate(${dx / 20}deg)`;
    stamp("viewed", up() ? 0 : clamp01(-dx / SW_LINE));
    stamp("download_later", up() ? 0 : clamp01(dx / SW_LINE));
    stamp("view_later", up() ? clamp01(-dy / SW_LINE) : 0);
  };
  const done = ev => {
    card.removeEventListener("pointermove", move); card.removeEventListener("pointerup", done); card.removeEventListener("pointercancel", done);
    SW.drag = false;
    const cancelled = ev.type === "pointercancel";
    if (!moved) {
      if (cancelled || !e.target.closest(".sw-media")) return;
      if (swBlurred() && swSlides(swCurrent() ?? {}).length) return swReveal();
      const r = card.getBoundingClientRect();
      return swStep(ev.clientX < r.left + r.width / 3 ? -1 : 1);
    }
    const mark = cancelled ? null : up() && -dy > SW_LINE ? "view_later" : dx < -SW_LINE ? "viewed" : dx > SW_LINE ? "download_later" : null;
    if (mark) return swDecide(mark);
    card.style.transition = "transform .2s ease"; card.style.transform = "";
    for (const s of card.querySelectorAll("[data-stamp]")) s.style.opacity = 0;
  };
  card.addEventListener("pointermove", move); card.addEventListener("pointerup", done); card.addEventListener("pointercancel", done);
});

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
    O.msg = `Couldn't load older uploads. ${e.message}`; O.error = true;
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
  catch (e) { return toast(`Couldn't save rating. ${e.message}`, false); }
  pushUndo({kind: "rating", previous: res.previous});
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
  renderTarget(); swHint();
}

/* ===== most seeded ===== */
// Opening Most seeded reads the tracker's seeders-sorted list again (3 pages), at
// most every 10 minutes; in between it shows the copy from the last read.
const TOP_PAGES = 3;
const TOP = {busy:false, msg:""};
async function refreshTop() {
  if (TOP.busy || S.syncing) return;
  await load(); // the copy from the last read, straight away
  if (S.view !== "top") return;
  TOP.busy = true;
  if (!S.items.length) render();
  else banner(`Reading the ${TOP_PAGES} most seeded pages from the tracker…`);
  try {
    const r = await api("POST", "/api/top");
    TOP.msg = r.status === "cooling_down" ? `Read less than 10 minutes ago. bbt reads it again in ${Math.ceil(r.seconds_left / 60)} min.` : "";
  } catch (e) {
    TOP.msg = `Couldn't read the tracker. ${e.message}`;
    if (S.view === "top") toast(TOP.msg, false);
  } finally {
    TOP.busy = false;
    if (S.view === "top") { banner(null); await load(); }
  }
}

/* ===== sync ===== */
async function doSync() {
  if (S.syncing) return;
  if (syncWaitMin() > 0) return toast(`bbt waits 10 minutes between syncs. Try again in ${syncWaitMin()} min.`, false);
  S.syncing = true;
  const prog = $("#syncProg"), text = $("#syncText"), btn = $("#syncBtn");
  prog.hidden = false; prog.classList.add("busy"); btn.disabled = true;
  text.textContent = "Syncing: reading up to 12 pages, about 40 seconds…";
  const older = $("#older"); if (older) older.outerHTML = olderHTML();
  let msg;
  try {
    const r = await api("POST", "/api/sync");
    msg = r.status === "cooling_down"
      ? `Sync again in ${Math.ceil(r.seconds_left / 60)} min: bbt waits 10 minutes between syncs.`
      : `Synced just now: ${r.new_count} new from ${plural(r.pages, "page")}.${r.complete ? "" : ` It ${stopText(r.stop)}.`}`;
  } catch (e) {
    msg = `Sync failed. ${e.message}`;
  } finally {
    S.syncing = false; prog.hidden = true; prog.classList.remove("busy"); btn.disabled = false;
  }
  await load();
  text.textContent = msg;
  checkSyncWait();
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
  if (e.target.closest("[data-act=top]")) { TOP.msg = ""; return refreshTop(); }
  const jump = e.target.closest("[data-view]"); if (jump) return showView(jump.dataset.view);
  if (e.target.closest("[data-act=clearfilters]")) { S.q = ""; $("#q").value = ""; S.free = false; $("#freeOnly").checked = false; return load(); }
  if (e.target.closest("[data-act=showolder]")) return showOlder();
  if (e.target.closest("[data-act=allcats]")) { S.cats = new Set(Object.keys(CATS).map(Number)); saveCats(); return load(); }
  if (e.target.closest("[data-act=older]")) { O.error = false; return loadOlder(); }
  const bulk = e.target.closest("[data-bulk]"); if (bulk) return bulkViewed(bulk.dataset.bulk);
  const r = e.target.closest("[data-id]"); if (!r) return;
  if (e.target.matches(".selbox")) return; // the label forwards its click to the checkbox
  const id = +r.dataset.id;
  if (e.target.classList.contains("sel")) { S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id); renderTarget(); return; }
  // First click focuses; clicking the focused torrent again opens its details.
  // On touch the first row is focused before anything is tapped, so its first tap only picks it.
  if (S.focus === id && S.drawer !== id && !blind()) return openDrawer(id);
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
  const b = e.target.closest("[data-set]"); if (b) return setMark([S.drawer], b.dataset.set || null);
  if (e.target.closest("[data-act=close]")) return closeDrawer();
  if (e.target.closest("[data-act=retry]")) { detailsCache.delete(S.drawer); return openDrawer(S.drawer); }
  if (e.target.closest("[data-act=download]")) return download(S.drawer);
  if (e.target.closest("[data-act=refresh]")) return refreshCounts(S.drawer);
  const other = e.target.closest("[data-other]");
  if (other) {
    const id = +other.dataset.other;
    if (S.items.some(i => i.id === id)) { S.focus = id; render(); scrollFocus(); return openDrawer(id, {focus: true}); }
    return showRelease(S.items.find(i => i.id === S.drawer));
  }
  if (e.target.closest("[data-act=allreleases]")) return showRelease(S.items.find(i => i.id === S.drawer));
});
function showView(v) { S.view = v; S.focus = null; S.picked = false; S.sel.clear(); main.scrollTop = 0; setRail(false); v === "top" ? refreshTop() : load(); }
// Below 760px the rail is a sheet and the toolbar controls fold into View.
function setRail(on) {
  $("#rail").classList.toggle("open", on); $("#scrim").hidden = !on; $("#railBtn").ariaExpanded = String(on);
  if (on) $("#views [aria-current=true]")?.focus();
}
$("#railBtn").onclick = () => setRail(true);
$("#railClose").onclick = () => { setRail(false); $("#railBtn").focus(); };
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
$("#vSwipe").onclick = openSwipe;
$("#syncBtn").onclick = doSync;
$("#helpBtn").onclick = () => $("#help").showModal();
$("#setBtn").onclick = () => $("#settings").showModal();
$("#undoBtn").onclick = undo;
$("#sOpen").onchange = e => localStorage.setItem("bbt_sync_on_open", e.target.checked ? "1" : "0");
$("#sSticky").onchange = e => { S.sticky = e.target.checked; localStorage.setItem("bbt_sticky", S.sticky ? "1" : "0"); load(); };
$("#sKeys").onchange = e => { S.keys = e.target.checked; localStorage.setItem("bbt_keys", S.keys ? "1" : "0"); };
$("#sBlur").onchange = e => localStorage.setItem("bbt_blur", e.target.checked ? "1" : "0");
$("#sBuffer").onchange = e => localStorage.setItem("bbt_sw_buffer", e.target.value);
$("#sHide").onchange = e => localStorage.setItem("bbt_hide_titles", e.target.checked ? "1" : "0");
document.addEventListener("keydown", e => {
  if (capturing) return; // the key editor takes this one
  const k = e.key;
  if (viewerOpen()) {
    if (k === "ArrowRight" || hit("lbNext", e)) { e.preventDefault(); stepViewer(1); }
    else if (k === "ArrowLeft" || hit("lbPrev", e)) { e.preventDefault(); stepViewer(-1); }
    // Up and down (j/k or the arrows): scroll a full-size image, otherwise go to the next or previous one.
    else if (k === "ArrowDown" || k === "ArrowUp" || hit("next", e) || hit("prev", e)) {
      e.preventDefault();
      const d = k === "ArrowDown" || hit("next", e) ? 1 : -1, st = $("#lbStage");
      if (V.full && st.scrollHeight > st.clientHeight) st.scrollBy({top: d * st.clientHeight * 0.4, behavior: "smooth"});
      else stepViewer(d);
    }
    else if (hit("lbFull", e)) { e.preventDefault(); setFull(!V.full); }
    else if (hit("lbReveal", e)) { e.preventDefault(); revealShots(true); }
    else if (k === "Escape") closeViewer();
    return;
  }
  if (SW.open) { if (!document.querySelector("dialog[open]")) swipeKey(e); return; }
  if (e.target.matches?.("input[type=search],input[type=text],input[type=number],input[type=date],select")) { if (k === "Escape") e.target.blur(); return; }
  if (document.querySelector("dialog[open]") || e.metaKey || e.ctrlKey || e.altKey) return;
  const step = S.grid ? gridCols() : 1;
  // WCAG 2.1.4: single-character shortcuts can be turned off in Settings.
  if (!S.keys && !["Escape", "Enter", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(k)) return;
  if (rateArmed && /^[0-5]$/.test(k)) {
    armRate(false); e.preventDefault();
    return setRating(targets(), k === "0" ? null : +k);
  }
  if (rateArmed) armRate(false);
  if (hit("rate", e)) { if (targets().length) armRate(true); }
  else if (k === "ArrowDown" || hit("next", e)) { e.preventDefault(); move(k === "ArrowDown" ? step : 1); }
  else if (k === "ArrowUp" || hit("prev", e)) { e.preventDefault(); move(k === "ArrowUp" ? -step : -1); }
  else if (S.grid && k === "ArrowRight") move(1);
  else if (S.grid && k === "ArrowLeft") move(-1);
  else if (hit("bulk", e)) bulkViewed(groupOf(S.focus));
  else if (Object.keys(MARKS).some(m => hit(m, e))) setMark(targets(), Object.keys(MARKS).find(m => hit(m, e)), {advance:true});
  else if (hit("unmark", e)) setMark(targets(), null);
  else if (hit("select", e) && S.focus) { const id = S.focus; S.sel.has(id) ? S.sel.delete(id) : S.sel.add(id); const c = main.querySelector(`[data-id="${id}"] .sel`); if (c) c.checked = S.sel.has(id); renderTarget(); }
  else if ((k === "Enter" || hit("open", e)) && S.focus) openDrawer(S.focus, {focus: true});
  else if (k === "Escape") { disarmBulk(); setRail(false); closeDrawer(); if (S.sel.size) { S.sel.clear(); render(); } }
  else if (hit("site", e) && S.focus) { const it = S.items.find(i => i.id === S.focus); if (it && siteUrl(it)) window.open(siteUrl(it), "_blank", "noreferrer"); }
  else if (hit("undo", e)) undo();
  else if (hit("blur", e)) setBlur(!document.body.classList.contains("blur"));
  else if (hit("titles", e)) setHideTitles(!document.body.classList.contains("hide-titles"));
  else if (hit("search", e)) { e.preventDefault(); $("#q").focus(); }
  else if (hit("help", e)) $("#help").showModal();
  else if (hit("swipe", e)) openSwipe();
});

/* ===== key editor (in the Keyboard dialog) ===== */
let capturing = null; // the button waiting for a key
const ACTION_NAMES = {
  next:"Next torrent", prev:"Previous torrent", open:"Open details", site:"Open on the tracker", search:"Search",
  viewed:"Viewed", view_later:"View later", download_later:"Download later", downloaded:"Downloaded",
  unmark:"Back to New", bulk:"Mark the day viewed", undo:"Undo", rate:"Rate", select:"Select",
  blur:"Blur posters", titles:"Blur titles", help:"Keyboard help", swipe:"Swipe mode",
  lbPrev:"Previous screenshot", lbNext:"Next screenshot", lbReveal:"Show a blurred screenshot", lbFull:"Full size or fit",
};
function saveKeys() {
  const changed = Object.fromEntries(Object.entries(KEYS).filter(([a, k]) => k !== KEY_DEFAULTS[a]));
  if (Object.keys(changed).length) localStorage.setItem("bbt_keymap", JSON.stringify(changed));
  else localStorage.removeItem("bbt_keymap");
}
function renderKeys() {
  for (const b of document.querySelectorAll("#help [data-k]")) {
    const a = b.dataset.k, changed = KEYS[a] !== KEY_DEFAULTS[a];
    b.textContent = keyOf(a);
    b.classList.toggle("changed", changed);
    b.setAttribute("aria-label", `${ACTION_NAMES[a]}: ${keyOf(a)}${changed ? ` (default ${keyName(KEY_DEFAULTS[a])})` : ""}. Press to change.`);
    b.title = changed ? `Default: ${keyName(KEY_DEFAULTS[a])}` : "Click to change";
  }
  $("#kbViewerJK").textContent = `${keyOf("next")} ${keyOf("prev")}`;
  $("#kbReset").disabled = Object.entries(KEYS).every(([a, k]) => k === KEY_DEFAULTS[a]);
  $("#q").placeholder = touch() ? "Search titles or uploaders" : `Search titles or uploaders  ( ${keyOf("search")} )`;
  $("#helpBtn").title = touch() ? "How to use bbt by touch" : `Keyboard shortcuts (${keyOf("help")})`;
  $("#helpBtn").textContent = touch() ? "Help" : "?";
  $("#lbReveal").textContent = touch() ? "Show image" : `Show image (${keyOf("lbReveal")})`;
  $("#vSwipe").title = `Swipe through new torrents (${keyOf("swipe")})`;
  swHint();
  renderBins();
  if (S.drawer) renderDrawer();
}
const kbMsg = msg => { $("#kbMsg").textContent = msg; };
function stopCapture(msg) {
  if (!capturing) return;
  capturing.classList.remove("capturing"); capturing = null;
  renderKeys();
  if (msg !== undefined) kbMsg(msg);
}
function startCapture(b) {
  stopCapture();
  capturing = b; b.classList.add("capturing"); b.textContent = "Press a key";
  kbMsg(`Press the new key for ${ACTION_NAMES[b.dataset.k]}. Esc cancels.`);
}
function bindKey(a, k) {
  // A key does one thing per place: if another action had it, that action takes this one's old key.
  const other = Object.keys(KEYS).find(o => o !== a && KEYS[o] === k && inViewer(o) === inViewer(a));
  const was = KEYS[a];
  KEYS[a] = k;
  if (other) KEYS[other] = was;
  saveKeys();
  return other ? `${ACTION_NAMES[a]} is now ${keyName(k)}. ${ACTION_NAMES[other]} moved to ${keyName(was)}.` : `${ACTION_NAMES[a]} is now ${keyName(k)}.`;
}
// Capture phase, so the dialog's own Esc and the button's Space/Enter don't fire first.
document.addEventListener("keydown", e => {
  if (!capturing) return;
  if (["Shift", "Alt", "Meta", "Control", "CapsLock"].includes(e.key)) return; // wait for the real key
  e.preventDefault(); e.stopPropagation();
  const b = capturing, a = b.dataset.k;
  if (e.key === "Escape") return stopCapture("No change.");
  if (e.metaKey || e.ctrlKey || e.altKey) return kbMsg("Use one key, without ⌘, Ctrl or Option. Esc cancels.");
  if (e.key.length !== 1) return kbMsg(`${e.key} is fixed. Pick a letter, digit or symbol. Esc cancels.`);
  const msg = bindKey(a, e.key);
  stopCapture(msg);
  b.focus();
}, true);
$("#help").addEventListener("click", e => {
  const b = e.target.closest("[data-k]");
  if (b) return capturing === b ? stopCapture("No change.") : startCapture(b);
  if (e.target.closest("#kbReset")) {
    stopCapture();
    KEYS = {...KEY_DEFAULTS}; saveKeys(); renderKeys();
    kbMsg("All keys are back to their defaults.");
  }
});
$("#help").addEventListener("close", () => { stopCapture(); kbMsg("Click a key to change it."); });

/* ===== boot ===== */
(async function boot() {
  renderKeys();
  setBlur(localStorage.getItem("bbt_blur") !== "0");
  $("#sOpen").checked = localStorage.getItem("bbt_sync_on_open") === "1";
  $("#sSticky").checked = S.sticky;
  $("#sKeys").checked = S.keys;
  $("#sBuffer").value = swBuffer();
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
  checkSyncWait();
  maybeSyncOnOpen();
})();
