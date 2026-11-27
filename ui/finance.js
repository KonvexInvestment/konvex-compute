/**
 * Reiter „Finanzierung“: Finanzierungsregime aus Kreditaufschlägen und GPU-Forward-Kurve.
 * Lädt die Modellausgabe je GPU über `get_finance_view`, zeigt Regime, Stressindex,
 * Emittenten, Makroblock und Einstellungen für die IB-Verbindung.
 */

import { escapeHtml } from "./charts.js";

/** Ab so vielen Kurventagen zeichnet der Chart den nahen Median (wie die GPU-Seite des Modells). */
const MIN_CURVE_DAYS_FOR_LINE = 15;
import { annotateTerms } from "./glossary.js";

const SVG_NS = "http://www.w3.org/2000/svg";
const NBSP = " ";
/** Nach dieser Zeit lädt der Reiter die Modellausgabe beim nächsten Anzeigen neu. */
const CACHE_MS = 10 * 60 * 1000;

const REGIME_COLORS = {
  unbekannt: "var(--text-dimmed)",
  ruhig: "var(--accent-green)",
  preisdruck: "var(--accent-amber)",
  entkoppelt: "var(--accent-blue)",
  uebertragung: "var(--accent-red)",
};
const REGIME_SHORT = {
  unbekannt: "Zu wenig Daten",
  ruhig: "Ruhig",
  preisdruck: "Preisdruck",
  entkoppelt: "Entkoppelt",
  uebertragung: "Übertragung",
};

const fin = {
  deps: null,
  views: {}, // gpu -> { view, at }; at = 0 heißt veraltet, wird beim nächsten Anzeigen neu geladen
  inflight: {}, // gpu -> true, solange eine Berechnung läuft
  version: 0, // steigt mit jeder Invalidierung; Ergebnisse älterer Versionen gelten als veraltet
  statusEpoch: 0, // steigt mit jedem compute:credit-Event; ältere Statusantworten zählen dann nicht
  again: {}, // gpu -> true, wenn während der Berechnung neue Daten kamen
  seq: 0,
  status: null,
  running: false,
  settingsDirty: false,
  shownSettings: null,
  // Speichern erst, wenn das Formular einmal mit den gespeicherten Werten gefüllt ist:
  // Ein leeres Formular würde sonst Emittenten und IB-Schalter überschreiben.
  settingsLoaded: false,
  saving: false,
};

const $ = (id) => document.getElementById(id);

/** Markiert alle Ansichten als veraltet, ohne sie zu verwerfen: Bis die neue Berechnung da ist, bleibt die alte sichtbar. */
function invalidate() {
  fin.version += 1;
  for (const entry of Object.values(fin.views)) entry.at = 0;
}

function reloadIfActive() {
  invalidate();
  if (fin.deps.isActive()) load(fin.deps.getGpu());
}

function isMissing(v) {
  return v === null || v === undefined || Number.isNaN(v);
}

function fmt(v, digits = 2) {
  if (isMissing(v)) return "k. A.";
  const f = 10 ** digits;
  const r = Math.round(v * f) / f;
  return new Intl.NumberFormat("de-DE", { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(r === 0 ? 0 : r);
}

function fmtSigned(v, digits = 2, unit = "") {
  if (isMissing(v)) return "k. A.";
  const s = fmt(v, digits);
  const sign = Number(s.replace(/\./g, "").replace(",", ".")) > 0 ? "+" : "";
  return `${sign}${s}${unit ? NBSP + unit : ""}`;
}

function fmtUsd(v) {
  return isMissing(v) ? "k. A." : `${fmt(v, 2)}${NBSP}$`;
}

function fmtPctOf(prob) {
  return isMissing(prob) ? "k. A." : `${fmt(prob * 100, 0)}${NBSP}%`;
}

function fmtDay(iso) {
  if (!iso) return "k. A.";
  const [y, m, d] = iso.slice(0, 10).split("-");
  return `${d}.${m}.${y}`;
}

function fmtTs(iso) {
  if (!iso) return "noch nie";
  const dt = new Date(iso);
  if (Number.isNaN(dt.getTime())) return iso;
  return new Intl.DateTimeFormat("de-DE", { day: "2-digit", month: "2-digit", year: "2-digit", hour: "2-digit", minute: "2-digit" }).format(dt);
}

function svgEl(tag, attrs = {}, text = null) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v !== null && v !== undefined) node.setAttribute(k, String(v));
  }
  if (text !== null) node.textContent = text;
  return node;
}

function dayMs(iso) {
  const [y, m, d] = iso.slice(0, 10).split("-").map(Number);
  return Date.UTC(y, m - 1, d);
}

// ---------------------------------------------------------------------------
// Laden
// ---------------------------------------------------------------------------

function updateSaveButton() {
  $("fin-save-btn").disabled = !fin.settingsLoaded || fin.saving;
}

export function initFinance(deps) {
  fin.deps = deps;
  updateSaveButton();
  $("fin-refresh-btn").addEventListener("click", triggerCreditRefresh);
  $("fin-settings").addEventListener("submit", (evt) => {
    evt.preventDefault();
    saveSettings();
  });
  // Eingaben vor dem ersten Laden zählen nicht: Das Formular wird dann ohnehin überschrieben
  $("fin-settings").addEventListener("input", () => { if (fin.settingsLoaded) fin.settingsDirty = true; });

  deps.listen("compute:credit", (event) => {
    const st = event && event.payload;
    if (!st) return;
    const wasRunning = fin.running;
    fin.statusEpoch += 1;
    fin.status = st;
    fin.running = !!st.running;
    renderStatus();
    // Nach einem Abruf (auch aus dem Hintergrund) neu rechnen
    if (wasRunning && !fin.running) reloadIfActive();
  });
  deps.listen("compute:updated", reloadIfActive);
}

export function renderFinance(gpu) {
  const cached = fin.views[gpu];
  if (cached) draw(cached.view, !!cached.at);
  else drawLoading();
  if (!cached || !cached.at || Date.now() - cached.at > CACHE_MS) load(gpu);
}

async function load(gpu) {
  if (fin.inflight[gpu]) {
    fin.again[gpu] = true;
    return;
  }
  fin.inflight[gpu] = true;
  const reqId = ++fin.seq;
  const version = fin.version;
  const epoch = fin.statusEpoch;
  try {
    const view = await fin.deps.invoke("get_finance_view", { gpu });
    // Kamen während der Berechnung neue Daten, gilt das Ergebnis sofort als veraltet
    fin.views[gpu] = { view, at: version === fin.version ? Date.now() : 0 };
    // Ein compute:credit-Event ist neuer als der Status dieser Antwort
    if (epoch === fin.statusEpoch) {
      fin.status = view.status;
      fin.running = !!view.status.running;
    }
    if (reqId !== fin.seq || fin.deps.getGpu() !== gpu || !fin.deps.isActive()) return;
    // Eine veraltete Antwort darf das Formular nicht mit alten Einstellungen füllen
    draw(view, version === fin.version);
  } catch (err) {
    if (reqId !== fin.seq) return;
    fin.deps.setActionError("Das Finanzierungsregime konnte nicht berechnet werden.", err);
  } finally {
    delete fin.inflight[gpu];
    if (fin.again[gpu]) {
      delete fin.again[gpu];
      if (fin.deps.isActive() && fin.deps.getGpu() === gpu) load(gpu);
    }
  }
}

async function triggerCreditRefresh() {
  if (fin.running) return;
  fin.running = true;
  // Ab hier zählt der lokale Status; ältere Antworten von get_finance_view überschreiben ihn nicht
  fin.statusEpoch += 1;
  const epoch = fin.statusEpoch;
  renderStatus();
  fin.deps.showToast("Lade Zinskurve, Kreditaufschläge und Anleihekurse. Der erste IB-Abruf dauert einige Minuten.");
  let alreadyRunning = false;
  try {
    const st = await fin.deps.invoke("refresh_credit_now");
    fin.status = st;
    const n = st.errors ? st.errors.length : 0;
    fin.deps.showToast(n ? `Kreditdaten aktualisiert, ${n} Hinweis(e) im Datenstand.` : "Kreditdaten aktualisiert.");
  } catch (err) {
    alreadyRunning = String(err).includes("läuft bereits");
    if (alreadyRunning) fin.deps.showToast("Der Kreditabruf läuft bereits im Hintergrund.");
    else fin.deps.setActionError("Kreditdaten konnten nicht aktualisiert werden.", err);
  } finally {
    const eventSeen = fin.statusEpoch !== epoch;
    if (alreadyRunning) {
      // Der Hintergrundabruf meldet sein Ende per compute:credit. Kam schon ein Event, gilt
      // dessen Stand; sonst bis zum Ende-Event als laufend zeigen.
      if (!eventSeen) fin.running = true;
      renderStatus();
    } else if (eventSeen && !fin.running) {
      // Das Ende-Event kam vor der Antwort und hat schon neu gerechnet
      renderStatus();
    } else {
      fin.running = false;
      renderStatus();
      reloadIfActive();
    }
  }
}

function readSettingsForm() {
  const rows = Array.from($("fin-issuer-rows").querySelectorAll(".fin-issuer-row"));
  const issuers = rows
    .map((row) => ({
      key: row.querySelector("[data-f=key]").value.trim().toUpperCase(),
      label: row.querySelector("[data-f=label]").value.trim(),
      agency_rating: row.querySelector("[data-f=rating]").value.trim(),
      enabled: row.querySelector("[data-f=enabled]").checked,
    }))
    .filter((i) => i.key);
  const port = Number($("fin-ib-port").value);
  const clientId = Number($("fin-ib-client").value);
  return {
    ib_enabled: $("fin-ib-enabled").checked,
    ib_host: $("fin-ib-host").value.trim(),
    ib_port: Number.isInteger(port) && port >= 0 && port <= 65535 ? port : 0,
    ib_client_id: Number.isInteger(clientId) && clientId >= 0 && clientId <= 999999 ? clientId : 0,
    issuers,
  };
}

async function saveSettings() {
  if (!fin.settingsLoaded || fin.saving) return;
  fin.saving = true;
  updateSaveButton();
  const form = $("fin-settings");
  form.setAttribute("aria-busy", "true");
  try {
    const clean = await fin.deps.invoke("save_credit_settings", { settings: readSettingsForm() });
    fin.settingsDirty = false;
    fin.shownSettings = null;
    fillSettings(clean, true);
    fin.deps.showToast("Einstellungen gespeichert. Neue Emittenten erscheinen nach dem nächsten Kreditabruf.");
    reloadIfActive();
  } catch (err) {
    fin.deps.setActionError("Einstellungen nicht gespeichert.", err);
  } finally {
    fin.saving = false;
    form.removeAttribute("aria-busy");
    updateSaveButton();
  }
}

// ---------------------------------------------------------------------------
// Darstellung
// ---------------------------------------------------------------------------

function drawLoading() {
  $("fin-regime").innerHTML = `<p class="section-sub">Berechne das Finanzierungsregime …</p>`;
}

function draw(view, settingsFresh = true) {
  const r = view.report;
  renderRegime(r);
  renderKpis(r);
  renderChart(r);
  renderIssuers(r, view.bonds || []);
  renderMacro(r);
  renderStatus();
  if (settingsFresh) fillSettings(view.settings, false);
  annotateTerms($("panel-finance"));
}

function regimeChip(key, label) {
  return `<span class="fin-chip" style="--chip:${REGIME_COLORS[key] || REGIME_COLORS.unbekannt}"><span class="fin-chip-dot"></span>${escapeHtml(label)}</span>`;
}

function renderRegime(r) {
  const since = r.regime_since ? ` seit ${fmtDay(r.regime_since)}` : "";
  const parts = [];
  parts.push(`
    <div class="fin-regime-head">
      <div>
        <div class="kpi-kicker" data-term="regime">Finanzierungsregime · ${escapeHtml(r.gpu)}</div>
        <div class="fin-regime-title">${regimeChip(r.regime, r.regime_label)}<span class="fin-regime-since mono">${escapeHtml(since)}</span></div>
      </div>
      <div class="fin-regime-side mono">Stand ${escapeHtml(fmtDay(r.as_of))}${
        r.classified_until && r.classified_until !== r.as_of ? `<br>Einstufung bis ${escapeHtml(fmtDay(r.classified_until))}` : ""
      }</div>
    </div>
    <p class="fin-regime-text">${escapeHtml(r.regime_explanation)}</p>`);

  if (r.regime === "unbekannt") {
    const credit = r.fsi === null || r.fsi === undefined
      ? "Kreditseite: noch keine Daten."
      : `Kreditseite: ${r.credit_stress ? "unter Stress" : "ruhig"} (Stressindex ${fmt(r.fsi)}).`;
    const curve = r.curve_days < r.curve_days_needed
      ? `GPU-Seite: ${r.curve_days} von ${r.curve_days_needed} Kurventagen gesammelt.`
      : "GPU-Seite: genug Kurventage gesammelt.";
    parts.push(`<p class="fin-regime-partial">${escapeHtml(credit)} ${escapeHtml(curve)}</p>`);
  }
  if (r.early_warning) {
    parts.push(`<div class="fin-warning" role="alert"><strong>Vorwarnung:</strong> Der Kredit steht unter Stress, und es gibt Anzeichen, dass er die Mieten erreicht.</div>`);
  }
  const hints = [...(r.warnings || []), ...(r.notes || [])];
  if (hints.length) {
    parts.push(`<ul class="fin-hints">${hints.map((h) => `<li>${escapeHtml(h)}</li>`).join("")}</ul>`);
  }
  const trans = (r.transitions || []).slice(-4).reverse();
  if (trans.length) {
    parts.push(`<div class="fin-transitions"><span class="toolbar-label">Letzte Wechsel</span>${trans
      .map((t) => `<span class="mono" title="Bestätigt am ${escapeHtml(fmtDay(t.date))}, erste Anzeichen ab ${escapeHtml(fmtDay(t.signal_since))}">${escapeHtml(fmtDay(t.date))}</span> ${regimeChip(t.to, REGIME_SHORT[t.to] || t.to_label)}`)
      .join(" ")}</div>`);
  }
  $("fin-regime").innerHTML = parts.join("");
}

function renderKpis(r) {
  const p = r.price || {};
  const c = r.coupling || {};
  const stressCls = r.credit_stress ? "is-neg" : "";
  const coupling = c.beta === null || c.beta === undefined
    ? `Kopplung: ${c.points} von 20 Punkten`
    : `Kopplung β ${fmtSigned(c.beta, 2)} · r ${fmtSigned(c.corr, 2)}`;
  $("fin-kpi-grid").innerHTML = `
    <article class="ka-card kpi-tile">
      <div class="kpi-label" data-term="fsi">Stressindex</div>
      <div class="kpi-val mono ${stressCls}">${fmt(r.fsi)}</div>
      <div class="kpi-sub mono"><span data-term="macroBlock">Makro ${fmt(r.macro_block)}</span> · <span data-term="aiBlock">KI ${fmt(r.ai_block)}</span></div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label">Kreditseite</div>
      <div class="kpi-val mono ${stressCls}">${r.fsi === null || r.fsi === undefined ? "k. A." : r.credit_stress ? "Stress" : "Ruhig"}</div>
      <div class="kpi-sub mono">Eintritt ab 1,0 · Austritt unter 0,5</div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label" data-term="nearCurve">Nahe Kurve (90 Tage)</div>
      <div class="kpi-val mono">${fmtUsd(p.near_median)}</div>
      <div class="kpi-sub mono">P25 ${fmtUsd(p.near_p25)} · <span data-term="basis">Basis ${fmtSigned(p.basis_pct, 1, "%")}</span></div>
    </article>
    <article class="ka-card kpi-tile">
      <div class="kpi-label" data-term="breakProb">Bruchwahrscheinlichkeit</div>
      <div class="kpi-val mono">${fmtPctOf(r.price_break_prob)}</div>
      <div class="kpi-sub mono">Kredit ${fmtPctOf(r.credit_break_prob)} · <span data-term="coupling">${escapeHtml(coupling)}</span></div>
    </article>`;

  $("fin-price-grid").innerHTML = [
    ["Median Δ 14T", fmtSigned(p.near_change_14d_pct, 1, "%")],
    ["P25 Δ 14T", fmtSigned(p.p25_change_14d_pct, 1, "%")],
    ["Median Δ 30T", fmtSigned(p.near_change_30d_pct, 1, "%")],
    ["P25 Δ 30T", fmtSigned(p.p25_change_30d_pct, 1, "%")],
    ["Basis Δ 14T", fmtSigned(p.basis_change_14d_pp, 1, "Pp")],
    ["365 T zu 90 T", fmtSigned(p.slope_pct, 1, "%")],
    ["Ferner Median", fmtUsd(p.far_median)],
    ["Kurventage", `${r.curve_days} / ${r.curve_days_needed}`],
  ]
    .map(([label, val]) => `<div class="mini-tile"><div class="mini-tile-label">${escapeHtml(label)}</div><div class="mini-tile-val">${escapeHtml(val)}</div></div>`)
    .join("");
}

function renderIssuers(r, bonds) {
  const tbody = $("fin-issuer-tbody");
  if (!r.issuers || !r.issuers.length) {
    tbody.innerHTML = `<tr><td colspan="7" class="section-sub">Keine Emittenten eingestellt.</td></tr>`;
  } else {
    tbody.innerHTML = r.issuers
      .map((i) => {
        const gapCls = !isMissing(i.gap_buckets) && i.gap_buckets >= 1 ? "is-neg" : "";
        const implied = i.implied_bucket ? `${escapeHtml(i.implied_bucket)} <span class="fin-dim mono">(${fmt(i.implied_position, 1)})</span>` : "k. A.";
        const stand = i.as_of ? `${fmtDay(i.as_of)}${i.stale ? " · veraltet" : ""}` : "keine Kurse";
        return `<tr>
          <th scope="row">${escapeHtml(i.label)} <span class="fin-dim mono">${escapeHtml(i.key)}</span></th>
          <td class="num mono">${isMissing(i.spread_bp) ? "k. A." : `${fmt(i.spread_bp, 0)}${NBSP}bp`}</td>
          <td class="num mono">${fmtSigned(i.change_30d_bp, 0, "bp")}</td>
          <td>${escapeHtml(i.agency_rating || "k. A.")}</td>
          <td>${implied}</td>
          <td class="num mono ${gapCls}">${fmtSigned(i.gap_buckets, 1)}</td>
          <td class="${i.stale ? "is-neg" : ""}">${escapeHtml(stand)}</td>
        </tr>`;
      })
      .join("");
  }
  const byIssuer = {};
  for (const b of bonds) (byIssuer[b.issuer] ||= []).push(b);
  const lines = Object.entries(byIssuer).map(([k, list]) => {
    const items = list.map((b) => {
      const px = isMissing(b.last_price) ? "" : `, Geldkurs ${fmt(b.last_price, 2)} am ${fmtDay(b.last_date)}`;
      return `${escapeHtml(b.description)}${escapeHtml(px)}`;
    });
    return `<strong>${escapeHtml(k)}:</strong> ${items.join(" · ")}`;
  });
  $("fin-bonds").innerHTML = lines.length
    ? `Anleihen im Korb. ${lines.join("<br>")}`
    : "Noch keine Anleihekurse. Läuft TWS oder IB Gateway, lädt der nächste Kreditabruf sie automatisch.";
}

function renderMacro(r) {
  const rows = r.macro_components || [];
  $("fin-macro-tbody").innerHTML = rows.length
    ? rows
        .map((c) => `<tr>
          <th scope="row">${escapeHtml(c.label)}</th>
          <td class="num mono">${isMissing(c.value) ? "k. A." : `${fmt(c.value, 2)}${NBSP}%`}</td>
          <td class="num mono ${!isMissing(c.score) && c.score >= 1 ? "is-neg" : ""}">${fmtSigned(c.score, 2)}</td>
          <td>${escapeHtml(fmtDay(c.as_of))}</td>
        </tr>`)
        .join("")
    : `<tr><td colspan="4" class="section-sub">Noch keine Kreditaufschläge geladen.</td></tr>`;
}

function renderStatus() {
  const st = fin.status;
  const btn = $("fin-refresh-btn");
  btn.disabled = fin.running;
  btn.classList.toggle("is-loading", fin.running);
  btn.setAttribute("aria-busy", fin.running ? "true" : "false");
  btn.querySelector(".ka-btn-label").textContent = fin.running ? "Abruf läuft …" : "Kreditdaten aktualisieren";
  if (!st) {
    $("fin-status").textContent = "";
    return;
  }
  const parts = [
    `Zinskurve und Aufschläge: ${fmtTs(st.last_success_at)}`,
    `IB: ${st.ib_status || "noch nicht versucht"}${st.ib_last_success_at ? ` (${fmtTs(st.ib_last_success_at)})` : ""}`,
  ];
  if (fin.running) parts.unshift("Abruf läuft");
  $("fin-status").textContent = parts.join(" · ");
  const errs = st.errors || [];
  const ul = $("fin-errors");
  ul.hidden = errs.length === 0;
  ul.innerHTML = errs.slice(0, 8).map((e) => `<li>${escapeHtml(e)}</li>`).join("");
}

function issuerRow(i) {
  const row = document.createElement("div");
  row.className = "fin-issuer-row";
  row.innerHTML = `
    <input class="ka-input mono" data-f="key" maxlength="12" placeholder="Symbol" aria-label="IB-Symbol" spellcheck="false">
    <input class="ka-input" data-f="label" maxlength="40" placeholder="Name" aria-label="Name">
    <input class="ka-input mono" data-f="rating" maxlength="8" placeholder="Rating" aria-label="Agenturrating" spellcheck="false">
    <label class="ka-check"><input type="checkbox" data-f="enabled"> aktiv</label>`;
  row.querySelector("[data-f=key]").value = i.key || "";
  row.querySelector("[data-f=label]").value = i.label || "";
  row.querySelector("[data-f=rating]").value = i.agency_rating || "";
  row.querySelector("[data-f=enabled]").checked = i.enabled !== false;
  return row;
}

function fillSettings(s, force) {
  if (!s || (!force && (fin.saving || fin.settingsDirty))) return;
  const sig = JSON.stringify(s);
  if (!force && fin.shownSettings === sig) return;
  fin.shownSettings = sig;
  fin.settingsLoaded = true;
  updateSaveButton();
  $("fin-ib-enabled").checked = !!s.ib_enabled;
  $("fin-ib-host").value = s.ib_host || "127.0.0.1";
  $("fin-ib-port").value = String(s.ib_port ?? 0);
  $("fin-ib-client").value = String(s.ib_client_id ?? 0);
  const box = $("fin-issuer-rows");
  box.innerHTML = "";
  for (const i of s.issuers || []) box.appendChild(issuerRow(i));
  // Eine leere Zeile zum Ergänzen; Zeilen ohne Symbol werden beim Speichern verworfen
  if ((s.issuers || []).length < 10) box.appendChild(issuerRow({ enabled: true }));
}

// ---------------------------------------------------------------------------
// Chart: Stressindex mit Regimephasen und naher Kurve
// ---------------------------------------------------------------------------

function renderChart(r) {
  const svg = $("fin-chart-svg");
  const stage = $("fin-chart-stage");
  const tooltip = $("fin-tooltip");
  svg.innerHTML = "";
  const width = Math.max(320, Math.round(svg.clientWidth || 900));
  const height = Math.max(200, Math.round(svg.clientHeight || 260));
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

  const hist = (r.history || []).filter((h) => !isMissing(h.fsi) || !isMissing(h.near_median));
  const nearCount = hist.filter((h) => !isMissing(h.near_median)).length;
  renderLegend(nearCount >= MIN_CURVE_DAYS_FOR_LINE);
  if (hist.length < 2) {
    svg.appendChild(svgEl("text", { x: width / 2, y: height / 2, "text-anchor": "middle", fill: "var(--text-muted)", "font-size": 12 }, "Noch keine Kreditdaten. Der erste Abruf startet kurz nach dem Start der Software."));
    return;
  }

  const pad = { top: 12, right: 58, bottom: 24, left: 42 };
  const plotW = width - pad.left - pad.right;
  const plotH = height - pad.top - pad.bottom;
  const t0 = dayMs(hist[0].date);
  const t1 = dayMs(hist[hist.length - 1].date);
  const tSpan = Math.max(86400000, t1 - t0);
  const x = (iso) => pad.left + ((dayMs(iso) - t0) / tSpan) * plotW;

  const fsiVals = hist.map((h) => h.fsi).filter((v) => !isMissing(v));
  const yMin = Math.floor(Math.min(-1, ...fsiVals) * 2) / 2;
  const yMax = Math.ceil(Math.max(2, ...fsiVals) * 2) / 2;
  const y = (v) => pad.top + plotH - ((v - yMin) / (yMax - yMin)) * plotH;

  // Regimephasen als Hintergrund
  let segStart = 0;
  for (let i = 1; i <= hist.length; i++) {
    if (i === hist.length || hist[i].regime !== hist[segStart].regime) {
      const key = hist[segStart].regime;
      if (key !== "unbekannt") {
        const xa = x(hist[segStart].date);
        const xb = i === hist.length ? pad.left + plotW : x(hist[i].date);
        svg.appendChild(svgEl("rect", { x: xa, y: pad.top, width: Math.max(1, xb - xa), height: plotH, fill: REGIME_COLORS[key], opacity: 0.13 }));
      }
      segStart = i;
    }
  }

  // Raster und Achse links (Stressindex)
  for (let v = yMin; v <= yMax + 1e-9; v += 0.5) {
    if (Math.abs(v * 2 - Math.round(v * 2)) > 1e-9) continue;
    const yy = y(v);
    const major = Math.abs(v - Math.round(v)) < 1e-9;
    if (major) {
      svg.appendChild(svgEl("line", { x1: pad.left, x2: pad.left + plotW, y1: yy, y2: yy, stroke: "var(--chart-grid)", "stroke-width": 1 }));
      svg.appendChild(svgEl("text", { x: pad.left - 6, y: yy + 3.5, "text-anchor": "end", fill: "var(--text-muted)", "font-size": 10, "font-family": "var(--font-mono)" }, fmt(v, 0)));
    }
  }
  for (const [v, label] of [[1.0, "Eintritt"], [0.5, "Austritt"]]) {
    if (v < yMin || v > yMax) continue;
    svg.appendChild(svgEl("line", { x1: pad.left, x2: pad.left + plotW, y1: y(v), y2: y(v), stroke: "var(--accent-red)", "stroke-width": 1, "stroke-dasharray": "4 4", opacity: 0.6 }));
    svg.appendChild(svgEl("text", { x: pad.left + 4, y: y(v) - 3, fill: "var(--accent-red)", "font-size": 9.5, opacity: 0.8 }, label));
  }

  // Stressindex
  const fsiPts = hist.filter((h) => !isMissing(h.fsi)).map((h) => `${x(h.date).toFixed(1)},${y(h.fsi).toFixed(1)}`);
  if (fsiPts.length > 1) {
    svg.appendChild(svgEl("path", { d: `M ${fsiPts.join(" L ")}`, fill: "none", stroke: "var(--text-primary)", "stroke-width": 1.7, "stroke-linejoin": "round" }));
  }

  // Naher Median auf der rechten Achse, erst mit genug Historie: Wenige Punkte am rechten Rand
  // eines 400-Tage-Charts sähen aus wie ein senkrechter Sprung.
  const nearVals = hist.map((h) => h.near_median).filter((v) => !isMissing(v));
  if (nearVals.length >= MIN_CURVE_DAYS_FOR_LINE) {
    const nMin = Math.min(...nearVals);
    const nMax = Math.max(...nearVals);
    const span = Math.max(0.05, nMax - nMin);
    const lo = nMin - span * 0.1;
    const hi = nMax + span * 0.1;
    const yn = (v) => pad.top + plotH - ((v - lo) / (hi - lo)) * plotH;
    const pts = hist.filter((h) => !isMissing(h.near_median)).map((h) => `${x(h.date).toFixed(1)},${yn(h.near_median).toFixed(1)}`);
    svg.appendChild(svgEl("path", { d: `M ${pts.join(" L ")}`, fill: "none", stroke: "var(--accent-purple)", "stroke-width": 1.6, "stroke-linejoin": "round" }));
    for (const v of [lo + (hi - lo) * 0.1, (lo + hi) / 2, hi - (hi - lo) * 0.1]) {
      svg.appendChild(svgEl("text", { x: pad.left + plotW + 6, y: yn(v) + 3.5, fill: "var(--accent-purple)", "font-size": 10, "font-family": "var(--font-mono)" }, fmtUsd(v)));
    }
  }

  // Zeitachse
  const fmtShort = (iso) => new Intl.DateTimeFormat("de-DE", { month: "short", year: "2-digit", timeZone: "UTC" }).format(new Date(dayMs(iso)));
  const nTicks = Math.max(2, Math.min(8, Math.floor(plotW / 110)));
  for (let k = 0; k <= nTicks; k++) {
    const idx = Math.round((k / nTicks) * (hist.length - 1));
    const h = hist[idx];
    svg.appendChild(svgEl("text", { x: x(h.date), y: height - 6, "text-anchor": k === 0 ? "start" : k === nTicks ? "end" : "middle", fill: "var(--text-muted)", "font-size": 10 }, fmtShort(h.date)));
  }

  // Tooltip
  const cursor = svgEl("line", { x1: 0, x2: 0, y1: pad.top, y2: pad.top + plotH, stroke: "var(--border-strong)", "stroke-width": 1, visibility: "hidden" });
  svg.appendChild(cursor);
  const overlay = svgEl("rect", { x: pad.left, y: pad.top, width: plotW, height: plotH, fill: "transparent" });
  svg.appendChild(overlay);
  const pick = (clientX) => {
    const rect = svg.getBoundingClientRect();
    const px = ((clientX - rect.left) / rect.width) * width;
    const t = t0 + ((px - pad.left) / plotW) * tSpan;
    let best = hist[0];
    for (const h of hist) if (Math.abs(dayMs(h.date) - t) < Math.abs(dayMs(best.date) - t)) best = h;
    return best;
  };
  overlay.addEventListener("mousemove", (evt) => {
    const h = pick(evt.clientX);
    const xx = x(h.date);
    cursor.setAttribute("x1", xx);
    cursor.setAttribute("x2", xx);
    cursor.setAttribute("visibility", "visible");
    const html = `<div class="tt-title">${escapeHtml(fmtDay(h.date))} · ${escapeHtml(REGIME_SHORT[h.regime] || h.regime)}</div>
      <div class="tt-row"><span>Stressindex</span><span class="mono">${fmt(h.fsi)}</span></div>
      <div class="tt-row"><span>Makro · KI</span><span class="mono">${fmt(h.macro_block)} · ${fmt(h.ai_block)}</span></div>
      <div class="tt-row"><span>Naher Median</span><span class="mono">${fmtUsd(h.near_median)}</span></div>
      <div class="tt-row"><span>Basis</span><span class="mono">${fmtSigned(h.basis_pct, 1, "%")}</span></div>
      <div class="tt-row"><span>Bruch Preis · Kredit</span><span class="mono">${fmtPctOf(h.price_break_prob)} · ${fmtPctOf(h.credit_break_prob)}</span></div>`;
    tooltip.innerHTML = html;
    tooltip.hidden = false;
    const rect = stage.getBoundingClientRect();
    let left = evt.clientX - rect.left + 14;
    if (left + tooltip.offsetWidth > rect.width - 8) left = evt.clientX - rect.left - tooltip.offsetWidth - 14;
    tooltip.style.left = `${Math.max(8, left)}px`;
    tooltip.style.top = `${Math.max(8, evt.clientY - rect.top - tooltip.offsetHeight / 2)}px`;
  });
  overlay.addEventListener("mouseleave", () => {
    tooltip.hidden = true;
    cursor.setAttribute("visibility", "hidden");
  });
}

function renderLegend(withNear) {
  const item = (swatch, label) => `<span class="legend-item">${swatch}${escapeHtml(label)}</span>`;
  const line = (color) => `<span class="legend-swatch" style="background:${color};height:2px"></span>`;
  const box = (color) => `<span class="legend-swatch" style="background:${color};opacity:.35;height:10px"></span>`;
  $("fin-legend").innerHTML = [
    item(line("var(--text-primary)"), "Stressindex"),
    item(line("var(--accent-purple)"), withNear ? "Naher Median (rechts)" : `Naher Median ab ${MIN_CURVE_DAYS_FOR_LINE} Kurventagen`),
    ...["ruhig", "preisdruck", "entkoppelt", "uebertragung"].map((k) => item(box(REGIME_COLORS[k]), REGIME_SHORT[k])),
  ].join("");
}
