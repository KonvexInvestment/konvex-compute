import {
  escapeHtml,
  fmtCompactUsd,
  fmtPct,
  fmtSignedUsd,
  fmtUsd,
  renderCurveShiftChart,
  renderDistributionChart,
  renderForwardChart,
  renderStrikeHistoryChart,
} from "./charts.js";

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
};

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

function deltaClass(v) {
  if (v === null || v === undefined) return "";
  return v >= 0 ? "is-pos" : "is-neg";
}

function showError(msg) {
  els.errorBanner.hidden = !msg;
  els.errorBanner.textContent = msg || "";
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

function renderTopbar() {
  const data = state.data;
  if (!data) return;

  els.freshnessDot.classList.remove("is-stale", "is-busy");
  if (data.refreshing || data.backfilling) {
    els.freshnessDot.classList.add("is-busy");
    els.freshnessText.textContent = data.backfilling ? "Backfill läuft..." : "Aktualisiere...";
  } else if (data.last_captured_at) {
    const ageMin = Math.round((Date.now() - new Date(data.last_captured_at).getTime()) / 60000);
    if (ageMin > 60) els.freshnessDot.classList.add("is-stale");
    els.freshnessText.textContent = `Stand ${fmtTs(data.last_captured_at)} · ${data.db_stats.total_snapshots} Snaps`;
  } else {
    els.freshnessDot.classList.add("is-stale");
    els.freshnessText.textContent = "Kein Datenstand";
  }

  if (data.last_errors && data.last_errors.length) {
    showError(`Hinweis vom letzten Abruf: ${data.last_errors.join(" | ")}`);
  } else {
    showError("");
  }
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
        <span class="gpu-sub-tag mono">${slope !== null ? `Slope ${fmtPct(slope)}` : ""}</span>
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
  const spotTime = gpu.spot_updated_at ? fmtTs(gpu.spot_updated_at) : "Ornn OCPI";
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
  const mid = exps.find((e) => e.slug.includes("june-2027")) || exps[Math.floor(exps.length / 2)] || null;
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
    sEl.innerHTML = `<span class="${deltaClass(exp.vs_spot_pct)}">${fmtPct(exp.vs_spot_pct)}</span>${d1}`;
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
  renderForwardChart({
    svg: els.forwardChartSvg,
    stage: els.forwardChartStage,
    tooltip: els.chartTooltip,
    gpuData: gpu,
    todayUtc: state.data ? state.data.today_utc : null,
    selectedExpirySlug: selectedExp ? selectedExp.slug : null,
    compareCurve: resolveCompareCurve(gpu),
    historyWindow: state.historyWindow,
    showIqr: state.showIqr,
    showOuter: state.showOuter,
    showMeanTrimmed: state.showMeanTrimmed,
    showMeanRaw: state.showMeanRaw,
    onSelectExpiry: (slug) => {
      state.selectedExpiryByGpu[gpu.meta.key] = slug;
      state.selectedStrikeTokenId = null;
      els.strikeHistoryBox.hidden = true;
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
  els.distSub.textContent = `${exp.strike_count} binäre Polymarket-Strikes · PAVA-monoton geglättet`;
  els.distSkewBadge.textContent = exp.skew_spread !== null
    ? `Tail-Skew ${fmtSignedUsd(exp.skew_spread)}`
    : "Skew k. A.";

  const bandLbl = exp.trimmed_band_label ? ` (${escapeHtml(exp.trimmed_band_label)})` : "";
  els.distMetrics.innerHTML = `
    <div class="mini-tile">
      <div class="mini-tile-label">Median (P50)</div>
      <div class="mini-tile-val">${fmtUsd(exp.median)}</div>
    </div>
    <div class="mini-tile">
      <div class="mini-tile-label">Kern-Mean${bandLbl}</div>
      <div class="mini-tile-val">${fmtUsd(exp.mean_trimmed)}</div>
    </div>
    <div class="mini-tile">
      <div class="mini-tile-label">Roh-Mean (inkl. Tail)</div>
      <div class="mini-tile-val">${fmtUsd(exp.mean_raw)}</div>
    </div>
    <div class="mini-tile">
      <div class="mini-tile-label">IQR (P25 bis P75)</div>
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
    ? `${exp.arbitrage_violations} Zacken geglättet`
    : "Sauber monoton";

  els.strikesTbody.innerHTML = "";
  for (const s of exp.strikes || []) {
    const tr = document.createElement("tr");
    tr.style.setProperty("--gpu-color", gpu.meta.color);
    if (s.token_id && s.token_id === state.selectedStrikeTokenId) tr.classList.add("is-selected");
    const pPct = s.prob_yes * 100;
    const pIso = s.prob_pava * 100;
    const bidAsk = s.best_bid !== null && s.best_ask !== null
      ? `${(s.best_bid * 100).toFixed(1)} / ${(s.best_ask * 100).toFixed(1)}`
      : "k. A.";
    const spreadTxt = s.spread !== null ? `${(s.spread * 100).toFixed(1)} ct` : "k. A.";
    const chg24 = s.one_day_change !== null ? s.one_day_change * 100 : null;

    tr.innerHTML = `
      <th scope="row"><button type="button" class="row-action-btn">&gt; ${fmtUsd(s.strike)}</button></th>
      <td class="num">
        <span class="prob-cell">
          <span class="prob-track"><span class="prob-fill" style="width:${Math.min(100, Math.max(0, pPct)).toFixed(1)}%"></span></span>
          <span>${pPct.toFixed(1)} %</span>
        </span>
      </td>
      <td class="num">${pIso.toFixed(1)} %</td>
      <td class="num">${bidAsk}</td>
      <td class="num">${spreadTxt}</td>
      <td class="num ${deltaClass(chg24)}">${chg24 !== null ? `${chg24 >= 0 ? "+" : ""}${chg24.toFixed(1)} Pp` : "k. A."}</td>
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
  els.strikeHistorySub.textContent = "Lade 30-Tage-CLOB-Verlauf...";
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
    els.strikeHistorySub.textContent = `${pts.length} Stundenpunkte · Aktuell ${(strikeObj.prob_yes * 100).toFixed(1)} %`;
    renderStrikeHistoryChart({ svg: els.strikeHistorySvg, points: pts, color: gpu.meta.color });
  } catch (err) {
    if (reqId !== state.strikeReqSeq) return;
    els.strikeHistorySub.textContent = `Fehler: ${err}`;
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
      <div class="kpi-label">Ø Kurven-Shift (1T)</div>
      <div class="kpi-val mono ${deltaClass(avg1d)}">${fmtSignedUsd(avg1d)}</div>
      <div class="kpi-sub mono">Mittel über alle Verfälle</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Spot-Veränderung (1W)</div>
      <div class="kpi-val mono ${deltaClass(spot7d)}">${fmtSignedUsd(spot7d)}</div>
      <div class="kpi-sub mono">${c7 ? `Basis ${fmtDateShort(c7.snapshot_date)} (${c7.days_ago}T)` : "Noch kein 1W-Stand"}</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Ø Kurven-Shift (1W)</div>
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

  for (const key of data.gpu_order) {
    const g = data.gpus[key];
    if (!g) continue;
    const findExp = (sub) => (g.expiries || []).find((e) => e.slug.includes(sub));
    const front = (g.expiries || [])[0] || null;
    const jun27 = findExp("june-2027");
    const dec27 = findExp("december-2027");
    const dec28 = findExp("december-2028");
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
      <td class="num">${jun27 ? fmtUsd(jun27.median) : "k. A."}</td>
      <td class="num">${dec27 ? fmtUsd(dec27.median) : "k. A."}</td>
      <td class="num">${dec28 ? fmtUsd(dec28.median) : "k. A."}</td>
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

function renderStoreView() {
  const data = state.data;
  if (!data) return;
  const db = data.db_stats;

  els.storeKpiGrid.innerHTML = `
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Gespeicherte Kurven</div>
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
      <div class="kpi-val mono">${db.size_kb} KB</div>
      <div class="kpi-sub mono">Auto-Snapshot alle ${data.interval_min} Min.</div>
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
}

function switchView(viewKey, focusTab = false) {
  state.activeView = viewKey;
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
  renderAll();
  const tab = document.getElementById(`tab-${gpuKey}`);
  if (tab) {
    tab.scrollIntoView({ block: "nearest", inline: "nearest" });
    if (focusTab) tab.focus();
  }
}

async function loadState() {
  try {
    state.data = await tauriInvoke("get_dashboard_state");
    renderAll();
  } catch (err) {
    showError(`Status konnte nicht geladen werden: ${err}`);
  }
}

async function triggerRefresh() {
  els.refreshBtn.disabled = true;
  els.refreshBtn.classList.add("is-loading");
  try {
    const res = await tauriInvoke("refresh_now");
    if (res && res.state) {
      state.data = res.state;
      renderAll();
      showToast(`Aktualisiert: ${res.saved_curves} Kurven und ${res.saved_daily} Spot-Punkte gespeichert.`);
    }
  } catch (err) {
    showError(`Aktualisierung fehlgeschlagen: ${err}`);
  } finally {
    els.refreshBtn.disabled = false;
    els.refreshBtn.classList.remove("is-loading");
  }
}

async function triggerBackfill() {
  els.runBackfillBtn.disabled = true;
  els.runBackfillBtn.classList.add("is-loading");
  els.runBackfillQuick.disabled = true;
  showToast("Lade 14-Tage-CLOB-Historie für Tages- und Wochenvergleiche...");
  try {
    const res = await tauriInvoke("backfill_history", { days: 14 });
    if (res && res.state) {
      state.data = res.state;
      renderAll();
      if (res.errors && res.errors.length) {
        showError(`Backfill-Teilfehler: ${res.errors.join(" | ")}`);
      }
      showToast(`Backfill fertig: ${res.saved_days} Kurventage aus ${res.strikes_fetched} Strikes gespeichert.`);
    }
  } catch (err) {
    showError(`Backfill fehlgeschlagen: ${err}`);
  } finally {
    els.runBackfillBtn.disabled = false;
    els.runBackfillBtn.classList.remove("is-loading");
    els.runBackfillQuick.disabled = false;
  }
}

async function handleExport(gpuKey) {
  try {
    const res = await tauriInvoke("export_csv", { gpu: gpuKey });
    showToast(`CSV gespeichert (${res.rows} Zeilen): ${res.path}`);
  } catch (err) {
    showError(`CSV-Export fehlgeschlagen: ${err}`);
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
  wireTabKeyboard(els.viewTablist, (tab, focus) => switchView(tab.dataset.view, focus));
  wireTabKeyboard(els.gpuTablist, (tab, focus) => selectGpu(tab.dataset.gpu, focus));

  document.querySelectorAll("[data-hist]").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.historyWindow = btn.dataset.hist;
      document.querySelectorAll("[data-hist]").forEach((b) => b.classList.toggle("is-active", b === btn));
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
      }
    } catch (err) {
      if (reqId !== state.compareReqSeq) return;
      showError(`Snapshot konnte nicht geladen werden: ${err}`);
    }
  });

  els.optIqr.addEventListener("change", () => {
    state.showIqr = els.optIqr.checked;
    const gpu = getCurrentGpu();
    if (gpu) renderCurveView(gpu);
  });
  els.optOuter.addEventListener("change", () => {
    state.showOuter = els.optOuter.checked;
    const gpu = getCurrentGpu();
    if (gpu) renderCurveView(gpu);
  });
  els.optMeanTrimmed.addEventListener("change", () => {
    state.showMeanTrimmed = els.optMeanTrimmed.checked;
    const gpu = getCurrentGpu();
    if (gpu) renderCurveView(gpu);
  });
  els.optMeanRaw.addEventListener("change", () => {
    state.showMeanRaw = els.optMeanRaw.checked;
    const gpu = getCurrentGpu();
    if (gpu) renderCurveView(gpu);
  });

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
  const savedTheme = localStorage.getItem("konvex-compute-theme") || "dark";
  document.documentElement.dataset.theme = savedTheme;
  els.themeToggle.textContent = savedTheme === "dark" ? "Hell" : "Dunkel";
  els.themeToggle.addEventListener("click", () => {
    const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    localStorage.setItem("konvex-compute-theme", next);
    els.themeToggle.textContent = next === "dark" ? "Hell" : "Dunkel";
  });

  let resizeTimer = null;
  window.addEventListener("resize", () => {
    if (resizeTimer) clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => renderAll(), 120);
  });

  // Hintergrund-Updates aus Rust (Initial-Abruf, 14T-Backfill, 15-Min-Poller)
  tauriListen("compute:updated", (event) => {
    if (event && event.payload) {
      state.data = event.payload;
      renderAll();
    }
  });
}

setupEvents();
loadState();
