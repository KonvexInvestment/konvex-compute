import {
  escapeHtml,
  fmtCompactUsd,
  fmtNum1,
  fmtPct,
  fmtSignedNum1,
  fmtSignedUsd,
  fmtUsd,
  renderCurveShiftChart,
  renderDistributionChart,
  renderForwardChart,
  renderStrikeHistoryChart,
} from "./charts.js";
import { annotateTerms, friendlyError, installTermTooltips } from "./glossary.js";

function tauriInvoke(cmd, args = {}) {
  const core = window.__TAURI__ && window.__TAURI__.core;
  if (!core || typeof core.invoke !== "function") {
    return Promise.reject(new Error("Tauri IPC nicht verfügbar."));
  }
  return core.invoke(cmd, args);
}

function tauriListen(eventName, handler) {
  const ev = window.__TAURI__ && window.__TAURI__.event;
  if (!ev || typeof ev.listen !== "function") return Promise.resolve(() => {});
  return ev.listen(eventName, handler);
}

const state = {
  data: null,
  activeView: "curve",
  selectedGpu: "H100",
  selectedExpiryByGpu: {},
  historyWindow: "90",
  compareMode: "none", // none | 1d | 7d | 30d | custom
  customCompareSnapshot: null,
  customCompareId: "",
  showIqr: true,
  showOuter: false,
  showMeanTrimmed: false,
  showMeanRaw: false,
  selectedStrikeTokenId: null,
  strikeReqSeq: 0,
  compareReqSeq: 0,
  toastTimer: null,
  // Lokal angestoßene Aktionen (bis das Backend-Busy-Event eintrifft)
  pending: { refresh: false, backfill: false },
  // Letztes compute:busy-Event. Sobald eines da ist, gilt nur noch dieses für den
  // Busy-Zustand: Statusantworten können älter sein als das letzte Busy-Event.
  busy: null,
  // Fehler aus einer Nutzeraktion: { title, raws }
  actionError: null,
  // Signatur der zuletzt weggeklickten Abruf-Hinweise
  dismissedFetchSig: "",
  // Signatur des aktuell angezeigten Banners
  bannerSig: "",
};

const VIEWS = ["curve", "compare", "matrix", "store"];
const GPU_KEYS = ["H100", "H200", "B200", "B300"];
const HIST_WINDOWS = ["30", "90", "all"];
const SETTINGS_KEY = "konvex-compute-ui";

function loadSettings() {
  let saved = {};
  try {
    saved = JSON.parse(localStorage.getItem(SETTINGS_KEY) || "{}") || {};
  } catch {
    saved = {};
  }
  if (VIEWS.includes(saved.view)) state.activeView = saved.view;
  if (GPU_KEYS.includes(saved.gpu)) state.selectedGpu = saved.gpu;
  if (HIST_WINDOWS.includes(saved.historyWindow)) state.historyWindow = saved.historyWindow;
  for (const key of ["showIqr", "showOuter", "showMeanTrimmed", "showMeanRaw"]) {
    if (typeof saved[key] === "boolean") state[key] = saved[key];
  }
  if (saved.expiryByGpu && typeof saved.expiryByGpu === "object") {
    for (const [gpu, slug] of Object.entries(saved.expiryByGpu)) {
      if (GPU_KEYS.includes(gpu) && typeof slug === "string") state.selectedExpiryByGpu[gpu] = slug;
    }
  }
}

function saveSettings() {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify({
      view: state.activeView,
      gpu: state.selectedGpu,
      historyWindow: state.historyWindow,
      showIqr: state.showIqr,
      showOuter: state.showOuter,
      showMeanTrimmed: state.showMeanTrimmed,
      showMeanRaw: state.showMeanRaw,
      expiryByGpu: state.selectedExpiryByGpu,
    }));
  } catch {
    /* Speicher voll oder gesperrt: Einstellungen gelten dann nur für diese Sitzung */
  }
}

const els = {
  freshnessDot: document.getElementById("freshness-dot"),
  freshnessText: document.getElementById("freshness-text"),
  errorBanner: document.getElementById("error-banner"),
  themeToggle: document.getElementById("theme-toggle"),
  refreshBtn: document.getElementById("refresh-btn"),
  viewTablist: document.getElementById("view-tablist"),
  gpuBar: document.getElementById("gpu-bar"),
  gpuTablist: document.getElementById("gpu-tablist"),
  panels: {
    curve: document.getElementById("panel-curve"),
    compare: document.getElementById("panel-compare"),
    matrix: document.getElementById("panel-matrix"),
    store: document.getElementById("panel-store"),
  },
  heroKicker: document.getElementById("hero-kicker"),
  heroPrice: document.getElementById("hero-price"),
  heroChanges: document.getElementById("hero-changes"),
  kpiFrontLabel: document.getElementById("kpi-front-label"),
  kpiFrontVal: document.getElementById("kpi-front-val"),
  kpiFrontSub: document.getElementById("kpi-front-sub"),
  kpiMidLabel: document.getElementById("kpi-mid-label"),
  kpiMidVal: document.getElementById("kpi-mid-val"),
  kpiMidSub: document.getElementById("kpi-mid-sub"),
  kpiBackLabel: document.getElementById("kpi-back-label"),
  kpiBackVal: document.getElementById("kpi-back-val"),
  kpiBackSub: document.getElementById("kpi-back-sub"),
  kpiLiqVal: document.getElementById("kpi-liq-val"),
  kpiLiqSub: document.getElementById("kpi-liq-sub"),
  cmpBtn1d: document.getElementById("cmp-btn-1d"),
  cmpBtn7d: document.getElementById("cmp-btn-7d"),
  cmpBtn30d: document.getElementById("cmp-btn-30d"),
  compareSelect: document.getElementById("compare-select"),
  optIqr: document.getElementById("opt-iqr"),
  optOuter: document.getElementById("opt-outer"),
  optMeanTrimmed: document.getElementById("opt-mean-trimmed"),
  optMeanRaw: document.getElementById("opt-mean-raw"),
  freshnessBadge: document.getElementById("freshness-badge"),
  chartLegend: document.getElementById("chart-legend"),
  matrixHeadRow: document.getElementById("matrix-head-row"),
  forwardChartStage: document.getElementById("forward-chart-stage"),
  forwardChartSvg: document.getElementById("forward-chart-svg"),
  chartTooltip: document.getElementById("chart-tooltip"),
  expiryStrip: document.getElementById("expiry-strip"),
  distTitle: document.getElementById("dist-title"),
  distSub: document.getElementById("dist-sub"),
  distSkewBadge: document.getElementById("dist-skew-badge"),
  distMetrics: document.getElementById("dist-metrics"),
  skewNote: document.getElementById("skew-note"),
  distChartStage: document.getElementById("dist-chart-stage"),
  distChartSvg: document.getElementById("dist-chart-svg"),
  distTooltip: document.getElementById("dist-tooltip"),
  strikesCaption: document.getElementById("strikes-caption"),
  strikesQualityPill: document.getElementById("strikes-quality-pill"),
  strikesTbody: document.getElementById("strikes-tbody"),
  strikeHistoryBox: document.getElementById("strike-history-box"),
  strikeHistoryTitle: document.getElementById("strike-history-title"),
  strikeHistorySub: document.getElementById("strike-history-sub"),
  strikeHistorySvg: document.getElementById("strike-history-svg"),
  closeStrikeHistory: document.getElementById("close-strike-history"),
  compareKpiGrid: document.getElementById("compare-kpi-grid"),
  shiftChartTitle: document.getElementById("shift-chart-title"),
  shiftChartSub: document.getElementById("shift-chart-sub"),
  shiftChartStage: document.getElementById("shift-chart-stage"),
  shiftChartSvg: document.getElementById("shift-chart-svg"),
  shiftTooltip: document.getElementById("shift-tooltip"),
  compareTableSub: document.getElementById("compare-table-sub"),
  compareTbody: document.getElementById("compare-tbody"),
  runBackfillQuick: document.getElementById("run-backfill-quick"),
  matrixTbody: document.getElementById("matrix-tbody"),
  extraSpotsRow: document.getElementById("extra-spots-row"),
  extraSpotsPills: document.getElementById("extra-spots-pills"),
  storeKpiGrid: document.getElementById("store-kpi-grid"),
  storeDbPath: document.getElementById("store-db-path"),
  storeTbody: document.getElementById("store-tbody"),
  runBackfillBtn: document.getElementById("run-backfill-btn"),
  toast: document.getElementById("toast"),
};

function fmtTs(iso) {
  if (!iso) return "noch nie";
  const dt = new Date(iso);
  if (Number.isNaN(dt.getTime())) return iso;
  return new Intl.DateTimeFormat("de-DE", {
    day: "2-digit",
    month: "2-digit",
    year: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(dt);
}

function fmtDateShort(iso) {
  if (!iso) return "k. A.";
  const [y, m, d] = iso.slice(0, 10).split("-").map(Number);
  return new Intl.DateTimeFormat("de-DE", {
    day: "2-digit",
    month: "2-digit",
    year: "2-digit",
    timeZone: "UTC",
  }).format(new Date(Date.UTC(y, m - 1, d)));
}

function deltaClass(v, digits = 2) {
  if (v === null || v === undefined) return "";
  // Werte, die auf die angezeigten Stellen zu 0 runden, nicht als Verlust einfärben
  const f = 10 ** digits;
  return Math.round(v * f) / f >= 0 ? "is-pos" : "is-neg";
}

function fmtAgo(iso) {
  const ts = new Date(iso).getTime();
  if (Number.isNaN(ts)) return "";
  const min = Math.floor((Date.now() - ts) / 60000);
  if (min < 1) return "gerade eben";
  if (min < 60) return `vor ${min} Min.`;
  const hours = Math.floor(min / 60);
  if (hours < 24) return `vor ${hours} Std.`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "vor 1 Tag" : `vor ${days} Tagen`;
}

function fmtUntil(iso) {
  const ts = new Date(iso).getTime();
  if (Number.isNaN(ts)) return "";
  const min = Math.ceil((ts - Date.now()) / 60000);
  return min <= 0 ? "gleich" : `in ${min} Min.`;
}

/* ---------- Hinweis-Banner (verständlich und schließbar) ---------- */

function setActionError(title, raws) {
  const list = (Array.isArray(raws) ? raws : [raws]).map((r) => String(r)).filter(Boolean);
  state.actionError = { title, raws: list };
  renderBanner();
}

function clearActionError() {
  if (!state.actionError) return;
  state.actionError = null;
  renderBanner();
}

function renderBanner() {
  let title = "";
  let raws = [];
  let tone = "error";
  if (state.actionError) {
    ({ title, raws } = state.actionError);
  } else {
    const errs = (state.data && state.data.last_errors) || [];
    // Weggeklickt gilt nur, bis die Quellen wieder fehlerfrei sind
    if (!errs.length) state.dismissedFetchSig = "";
    if (errs.length && errs.join("|") !== state.dismissedFetchSig) {
      title = "Letzter Abruf unvollständig. Für diese Quellen wird der zuletzt gespeicherte Stand angezeigt:";
      raws = errs;
      tone = "warn";
    }
  }
  if (!title) {
    state.bannerSig = "";
    els.errorBanner.hidden = true;
    els.errorBanner.innerHTML = "";
    return;
  }
  // Nur bei geändertem Inhalt neu bauen (sonst klappen offene Details alle 30 s zu)
  const sig = `${tone}|${title}|${raws.join("|")}`;
  if (sig === state.bannerSig && !els.errorBanner.hidden) return;
  state.bannerSig = sig;
  const friendly = [...new Set(raws.map(friendlyError))];
  els.errorBanner.dataset.tone = tone;
  els.errorBanner.innerHTML = `
    <div class="alert-body">
      <strong class="alert-title">${escapeHtml(title)}</strong>
      ${friendly.length ? `<ul class="alert-list">${friendly.map((f) => `<li>${escapeHtml(f)}</li>`).join("")}</ul>` : ""}
      ${raws.length ? `<details class="alert-details"><summary>Technische Details</summary><pre>${escapeHtml(raws.join("\n"))}</pre></details>` : ""}
    </div>
    <button type="button" class="alert-close" aria-label="Hinweis schließen">×</button>
  `;
  els.errorBanner.hidden = false;
  els.errorBanner.querySelector(".alert-close").addEventListener("click", () => {
    if (state.actionError) {
      state.actionError = null;
    } else if (state.data) {
      state.dismissedFetchSig = (state.data.last_errors || []).join("|");
    }
    renderBanner();
  });
}

/* ---------- Laufende Hintergrundabrufe sichtbar machen ---------- */

function isRefreshing() {
  return state.pending.refresh || !!(state.data && state.data.refreshing);
}

function isBackfilling() {
  return state.pending.backfill || !!(state.data && state.data.backfilling);
}

function setBtnBusy(btn, busy, idleLabel, busyLabel) {
  btn.disabled = busy;
  btn.classList.toggle("is-loading", busy);
  btn.setAttribute("aria-busy", busy ? "true" : "false");
  const label = btn.querySelector(".ka-btn-label");
  if (label) label.textContent = busy ? busyLabel : idleLabel;
  else btn.textContent = busy ? busyLabel : idleLabel;
}

function renderBusy() {
  setBtnBusy(els.refreshBtn, isRefreshing(), "Aktualisieren", "Aktualisiere …");
  const bf = isBackfilling();
  setBtnBusy(els.runBackfillBtn, bf, "14-Tage-Backfill aus CLOB laden", "Backfill läuft …");
  setBtnBusy(els.runBackfillQuick, bf, "14T-Backfill aktualisieren", "Backfill läuft …");
}

function showToast(msg) {
  els.toast.textContent = msg;
  els.toast.hidden = false;
  if (state.toastTimer) clearTimeout(state.toastTimer);
  state.toastTimer = setTimeout(() => { els.toast.hidden = true; }, 4500);
}

function getCurrentGpu() {
  return state.data && state.data.gpus ? state.data.gpus[state.selectedGpu] || null : null;
}

function getSelectedExpiry(gpu) {
  if (!gpu || !gpu.expiries || !gpu.expiries.length) return null;
  const saved = state.selectedExpiryByGpu[gpu.meta.key];
  if (saved) {
    const match = gpu.expiries.find((e) => e.slug === saved);
    if (match) return match;
  }
  const mid = gpu.expiries[Math.min(4, Math.floor(gpu.expiries.length / 2))];
  state.selectedExpiryByGpu[gpu.meta.key] = mid.slug;
  return mid;
}

function resolveCompareCurve(gpu) {
  if (!gpu || state.compareMode === "none") return null;
  if (state.compareMode === "1d" && gpu.compare_1d) {
    return {
      label: `1T (${fmtDateShort(gpu.compare_1d.snapshot_date)})`,
      shortLabel: "Gestern (1T)",
      spot_price: gpu.compare_1d.spot_price,
      expiries: gpu.compare_1d.expiries,
    };
  }
  if (state.compareMode === "7d" && gpu.compare_7d) {
    return {
      label: `1W (${fmtDateShort(gpu.compare_7d.snapshot_date)})`,
      shortLabel: "Vor 7T (1W)",
      spot_price: gpu.compare_7d.spot_price,
      expiries: gpu.compare_7d.expiries,
    };
  }
  if (state.compareMode === "30d" && gpu.compare_30d) {
    return {
      label: `1M (${fmtDateShort(gpu.compare_30d.snapshot_date)})`,
      shortLabel: "Vor 30T (1M)",
      spot_price: gpu.compare_30d.spot_price,
      expiries: gpu.compare_30d.expiries,
    };
  }
  if (state.compareMode === "custom" && state.customCompareSnapshot) {
    return {
      label: fmtTs(state.customCompareSnapshot.captured_at),
      shortLabel: "Snapshot",
      spot_price: state.customCompareSnapshot.spot_price,
      expiries: state.customCompareSnapshot.expiries,
    };
  }
  return null;
}

/* ---------- Laufzeiten relativ zu heute statt fest im Code ---------- */

function todayIso() {
  return (state.data && state.data.today_utc) || new Date().toISOString().slice(0, 10);
}

function addMonthsIso(iso, months) {
  const [y, m, d] = iso.slice(0, 10).split("-").map(Number);
  return new Date(Date.UTC(y, m - 1 + months, Math.min(d, 28))).toISOString().slice(0, 10);
}

function nearestByDate(items, getDate, targetIso) {
  const target = Date.parse(targetIso);
  let best = null;
  let bestDist = Infinity;
  for (const item of items) {
    const dist = Math.abs(Date.parse(getDate(item)) - target);
    if (dist < bestDist) { bestDist = dist; best = item; }
  }
  return best;
}

/* ---------- Legende zum Hauptchart ---------- */

function legendSwatch({ kind, color, dash }) {
  if (kind === "band") {
    return `<svg class="legend-swatch" viewBox="0 0 22 10" aria-hidden="true"><rect x="0" y="1" width="22" height="8" rx="2" fill="${color}"/></svg>`;
  }
  const dashAttr = dash ? ` stroke-dasharray="${dash}"` : "";
  return `<svg class="legend-swatch" viewBox="0 0 22 10" aria-hidden="true"><line x1="1" y1="5" x2="21" y2="5" stroke="${color}" stroke-width="2"${dashAttr}/></svg>`;
}

function renderChartLegend(gpu, compareCurve) {
  const color = gpu.meta.color;
  const band = gpu.meta.band_rgb;
  const items = [
    { term: "spot", label: "Spot-Historie (Ornn)", sw: { kind: "line", color } },
    { term: "median", label: "Forward-Kurve (Median)", sw: { kind: "line", color, dash: "3 4" } },
    { term: "spot", label: "Spotpreis heute", sw: { kind: "line", color: "var(--text-dimmed)", dash: "4 4" } },
  ];
  if (state.showIqr) items.push({ term: "iqr", label: "P25 bis P75", sw: { kind: "band", color: `rgba(${band}, 0.34)` } });
  if (state.showOuter) items.push({ term: "outer", label: "P10 bis P90", sw: { kind: "band", color: `rgba(${band}, 0.16)` } });
  if (state.showMeanTrimmed) items.push({ term: "meanTrimmed", label: "Kern-Mean", sw: { kind: "line", color: "#22d3ee", dash: "4 3" } });
  if (state.showMeanRaw) items.push({ term: "meanRaw", label: "Roh-Mean", sw: { kind: "line", color: "var(--text-muted)", dash: "2 3" } });
  if (compareCurve) {
    items.push({ term: "compareCurve", label: `Vergleich: ${compareCurve.label}`, sw: { kind: "line", color: "var(--accent-amber)", dash: "6 4" } });
  }
  els.chartLegend.innerHTML = `
    ${items.map((it) => `<span class="legend-item">${legendSwatch(it.sw)}<span data-term="${it.term}">${escapeHtml(it.label)}</span></span>`).join("")}
    <span class="legend-hint">Klick rechts von „Heute“ wählt einen Liefermonat</span>
  `;
}

function renderTopbar() {
  const data = state.data;
  if (!data) return;

  els.freshnessDot.classList.remove("is-stale", "is-busy");
  const interval = data.interval_min || 15;
  const parts = [];
  if (data.last_captured_at) parts.push(`Letzter Datenabruf: ${fmtTs(data.last_captured_at)}`);
  parts.push(`Automatische Aktualisierung alle ${interval} Min.`);
  parts.push(`${data.db_stats.total_snapshots} gespeicherte Snapshots`);
  els.freshnessBadge.title = parts.join(" · ");

  if (isRefreshing() || isBackfilling()) {
    els.freshnessDot.classList.add("is-busy");
    els.freshnessText.textContent = isBackfilling() && !isRefreshing() ? "Backfill läuft …" : "Aktualisiere …";
  } else if (data.last_captured_at) {
    const ageMin = (Date.now() - new Date(data.last_captured_at).getTime()) / 60000;
    if (ageMin > interval * 2) els.freshnessDot.classList.add("is-stale");
    const next = data.next_refresh_at ? ` · nächstes Update ${fmtUntil(data.next_refresh_at)}` : "";
    els.freshnessText.textContent = `Stand ${fmtAgo(data.last_captured_at)}${next}`;
  } else {
    els.freshnessDot.classList.add("is-stale");
    els.freshnessText.textContent = "Noch kein Datenstand";
  }

  renderBanner();
  renderBusy();
}

function renderGpuTabs() {
  const data = state.data;
  if (!data) return;
  const tabs = Array.from(els.gpuTablist.querySelectorAll('[role="tab"]'));
  for (const tab of tabs) {
    const key = tab.dataset.gpu;
    const gpu = data.gpus[key];
    const selected = key === state.selectedGpu;
    tab.setAttribute("aria-selected", selected ? "true" : "false");
    tab.toggleAttribute("focusgroupstart", selected);
    tab.tabIndex = selected ? 0 : -1;
    if (!gpu) continue;
    const chg7 = gpu.change_7d_pct;
    const slope = gpu.slope_to_back_pct;
    tab.innerHTML = `
      <span class="gpu-tab-top">
        <span class="gpu-dot"></span>
        <span class="gpu-name">${escapeHtml(gpu.meta.label)}</span>
        <span class="gpu-sub-tag mono"${slope !== null ? ' data-term="slope"' : ""}>${slope !== null ? `Slope ${fmtPct(slope)}` : ""}</span>
      </span>
      <span class="gpu-tab-bottom">
        <span class="gpu-price">${fmtUsd(gpu.spot_price)}</span>
        <span class="gpu-delta ${deltaClass(chg7)}">${chg7 !== null ? `7T ${fmtPct(chg7)}` : ""}</span>
      </span>
    `;
  }
}

function renderCurveView(gpu) {
  // Hero KPIs
  // Tagesschlüsse sind reine Daten (YYYY-MM-DD); fmtTs würde daraus "01:00 Uhr" Ortszeit machen
  let spotTime = "Ornn OCPI";
  if (gpu.spot_updated_at) {
    spotTime = gpu.spot_source === "ornn-daily"
      ? `Tagesschluss ${fmtDateShort(gpu.spot_updated_at)}`
      : fmtTs(gpu.spot_updated_at);
  }
  els.heroKicker.textContent = `Ornn Compute Price Index · ${gpu.meta.full_name} · ${spotTime}`;
  els.heroPrice.textContent = fmtUsd(gpu.spot_price);

  const chgs = [];
  if (gpu.change_1d_pct !== null) chgs.push(`<span>1T <strong class="${deltaClass(gpu.change_1d_pct)}">${fmtPct(gpu.change_1d_pct)}</strong></span>`);
  if (gpu.change_7d_pct !== null) chgs.push(`<span>7T <strong class="${deltaClass(gpu.change_7d_pct)}">${fmtPct(gpu.change_7d_pct)}</strong></span>`);
  if (gpu.change_30d_pct !== null) chgs.push(`<span>30T <strong class="${deltaClass(gpu.change_30d_pct)}">${fmtPct(gpu.change_30d_pct)}</strong></span>`);
  chgs.push(`<span>Historie <strong>${gpu.spot_history.length} T</strong></span>`);
  els.heroChanges.innerHTML = chgs.join("");

  const exps = gpu.expiries || [];
  const front = exps[0] || null;
  // Mittelstrecke: Liefermonat nahe "heute + 6 Monate" (statt fest "Juni 27")
  const mid = exps.length >= 3
    ? nearestByDate(exps.slice(1, -1), (e) => e.delivery_date, addMonthsIso(todayIso(), 6))
    : exps[Math.floor(exps.length / 2)] || null;
  const back = exps[exps.length - 1] || null;

  const setTile = (lEl, vEl, sEl, exp, fallback) => {
    if (!exp) {
      lEl.textContent = fallback;
      vEl.textContent = "k. A.";
      sEl.textContent = "";
      return;
    }
    lEl.textContent = `Forward ${exp.short_label}`;
    vEl.textContent = fmtUsd(exp.median);
    const d1 = exp.delta_1d_usd !== null ? ` · 1T ${fmtSignedUsd(exp.delta_1d_usd)}` : "";
    sEl.innerHTML = `<span class="${deltaClass(exp.vs_spot_pct)}" data-term="vsSpot">${fmtPct(exp.vs_spot_pct)}</span>${d1}`;
  };

  setTile(els.kpiFrontLabel, els.kpiFrontVal, els.kpiFrontSub, front, "Front-Monat");
  setTile(els.kpiMidLabel, els.kpiMidVal, els.kpiMidSub, mid, "Mittelstrecke");
  setTile(els.kpiBackLabel, els.kpiBackVal, els.kpiBackSub, back, "Langfrist-Ende");

  els.kpiLiqVal.textContent = fmtCompactUsd(gpu.total_liquidity);
  els.kpiLiqSub.textContent = `24h-Vol. ${fmtCompactUsd(gpu.total_volume_24h)} · ${exps.length} Verfälle`;

  // Vergleichs-Buttons aktiv/inaktiv setzen
  els.cmpBtn1d.disabled = !gpu.compare_1d;
  els.cmpBtn7d.disabled = !gpu.compare_7d;
  els.cmpBtn30d.disabled = !gpu.compare_30d;
  els.cmpBtn1d.title = gpu.compare_1d ? `Vergleich mit ${fmtDateShort(gpu.compare_1d.snapshot_date)}` : "Noch kein 1T-Vergleich in SQLite";
  els.cmpBtn7d.title = gpu.compare_7d ? `Vergleich mit ${fmtDateShort(gpu.compare_7d.snapshot_date)} (${gpu.compare_7d.days_ago}T)` : "Noch kein 1W-Vergleich in SQLite";
  els.cmpBtn30d.title = gpu.compare_30d ? `Vergleich mit ${fmtDateShort(gpu.compare_30d.snapshot_date)}` : "Noch kein 1M-Vergleich in SQLite";

  if (
    (state.compareMode === "1d" && !gpu.compare_1d) ||
    (state.compareMode === "7d" && !gpu.compare_7d) ||
    (state.compareMode === "30d" && !gpu.compare_30d)
  ) {
    state.compareMode = "none";
  }

  document.querySelectorAll("[data-cmp]").forEach((btn) => {
    btn.classList.toggle("is-active", btn.dataset.cmp === state.compareMode);
  });

  // Snapshot-Dropdown füllen (aktuell angezeigten Snapshot sauber nach ID ausfiltern)
  const snaps = (gpu.available_snapshots || []).filter((s) => s.id !== gpu.snapshot_id);
  els.compareSelect.innerHTML = `<option value="">Snapshot wählen...</option>`;
  for (const s of snaps) {
    const opt = document.createElement("option");
    opt.value = String(s.id);
    const tag = s.snapshot_kind === "daily_backfill" ? "Backfill" : "Live";
    opt.textContent = `${fmtDateShort(s.snapshot_date)} · ${tag} (${fmtUsd(s.spot_price)})`;
    if (state.compareMode === "custom" && String(s.id) === state.customCompareId) opt.selected = true;
    els.compareSelect.appendChild(opt);
  }
  els.compareSelect.disabled = snaps.length === 0;

  // Chart rendern
  const selectedExp = getSelectedExpiry(gpu);
  const compareCurve = resolveCompareCurve(gpu);
  renderChartLegend(gpu, compareCurve);
  renderForwardChart({
    svg: els.forwardChartSvg,
    stage: els.forwardChartStage,
    tooltip: els.chartTooltip,
    gpuData: gpu,
    todayUtc: state.data ? state.data.today_utc : null,
    selectedExpirySlug: selectedExp ? selectedExp.slug : null,
    compareCurve,
    historyWindow: state.historyWindow,
    showIqr: state.showIqr,
    showOuter: state.showOuter,
    showMeanTrimmed: state.showMeanTrimmed,
    showMeanRaw: state.showMeanRaw,
    onSelectExpiry: (slug) => {
      state.selectedExpiryByGpu[gpu.meta.key] = slug;
      state.selectedStrikeTokenId = null;
      els.strikeHistoryBox.hidden = true;
      saveSettings();
      renderCurveView(gpu);
    },
  });

  // Verfalls-Kacheln
  els.expiryStrip.innerHTML = "";
  const spotChip = document.createElement("div");
  spotChip.className = "expiry-chip is-spot";
  spotChip.innerHTML = `
    <span class="expiry-chip-top"><span>Aktuell</span><span>Ornn</span></span>
    <span class="expiry-chip-price">${fmtUsd(gpu.spot_price)}</span>
    <span class="expiry-chip-deltas"><span class="${deltaClass(gpu.change_1d_pct)}">1T ${fmtPct(gpu.change_1d_pct)}</span></span>
  `;
  els.expiryStrip.appendChild(spotChip);

  for (const exp of exps) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `expiry-chip${selectedExp && exp.slug === selectedExp.slug ? " is-active" : ""}`;
    btn.style.setProperty("--gpu-color", gpu.meta.color);
    const d1Html = exp.delta_1d_usd !== null
      ? `<span class="${deltaClass(exp.delta_1d_usd)}">1T ${fmtSignedUsd(exp.delta_1d_usd)}</span>`
      : "";
    btn.innerHTML = `
      <span class="expiry-chip-top"><span>${escapeHtml(exp.short_label)}</span><span class="${deltaClass(exp.vs_spot_pct)}">${fmtPct(exp.vs_spot_pct)}</span></span>
      <span class="expiry-chip-price">${fmtUsd(exp.median)}</span>
      <span class="expiry-chip-deltas">${d1Html || `<span>IQR ${fmtUsd(exp.iqr)}</span>`}</span>
    `;
    btn.addEventListener("click", () => {
      state.selectedExpiryByGpu[gpu.meta.key] = exp.slug;
      state.selectedStrikeTokenId = null;
      els.strikeHistoryBox.hidden = true;
      saveSettings();
      renderCurveView(gpu);
    });
    els.expiryStrip.appendChild(btn);
  }

  renderDistributionAndStrikes(gpu, selectedExp);
}

function renderDistributionAndStrikes(gpu, exp) {
  if (!exp) {
    els.distTitle.textContent = "Implizite Verteilung";
    els.distSub.textContent = "Keine aktiven Verfälle gefunden.";
    els.distMetrics.innerHTML = "";
    els.distChartSvg.innerHTML = "";
    els.distTooltip.hidden = true;
    els.skewNote.hidden = true;
    els.strikesTbody.innerHTML = "";
    els.strikeHistoryBox.hidden = true;
    return;
  }

  els.distTitle.textContent = `Implizite Verteilung · ${gpu.meta.label} ${exp.month_label}`;
  els.distSub.textContent = `Aus ${exp.strike_count} Ja/Nein-Wetten auf Polymarket berechnet · Balkenfläche = Wahrscheinlichkeit`;
  els.distSkewBadge.textContent = exp.skew_spread !== null
    ? `Tail-Skew ${fmtSignedUsd(exp.skew_spread)}`
    : "Skew k. A.";

  const bandLbl = exp.trimmed_band_label ? ` (${escapeHtml(exp.trimmed_band_label)})` : "";
  els.distMetrics.innerHTML = `
    <div class="mini-tile">
      <div class="mini-tile-label"><span data-term="median">Median (P50)</span></div>
      <div class="mini-tile-val">${fmtUsd(exp.median)}</div>
    </div>
    <div class="mini-tile">
      <div class="mini-tile-label"><span data-term="meanTrimmed">Kern-Mean${bandLbl}</span></div>
      <div class="mini-tile-val">${fmtUsd(exp.mean_trimmed)}</div>
    </div>
    <div class="mini-tile">
      <div class="mini-tile-label"><span data-term="meanRaw">Roh-Mean (inkl. Tail)</span></div>
      <div class="mini-tile-val">${fmtUsd(exp.mean_raw)}</div>
    </div>
    <div class="mini-tile">
      <div class="mini-tile-label"><span data-term="iqr">IQR (P25 bis P75)</span></div>
      <div class="mini-tile-val">${fmtUsd(exp.p25)} bis ${fmtUsd(exp.p75)}</div>
    </div>
  `;

  if (exp.skew_spread !== null && exp.skew_spread > 0.35) {
    els.skewNote.hidden = false;
    els.skewNote.textContent = `Hinweis zum Roh-Mean (+${exp.skew_spread.toFixed(2).replace(".", ",")} $ über Median): Durch die Preisuntergrenze bei 0 $ und die Kapitalbindung bei weit entfernten binären Strikes liegen hohe Tail-Strikes strukturell höher. Für die zentrale Markterwartung sind Median (P50) und Kern-Mean maßgeblich.`;
  } else {
    els.skewNote.hidden = true;
  }

  renderDistributionChart({
    svg: els.distChartSvg,
    stage: els.distChartStage,
    tooltip: els.distTooltip,
    expiry: exp,
    spotPrice: gpu.spot_price,
    color: gpu.meta.color,
    bandRgb: gpu.meta.band_rgb,
  });

  els.strikesCaption.textContent = `Strike-Leiter · ${exp.short_label} (${exp.strike_count} Strikes)`;
  els.strikesQualityPill.textContent = exp.arbitrage_violations > 0
    ? `${exp.arbitrage_violations} Ausreißer geglättet`
    : "Keine Glättung nötig";

  els.strikesTbody.innerHTML = "";
  for (const s of exp.strikes || []) {
    const tr = document.createElement("tr");
    tr.style.setProperty("--gpu-color", gpu.meta.color);
    if (s.token_id && s.token_id === state.selectedStrikeTokenId) tr.classList.add("is-selected");
    const pPct = s.prob_yes * 100;
    const pIso = s.prob_pava * 100;
    const bidAsk = s.best_bid !== null && s.best_ask !== null
      ? `${fmtNum1(s.best_bid * 100)} / ${fmtNum1(s.best_ask * 100)}`
      : "k. A.";
    const spreadTxt = s.spread !== null ? `${fmtNum1(s.spread * 100)}\u00a0ct` : "k. A.";
    const chg24 = s.one_day_change !== null ? s.one_day_change * 100 : null;

    tr.innerHTML = `
      <th scope="row"><button type="button" class="row-action-btn">&gt; ${fmtUsd(s.strike)}</button></th>
      <td class="num">
        <span class="prob-cell">
          <span class="prob-track"><span class="prob-fill" style="width:${Math.min(100, Math.max(0, pPct)).toFixed(1)}%"></span></span>
          <span>${fmtPct(pPct, false)}</span>
        </span>
      </td>
      <td class="num">${fmtPct(pIso, false)}</td>
      <td class="num">${bidAsk}</td>
      <td class="num">${spreadTxt}</td>
      <td class="num ${deltaClass(chg24, 1)}">${chg24 !== null ? fmtSignedNum1(chg24, "Pp") : "k. A."}</td>
      <td class="num">${fmtCompactUsd(s.liquidity)}</td>
      <td class="num">${fmtCompactUsd(s.volume_24h)}</td>
    `;
    const openHistory = () => { if (s.token_id) loadStrikeHistory(gpu, exp, s); };
    tr.querySelector(".row-action-btn").addEventListener("click", (evt) => {
      evt.stopPropagation();
      openHistory();
    });
    tr.addEventListener("click", openHistory);
    els.strikesTbody.appendChild(tr);
  }
}

async function loadStrikeHistory(gpu, exp, strikeObj) {
  const reqId = ++state.strikeReqSeq;
  const targetGpuKey = gpu.meta.key;
  const targetTokenId = strikeObj.token_id;
  state.selectedStrikeTokenId = targetTokenId;
  renderDistributionAndStrikes(gpu, exp);
  els.strikeHistoryBox.hidden = false;
  els.strikeHistoryTitle.textContent = `${gpu.meta.label} ${exp.short_label} · ${strikeObj.title}`;
  els.strikeHistorySub.textContent = "Lade 30-Tage-Verlauf …";
  els.strikeHistorySvg.innerHTML = "";
  try {
    const res = await tauriInvoke("get_strike_history", {
      tokenId: targetTokenId,
      days: 30,
      fidelity: 60,
    });
    // Späte Antworten nach GPU- oder Strike-Wechsel verwerfen
    if (
      reqId !== state.strikeReqSeq ||
      state.selectedGpu !== targetGpuKey ||
      state.selectedStrikeTokenId !== targetTokenId
    ) {
      return;
    }
    const pts = res.history || [];
    els.strikeHistorySub.textContent = `${pts.length} Stundenpunkte · Aktuell ${fmtPct(strikeObj.prob_yes * 100, false)}`;
    renderStrikeHistoryChart({ svg: els.strikeHistorySvg, points: pts, color: gpu.meta.color });
  } catch (err) {
    if (reqId !== state.strikeReqSeq) return;
    els.strikeHistorySub.textContent = `Verlauf nicht verfügbar: ${friendlyError(err)}`;
  }
}

function renderCompareView(gpu) {
  const c1 = gpu.compare_1d;
  const c7 = gpu.compare_7d;
  const exps = gpu.expiries || [];

  const avgDelta = (key) => {
    const vals = exps.map((e) => e[key]).filter((v) => v !== null && v !== undefined);
    if (!vals.length) return null;
    return vals.reduce((a, b) => a + b, 0) / vals.length;
  };
  const avg1d = avgDelta("delta_1d_usd");
  const avg7d = avgDelta("delta_7d_usd");
  const spot1d = c1 && c1.spot_price && gpu.spot_price ? gpu.spot_price - c1.spot_price : null;
  const spot7d = c7 && c7.spot_price && gpu.spot_price ? gpu.spot_price - c7.spot_price : null;

  els.compareKpiGrid.innerHTML = `
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Spot-Veränderung (1T)</div>
      <div class="kpi-val mono ${deltaClass(spot1d)}">${fmtSignedUsd(spot1d)}</div>
      <div class="kpi-sub mono">${c1 ? `Basis ${fmtDateShort(c1.snapshot_date)} (${fmtUsd(c1.spot_price)})` : "Noch kein 1T-Stand"}</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label"><span data-term="curveShift">Ø Kurven-Shift (1T)</span></div>
      <div class="kpi-val mono ${deltaClass(avg1d)}">${fmtSignedUsd(avg1d)}</div>
      <div class="kpi-sub mono">Mittel über alle Verfälle</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Spot-Veränderung (1W)</div>
      <div class="kpi-val mono ${deltaClass(spot7d)}">${fmtSignedUsd(spot7d)}</div>
      <div class="kpi-sub mono">${c7 ? `Basis ${fmtDateShort(c7.snapshot_date)} (${c7.days_ago}T)` : "Noch kein 1W-Stand"}</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label"><span data-term="curveShift">Ø Kurven-Shift (1W)</span></div>
      <div class="kpi-val mono ${deltaClass(avg7d)}">${fmtSignedUsd(avg7d)}</div>
      <div class="kpi-sub mono">Mittel über alle Verfälle</div>
    </article>
  `;

  els.shiftChartTitle.textContent = `${gpu.meta.label} Kurvenverschiebung nach Laufzeit (Δ 1T vs. Δ 1W)`;
  const c1Txt = c1 ? `1T = ${fmtDateShort(c1.snapshot_date)}` : "1T offen";
  const c7Txt = c7 ? `1W = ${fmtDateShort(c7.snapshot_date)} (${c7.days_ago}T)` : "1W noch im Aufbau";
  els.shiftChartSub.textContent = `Vergleichsbasis aus SQLite: ${c1Txt} · ${c7Txt}`;
  els.compareTableSub.textContent = `${gpu.meta.full_name} · alle Verfälle im Vergleich`;

  renderCurveShiftChart({
    svg: els.shiftChartSvg,
    stage: els.shiftChartStage,
    tooltip: els.shiftTooltip,
    gpu,
  });

  els.compareTbody.innerHTML = "";
  for (const e of exps) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <th scope="row">${escapeHtml(e.short_label)}</th>
      <td class="num"><strong>${fmtUsd(e.median)}</strong></td>
      <td class="num">${fmtUsd(e.prev_1d_median)}</td>
      <td class="num ${deltaClass(e.delta_1d_usd)}">${fmtSignedUsd(e.delta_1d_usd)}</td>
      <td class="num ${deltaClass(e.delta_1d_pct)}">${fmtPct(e.delta_1d_pct)}</td>
      <td class="num">${fmtUsd(e.prev_7d_median)}</td>
      <td class="num ${deltaClass(e.delta_7d_usd)}">${fmtSignedUsd(e.delta_7d_usd)}</td>
      <td class="num ${deltaClass(e.delta_7d_pct)}">${fmtPct(e.delta_7d_pct)}</td>
      <td class="num">${fmtUsd(e.mean_trimmed)}</td>
      <td class="num">${fmtUsd(e.mean_raw)}</td>
      <td class="num">${fmtUsd(e.iqr)}</td>
    `;
    els.compareTbody.appendChild(tr);
  }
}

function renderMatrixView() {
  const data = state.data;
  if (!data) return;
  const h100Spot = data.gpus.H100 ? data.gpus.H100.spot_price : null;
  els.matrixTbody.innerHTML = "";

  // Liefermonat-Spalten relativ zu heute: ~6 Monate, ~12 Monate, längste Laufzeit
  const allExps = data.gpu_order.flatMap((k) => (data.gpus[k] && data.gpus[k].expiries) || []);
  const allDates = [...new Set(allExps.map((e) => e.delivery_date))].sort();
  const laterDates = allDates.slice(1);
  const colDates = [];
  const addCol = (iso) => { if (iso && !colDates.includes(iso)) colDates.push(iso); };
  addCol(nearestByDate(laterDates, (d) => d, addMonthsIso(todayIso(), 6)));
  addCol(nearestByDate(laterDates, (d) => d, addMonthsIso(todayIso(), 12)));
  addCol(laterDates[laterDates.length - 1]);
  colDates.sort();
  const colMeta = colDates.map((iso) => allExps.find((e) => e.delivery_date === iso));

  els.matrixHeadRow.innerHTML = `
    <th scope="col">GPU</th>
    <th scope="col" class="num"><span data-term="spot">Spot</span></th>
    <th scope="col" class="num">1T</th>
    <th scope="col" class="num">7T</th>
    <th scope="col" class="num">30T</th>
    <th scope="col" class="num"><span data-term="front">Front</span></th>
    ${colMeta.map((e) => `<th scope="col" class="num"><span data-term="median">${escapeHtml(e.short_label)}</span></th>`).join("")}
    <th scope="col" class="num"><span data-term="slope">Slope</span></th>
    <th scope="col" class="num"><span data-term="vsH100">vs. H100</span></th>
    <th scope="col" class="num"><span data-term="marketDepth">Liquidität</span></th>
  `;

  for (const key of data.gpu_order) {
    const g = data.gpus[key];
    if (!g) continue;
    const front = (g.expiries || [])[0] || null;
    const colCells = colDates.map((iso) => {
      const e = (g.expiries || []).find((x) => x.delivery_date === iso);
      return `<td class="num">${e ? fmtUsd(e.median) : "k. A."}</td>`;
    }).join("");
    const ratio = h100Spot && g.spot_price ? `${(g.spot_price / h100Spot).toFixed(2).replace(".", ",")}x` : "k. A.";

    const tr = document.createElement("tr");
    tr.innerHTML = `
      <th scope="row">
        <button type="button" class="row-action-btn matrix-gpu-cell">
          <span class="gpu-dot" style="background:${escapeHtml(g.meta.color)}"></span>
          <span>${escapeHtml(g.meta.label)}</span>
        </button>
      </th>
      <td class="num"><strong>${fmtUsd(g.spot_price)}</strong></td>
      <td class="num ${deltaClass(g.change_1d_pct)}">${fmtPct(g.change_1d_pct)}</td>
      <td class="num ${deltaClass(g.change_7d_pct)}">${fmtPct(g.change_7d_pct)}</td>
      <td class="num ${deltaClass(g.change_30d_pct)}">${fmtPct(g.change_30d_pct)}</td>
      <td class="num">${front ? fmtUsd(front.median) : "k. A."}</td>
      ${colCells}
      <td class="num ${deltaClass(g.slope_to_back_pct)}">${fmtPct(g.slope_to_back_pct)}</td>
      <td class="num">${ratio}</td>
      <td class="num">${fmtCompactUsd(g.total_liquidity)}</td>
    `;
    const openGpu = () => {
      selectGpu(key);
      switchView("curve");
    };
    tr.querySelector(".row-action-btn").addEventListener("click", (evt) => {
      evt.stopPropagation();
      openGpu();
    });
    tr.addEventListener("click", openGpu);
    els.matrixTbody.appendChild(tr);
  }

  const extras = data.extra_spots || [];
  if (extras.length) {
    els.extraSpotsRow.hidden = false;
    els.extraSpotsPills.innerHTML = extras
      .map((e) => `<span class="extra-spot-pill">${escapeHtml(e.gpu_name)}<strong>${fmtUsd(e.index_value)}</strong></span>`)
      .join("");
  } else {
    els.extraSpotsRow.hidden = true;
  }
}

function fmtFileSize(kb) {
  if (kb === null || kb === undefined || Number.isNaN(kb)) return "k. A.";
  if (kb >= 1024 * 1024) return `${(kb / 1024 / 1024).toFixed(2).replace(".", ",")} GB`;
  if (kb >= 1024) return `${(kb / 1024).toFixed(1).replace(".", ",")} MB`;
  return `${Math.round(kb)} KB`;
}

function renderStoreView() {
  const data = state.data;
  if (!data) return;
  const db = data.db_stats;

  els.storeKpiGrid.innerHTML = `
    <article class="ka-card kpi-tile">
      <div class="kpi-label"><span data-term="snapshot">Gespeicherte Kurven</span></div>
      <div class="kpi-val mono">${db.total_snapshots}</div>
      <div class="kpi-sub mono">${db.live_snapshots} Live · ${db.backfill_snapshots} Backfill</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Spot-Tagespunkte</div>
      <div class="kpi-val mono">${db.total_spot_days}</div>
      <div class="kpi-sub mono">Seit ${fmtDateShort(db.earliest_spot_date)}</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Kurvenhistorie ab</div>
      <div class="kpi-val mono">${fmtDateShort(db.earliest_curve_date)}</div>
      <div class="kpi-sub mono">Backfill: ${data.last_backfill_at ? fmtTs(data.last_backfill_at) : "ausstehend"}</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label">SQLite-Dateigröße</div>
      <div class="kpi-val mono">${fmtFileSize(db.size_kb)}</div>
      <div class="kpi-sub mono">Snapshot alle ${data.interval_min} Min., nach 48 Std. 1 je Tag</div>
    </article>
  `;

  els.storeDbPath.textContent = db.path;
  els.storeTbody.innerHTML = "";

  for (const key of data.gpu_order) {
    const g = data.gpus[key];
    if (!g) continue;
    const c1Kind = g.compare_1d && g.compare_1d.snapshot_kind === "daily_backfill" ? "Backfill" : "Live";
    const c1Text = g.compare_1d ? `${fmtDateShort(g.compare_1d.snapshot_date)} (${c1Kind})` : "offen";
    const c7Text = g.compare_7d ? `${fmtDateShort(g.compare_7d.snapshot_date)} (${g.compare_7d.days_ago}T)` : "im Aufbau";
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <th scope="row">${escapeHtml(g.meta.label)}</th>
      <td class="num">${g.spot_history.length}</td>
      <td class="num">${g.live_snapshot_count}</td>
      <td class="num">${g.backfill_snapshot_count}</td>
      <td>${escapeHtml(c1Text)}</td>
      <td>${escapeHtml(c7Text)}</td>
      <td class="num"><button type="button" class="ka-btn ka-btn-ghost ka-btn-xs" data-export-gpu="${escapeHtml(key)}">CSV in Downloads</button></td>
    `;
    tr.querySelector("[data-export-gpu]").addEventListener("click", () => handleExport(key));
    els.storeTbody.appendChild(tr);
  }
}

function renderAll() {
  if (!state.data) return;
  renderTopbar();
  renderGpuTabs();

  const showGpuBar = state.activeView === "curve" || state.activeView === "compare";
  els.gpuBar.hidden = !showGpuBar;

  const gpu = getCurrentGpu();
  if (state.activeView === "curve" && gpu) {
    renderCurveView(gpu);
  } else if (state.activeView === "compare" && gpu) {
    renderCompareView(gpu);
  } else if (state.activeView === "matrix") {
    renderMatrixView();
  } else if (state.activeView === "store") {
    renderStoreView();
  }
  annotateTerms();
}

function applyViewTabs(viewKey, focusTab = false) {
  const tabs = Array.from(els.viewTablist.querySelectorAll('[role="tab"]'));
  for (const tab of tabs) {
    const active = tab.dataset.view === viewKey;
    tab.setAttribute("aria-selected", active ? "true" : "false");
    tab.toggleAttribute("focusgroupstart", active);
    tab.tabIndex = active ? 0 : -1;
    if (active && focusTab) tab.focus();
  }
  for (const [k, panel] of Object.entries(els.panels)) {
    panel.hidden = k !== viewKey;
  }
}

function switchView(viewKey, focusTab = false) {
  state.activeView = viewKey;
  applyViewTabs(viewKey, focusTab);
  saveSettings();
  renderAll();
}

function selectGpu(gpuKey, focusTab = false) {
  if (!state.data || !state.data.gpus[gpuKey]) return;
  state.selectedGpu = gpuKey;
  state.customCompareSnapshot = null;
  state.customCompareId = "";
  if (state.compareMode === "custom") state.compareMode = "none";
  state.selectedStrikeTokenId = null;
  els.strikeHistoryBox.hidden = true;
  saveSettings();
  renderAll();
  const tab = document.getElementById(`tab-${gpuKey}`);
  if (tab) {
    tab.scrollIntoView({ block: "nearest", inline: "nearest" });
    if (focusTab) tab.focus();
  }
}

/** Gespeicherte Einstellungen in Schalter und Segment-Buttons spiegeln. */
function syncControlsFromState() {
  els.optIqr.checked = state.showIqr;
  els.optOuter.checked = state.showOuter;
  els.optMeanTrimmed.checked = state.showMeanTrimmed;
  els.optMeanRaw.checked = state.showMeanRaw;
  document.querySelectorAll("[data-hist]").forEach((b) => {
    b.classList.toggle("is-active", b.dataset.hist === state.historyWindow);
  });
  applyViewTabs(state.activeView);
}

/** Übernimmt einen vollständigen Status, ohne einen neueren Busy-Stand zu überschreiben. */
function adoptState(data) {
  if (state.busy) {
    data.refreshing = state.busy.refreshing;
    data.backfilling = state.busy.backfilling;
    data.next_refresh_at = state.busy.next_refresh_at;
  }
  state.data = data;
}

async function loadState() {
  try {
    adoptState(await tauriInvoke("get_dashboard_state"));
    renderAll();
  } catch (err) {
    setActionError("Daten konnten nicht geladen werden.", err);
  }
}

const isAlreadyRunning = (err) => String(err).includes("läuft bereits");

async function triggerRefresh() {
  if (isRefreshing()) return;
  state.pending.refresh = true;
  renderTopbar();
  try {
    const res = await tauriInvoke("refresh_now");
    if (res && res.state) {
      adoptState(res.state);
      // Manueller Abruf zeigt aktuelle Quellenfehler wieder an, auch wenn sie vorher weggeklickt waren
      state.dismissedFetchSig = "";
      clearActionError();
      renderAll();
      const issues = res.errors && res.errors.length ? ` ${res.errors.length} Quelle(n) mit Hinweis, siehe oben.` : "";
      showToast(`Aktualisiert: ${res.saved_curves} Kurven und ${res.saved_daily} Spot-Punkte gespeichert.${issues}`);
    }
  } catch (err) {
    if (isAlreadyRunning(err)) showToast("Eine Aktualisierung läuft bereits, die neuen Daten erscheinen gleich.");
    else setActionError("Aktualisierung fehlgeschlagen, angezeigt wird der zuletzt gespeicherte Stand.", err);
  } finally {
    state.pending.refresh = false;
    renderTopbar();
  }
}

async function triggerBackfill() {
  if (isBackfilling()) return;
  state.pending.backfill = true;
  renderTopbar();
  showToast("Lade die letzten 14 Tage aus dem Polymarket-Kursverlauf …");
  try {
    const res = await tauriInvoke("backfill_history", { days: 14 });
    if (res && res.state) {
      adoptState(res.state);
      if (res.errors && res.errors.length) {
        setActionError("Backfill unvollständig: Einige Kursverläufe fehlen, die übrigen Tage wurden gespeichert.", res.errors);
      } else {
        clearActionError();
      }
      renderAll();
      showToast(`Backfill fertig: ${res.saved_days} Kurventage aus ${res.strikes_fetched} Strikes gespeichert.`);
    }
  } catch (err) {
    if (isAlreadyRunning(err)) showToast("Der Backfill läuft bereits im Hintergrund.");
    else setActionError("Backfill fehlgeschlagen.", err);
  } finally {
    state.pending.backfill = false;
    renderTopbar();
  }
}

async function handleExport(gpuKey) {
  try {
    const res = await tauriInvoke("export_csv", { gpu: gpuKey });
    showToast(`CSV gespeichert (${res.rows} Zeilen): ${res.path}`);
  } catch (err) {
    setActionError("CSV-Export fehlgeschlagen.", err);
  }
}

function wireTabKeyboard(container, onSelect) {
  const tabs = Array.from(container.querySelectorAll('[role="tab"]'));
  tabs.forEach((tab, idx) => {
    tab.addEventListener("click", () => onSelect(tab, false));
    tab.addEventListener("keydown", (evt) => {
      let nextIdx = null;
      if (evt.key === "ArrowRight") nextIdx = (idx + 1) % tabs.length;
      else if (evt.key === "ArrowLeft") nextIdx = (idx - 1 + tabs.length) % tabs.length;
      else if (evt.key === "Home") nextIdx = 0;
      else if (evt.key === "End") nextIdx = tabs.length - 1;
      if (nextIdx !== null) {
        evt.preventDefault();
        onSelect(tabs[nextIdx], true);
      }
    });
  });
}

function setupEvents() {
  installTermTooltips();
  wireTabKeyboard(els.viewTablist, (tab, focus) => switchView(tab.dataset.view, focus));
  wireTabKeyboard(els.gpuTablist, (tab, focus) => selectGpu(tab.dataset.gpu, focus));

  document.querySelectorAll("[data-hist]").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.historyWindow = btn.dataset.hist;
      document.querySelectorAll("[data-hist]").forEach((b) => b.classList.toggle("is-active", b === btn));
      saveSettings();
      const gpu = getCurrentGpu();
      if (gpu) renderCurveView(gpu);
    });
  });

  document.querySelectorAll("[data-cmp]").forEach((btn) => {
    btn.addEventListener("click", () => {
      if (btn.disabled) return;
      state.compareMode = btn.dataset.cmp;
      state.customCompareSnapshot = null;
      state.customCompareId = "";
      els.compareSelect.value = "";
      const gpu = getCurrentGpu();
      if (gpu) renderCurveView(gpu);
    });
  });

  els.compareSelect.addEventListener("change", async () => {
    const val = els.compareSelect.value;
    const reqId = ++state.compareReqSeq;
    const reqGpu = state.selectedGpu;
    if (!val) {
      state.compareMode = "none";
      state.customCompareSnapshot = null;
      state.customCompareId = "";
      const gpu = getCurrentGpu();
      if (gpu) renderCurveView(gpu);
      return;
    }
    try {
      const snap = await tauriInvoke("get_snapshot", { snapshotId: Number(val) });
      if (reqId !== state.compareReqSeq || state.selectedGpu !== reqGpu) return;
      if (snap && snap.gpu === reqGpu) {
        state.compareMode = "custom";
        state.customCompareId = val;
        state.customCompareSnapshot = snap;
        const gpu = getCurrentGpu();
        if (gpu) renderCurveView(gpu);
      } else if (!snap) {
        setActionError("Dieser Snapshot ist nicht mehr gespeichert.", []);
        const gpu = getCurrentGpu();
        if (gpu) renderCurveView(gpu);
      }
    } catch (err) {
      if (reqId !== state.compareReqSeq) return;
      setActionError("Snapshot konnte nicht geladen werden.", err);
    }
  });

  const bindToggle = (input, key) => {
    input.addEventListener("change", () => {
      state[key] = input.checked;
      saveSettings();
      const gpu = getCurrentGpu();
      if (gpu) renderCurveView(gpu);
    });
  };
  bindToggle(els.optIqr, "showIqr");
  bindToggle(els.optOuter, "showOuter");
  bindToggle(els.optMeanTrimmed, "showMeanTrimmed");
  bindToggle(els.optMeanRaw, "showMeanRaw");

  els.refreshBtn.addEventListener("click", triggerRefresh);
  els.runBackfillBtn.addEventListener("click", triggerBackfill);
  els.runBackfillQuick.addEventListener("click", triggerBackfill);
  els.closeStrikeHistory.addEventListener("click", () => {
    state.selectedStrikeTokenId = null;
    els.strikeHistoryBox.hidden = true;
    const gpu = getCurrentGpu();
    if (gpu) renderDistributionAndStrikes(gpu, getSelectedExpiry(gpu));
  });

  // Theme-Schalter (Konvex Dark Standard + Light Theme)
  // Native Fensterleiste mitziehen (Windows-Titelleiste, macOS-Fensterdarstellung)
  const syncWindowTheme = (theme) => {
    const win = window.__TAURI__?.window?.getCurrentWindow?.();
    win?.setTheme?.(theme)?.catch?.(() => {});
  };
  const savedTheme = localStorage.getItem("konvex-compute-theme") || "dark";
  document.documentElement.dataset.theme = savedTheme;
  syncWindowTheme(savedTheme);
  els.themeToggle.textContent = savedTheme === "dark" ? "Hell" : "Dunkel";
  els.themeToggle.addEventListener("click", () => {
    const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    localStorage.setItem("konvex-compute-theme", next);
    els.themeToggle.textContent = next === "dark" ? "Hell" : "Dunkel";
    syncWindowTheme(next);
  });

  let resizeTimer = null;
  window.addEventListener("resize", () => {
    if (resizeTimer) clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => renderAll(), 120);
  });

  // Relative Zeitangaben ("vor 4 Min.") laufend aktuell halten
  setInterval(() => { if (state.data) renderTopbar(); }, 30_000);

  // Hintergrund-Updates aus Rust (Start-Abruf, Backfill, 15-Min-Poller)
  tauriListen("compute:updated", (event) => {
    if (event && event.payload) {
      adoptState(event.payload);
      renderAll();
    }
  });

  // Start/Ende eines Abrufs oder Backfills: Buttons sperren statt "läuft bereits"-Fehler
  tauriListen("compute:busy", (event) => {
    const p = event && event.payload;
    if (!p) return;
    state.busy = { refreshing: !!p.refreshing, backfilling: !!p.backfilling, next_refresh_at: p.next_refresh_at || null };
    if (!state.data) return;
    adoptState(state.data);
    renderTopbar();
  });
}

loadSettings();
syncControlsFromState();
setupEvents();
loadState();
