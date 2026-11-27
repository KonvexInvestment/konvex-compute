/**
 * Interaktive SVG-Charts für Konvex Compute (Tauri 2):
 * 1. Haupt-Chart: Ornn-Spot-Historie + Forward-Kurve + P25/P75 & P10/P90 + Kern-Mean/Roh-Mean + 1T/1W/Snapshot-Overlay.
 * 2. PDF-Verteilungs-Histogramm für den gewählten Verfall.
 * 3. Kurvenverschiebungs-Chart (Δ 1T vs. Δ 1W nach Laufzeit).
 * 4. CLOB-Strike-Verlauf (30 Tage).
 */

const SVG_NS = "http://www.w3.org/2000/svg";

const num2 = new Intl.NumberFormat("de-DE", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const num1 = new Intl.NumberFormat("de-DE", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const intFmt = new Intl.NumberFormat("de-DE", { maximumFractionDigits: 0 });

// Geschütztes Leerzeichen: Einheit bricht nie allein in die nächste Zeile
const NBSP = "\u00a0";

// Auf die angezeigten Stellen runden; aus -0,00 wird 0,00
function roundShown(v, digits) {
  const f = 10 ** digits;
  const r = Math.round(v * f) / f;
  return r === 0 ? 0 : r;
}

function isMissing(v) {
  return v === null || v === undefined || Number.isNaN(v);
}

export function fmtUsd(v) {
  if (isMissing(v)) return "k. A.";
  return `${num2.format(roundShown(v, 2))}${NBSP}$`;
}

export function fmtSignedUsd(v) {
  if (isMissing(v)) return "k. A.";
  const r = roundShown(v, 2);
  const prefix = r > 0 ? "+" : "";
  return `${prefix}${num2.format(r)}${NBSP}$`;
}

export function fmtPct(v, signed = true) {
  if (isMissing(v)) return "k. A.";
  const r = roundShown(v, 1);
  const prefix = signed && r > 0 ? "+" : "";
  return `${prefix}${num1.format(r)}${NBSP}%`;
}

export function fmtNum1(v) {
  if (isMissing(v)) return "k. A.";
  return num1.format(roundShown(v, 1));
}

export function fmtSignedNum1(v, unit) {
  if (isMissing(v)) return "k. A.";
  const r = roundShown(v, 1);
  const prefix = r > 0 ? "+" : "";
  return `${prefix}${num1.format(r)}${NBSP}${unit}`;
}

export function fmtCompactUsd(v) {
  if (isMissing(v)) return "k. A.";
  if (v >= 1_000_000) return `${num2.format(v / 1_000_000)}${NBSP}Mio.${NBSP}$`;
  if (v >= 1_000) return `${num1.format(v / 1_000)}${NBSP}Tsd.${NBSP}$`;
  return `${intFmt.format(v)}${NBSP}$`;
}

export function escapeHtml(str) {
  return String(str ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function el(tag, attrs = {}, text = null) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v !== null && v !== undefined) node.setAttribute(k, String(v));
  }
  if (text !== null) node.textContent = text;
  return node;
}

function parseIsoDay(iso) {
  const [y, m, d] = iso.slice(0, 10).split("-").map(Number);
  return Date.UTC(y, m - 1, d);
}

function fmtDateDe(iso) {
  const dt = new Date(parseIsoDay(iso));
  return new Intl.DateTimeFormat("de-DE", {
    day: "2-digit",
    month: "short",
    year: "numeric",
    timeZone: "UTC",
  }).format(dt);
}

function niceTicks(minVal, maxVal, count = 6) {
  const span = Math.max(1e-6, maxVal - minVal);
  const rawStep = span / Math.max(2, count);
  const mag = 10 ** Math.floor(Math.log10(rawStep));
  const norm = rawStep / mag;
  let step = mag;
  if (norm > 5) step = 10 * mag;
  else if (norm > 2) step = 5 * mag;
  else if (norm > 1) step = 2 * mag;
  const start = Math.floor(minVal / step) * step;
  const end = Math.ceil(maxVal / step) * step;
  const ticks = [];
  for (let v = start; v <= end + step * 0.25; v += step) {
    ticks.push(Number(v.toFixed(6)));
  }
  return ticks;
}

function showTooltip(tooltipEl, stageEl, clientX, clientY, html) {
  tooltipEl.innerHTML = html;
  tooltipEl.hidden = false;
  const rect = stageEl.getBoundingClientRect();
  const tw = tooltipEl.offsetWidth;
  const th = tooltipEl.offsetHeight;
  let x = clientX - rect.left + 14;
  let y = clientY - rect.top - th / 2;
  if (x + tw > rect.width - 8) x = clientX - rect.left - tw - 14;
  if (x < 8) x = 8;
  if (y < 8) y = 8;
  if (y + th > rect.height - 8) y = rect.height - th - 8;
  tooltipEl.style.left = `${x}px`;
  tooltipEl.style.top = `${y}px`;
}

export function renderForwardChart(config) {
  const {
    svg,
    stage,
    tooltip,
    gpuData,
    todayUtc,
    selectedExpirySlug,
    compareCurve,
    historyWindow,
    showIqr,
    showOuter,
    showMeanTrimmed,
    showMeanRaw,
    onSelectExpiry,
  } = config;

  svg.innerHTML = "";
  tooltip.hidden = true;

  const width = Math.max(360, Math.round(stage.clientWidth || 980));
  const height = Math.max(240, Math.round(stage.clientHeight || 365));
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

  const pad = { top: 40, right: 70, bottom: 32, left: 16 };
  const plotW = width - pad.left - pad.right;
  const plotH = height - pad.top - pad.bottom;

  const color = gpuData.meta.color || "#38b6ff";
  const bandRgb = gpuData.meta.band_rgb || "56, 182, 255";
  const spotPrice = gpuData.spot_price;

  const todayTs = todayUtc ? parseIsoDay(todayUtc) : Date.now();
  let hist = (gpuData.spot_history || []).slice();
  if (historyWindow !== "all" && hist.length > 0) {
    const days = Number(historyWindow) || 90;
    const cutoff = todayTs - days * 86400 * 1000;
    hist = hist.filter((r) => parseIsoDay(r.date) >= cutoff);
  }
  const expiries = (gpuData.expiries || []).filter((e) => e.median !== null && e.median !== undefined);

  if (!hist.length && !expiries.length) {
    svg.appendChild(el("text", {
      x: width / 2,
      y: height / 2,
      "text-anchor": "middle",
      fill: "var(--text-muted)",
      "font-size": 13,
    }, "Lade Marktdaten von Polymarket und Ornn..."));
    return;
  }

  const splitRatio = hist.length >= 2 ? 0.25 : 0.04;
  const splitX = pad.left + plotW * splitRatio;
  const rightW = pad.left + plotW - splitX;

  const histStartTs = hist.length ? parseIsoDay(hist[0].date) : todayTs - 86400 * 1000;
  const histSpan = Math.max(86400 * 1000, todayTs - histStartTs);

  const maxExpiryTs = expiries.length
    ? parseIsoDay(expiries[expiries.length - 1].delivery_date)
    : todayTs + 365 * 86400 * 1000;
  const fwdSpan = Math.max(86400 * 1000 * 30, maxExpiryTs - todayTs);

  const xHist = (ts) => pad.left + ((ts - histStartTs) / histSpan) * (splitX - pad.left);
  const xFwd = (ts) => splitX + ((ts - todayTs) / fwdSpan) * (rightW - 14);

  const yVals = [];
  if (spotPrice !== null && spotPrice !== undefined) yVals.push(spotPrice);
  for (const r of hist) yVals.push(r.index_value);
  for (const e of expiries) {
    if (e.median !== null) yVals.push(e.median);
    if (showMeanTrimmed && e.mean_trimmed !== null) yVals.push(e.mean_trimmed);
    if (showMeanRaw && e.mean_raw !== null) yVals.push(e.mean_raw);
    if (showIqr) {
      if (e.p25 !== null) yVals.push(e.p25);
      if (e.p75 !== null) yVals.push(e.p75);
    }
    if (showOuter) {
      if (e.p10 !== null) yVals.push(e.p10);
      if (e.p90 !== null) yVals.push(e.p90);
    }
  }
  if (compareCurve && compareCurve.expiries) {
    if (compareCurve.spot_price) yVals.push(compareCurve.spot_price);
    for (const ce of compareCurve.expiries) {
      if (ce.median !== null && ce.median !== undefined) yVals.push(ce.median);
    }
  }

  let yMin = Math.min(...yVals);
  let yMax = Math.max(...yVals);
  const yPad = Math.max(0.15, (yMax - yMin) * 0.12);
  yMin = Math.max(0, yMin - yPad);
  yMax = yMax + yPad;

  const yTicks = niceTicks(yMin, yMax, 6).filter((t) => t >= 0);
  if (yTicks.length >= 2) {
    yMin = Math.min(yMin, yTicks[0]);
    yMax = Math.max(yMax, yTicks[yTicks.length - 1]);
  }
  const yScale = (v) => pad.top + plotH - ((v - yMin) / Math.max(1e-6, yMax - yMin)) * plotH;

  const defs = el("defs");
  const gradId = `hist-grad-${gpuData.meta.key}`;
  const grad = el("linearGradient", { id: gradId, x1: "0", y1: "0", x2: "0", y2: "1" });
  grad.appendChild(el("stop", { offset: "0%", "stop-color": color, "stop-opacity": "0.24" }));
  grad.appendChild(el("stop", { offset: "100%", "stop-color": color, "stop-opacity": "0.0" }));
  defs.appendChild(grad);
  svg.appendChild(defs);

  // Horizontale Gridlines + Y-Achse rechts
  for (const t of yTicks) {
    const yy = yScale(t);
    if (yy < pad.top - 4 || yy > pad.top + plotH + 4) continue;
    svg.appendChild(el("line", {
      x1: pad.left,
      x2: pad.left + plotW,
      y1: yy,
      y2: yy,
      stroke: "var(--chart-grid)",
      "stroke-width": 1,
    }));
    svg.appendChild(el("text", {
      x: pad.left + plotW + 8,
      y: yy + 3.5,
      fill: "var(--text-muted)",
      "font-size": 10.5,
      "font-family": "var(--font-mono)",
    }, fmtUsd(t)));
  }

  // Horizontale Spot-Referenzlinie
  if (spotPrice !== null && spotPrice !== undefined) {
    const sy = yScale(spotPrice);
    svg.appendChild(el("line", {
      x1: splitX,
      x2: pad.left + plotW,
      y1: sy,
      y2: sy,
      stroke: "var(--text-dimmed)",
      "stroke-opacity": "0.5",
      "stroke-dasharray": "4 4",
      "stroke-width": 1,
    }));
  }

  const anchorX = splitX;
  const anchorY = spotPrice !== null && spotPrice !== undefined ? yScale(spotPrice) : null;

  function drawBand(upperKey, lowerKey, opacity) {
    const pts = expiries.filter((e) => e[upperKey] !== null && e[lowerKey] !== null);
    if (!pts.length) return;
    const topPts = [];
    const botPts = [];
    if (anchorY !== null) {
      topPts.push(`${anchorX.toFixed(1)},${anchorY.toFixed(1)}`);
      botPts.push(`${anchorX.toFixed(1)},${anchorY.toFixed(1)}`);
    }
    for (const e of pts) {
      const px = xFwd(parseIsoDay(e.delivery_date));
      topPts.push(`${px.toFixed(1)},${yScale(e[upperKey]).toFixed(1)}`);
      botPts.push(`${px.toFixed(1)},${yScale(e[lowerKey]).toFixed(1)}`);
    }
    svg.appendChild(el("polygon", {
      points: [...topPts, ...botPts.reverse()].join(" "),
      fill: `rgba(${bandRgb}, ${opacity})`,
    }));
  }

  if (showOuter) drawBand("p90", "p10", 0.1);
  if (showIqr) drawBand("p75", "p25", 0.22);

  // Historische Spot-Linie
  if (hist.length >= 2) {
    const linePts = hist.map((r) => `${xHist(parseIsoDay(r.date)).toFixed(1)},${yScale(r.index_value).toFixed(1)}`);
    const lastX = xHist(parseIsoDay(hist[hist.length - 1].date));
    const firstX = xHist(parseIsoDay(hist[0].date));
    const baseY = pad.top + plotH;
    svg.appendChild(el("path", {
      d: `M ${linePts.join(" L ")} L ${lastX.toFixed(1)},${baseY} L ${firstX.toFixed(1)},${baseY} Z`,
      fill: `url(#${gradId})`,
    }));
    svg.appendChild(el("path", {
      d: `M ${linePts.join(" L ")}`,
      fill: "none",
      stroke: color,
      "stroke-width": 2,
      "stroke-linejoin": "round",
      "stroke-linecap": "round",
    }));
  }

  // Heute-Linie
  svg.appendChild(el("line", {
    x1: splitX,
    x2: splitX,
    y1: pad.top - 8,
    y2: pad.top + plotH,
    stroke: "var(--border-strong)",
    "stroke-dasharray": "3 3",
    "stroke-width": 1.2,
  }));
  if (spotPrice !== null && spotPrice !== undefined) {
    svg.appendChild(el("text", {
      x: splitX,
      y: pad.top - 14,
      "text-anchor": "middle",
      fill: color,
      "font-size": 11.5,
      "font-weight": 700,
      "font-family": "var(--font-mono)",
    }, fmtUsd(spotPrice)));
  }

  // Vergleichskurve (1T / 1W / 1M / Snapshot)
  if (compareCurve && Array.isArray(compareCurve.expiries) && compareCurve.expiries.length) {
    const cPts = [];
    if (compareCurve.spot_price !== null && compareCurve.spot_price !== undefined) {
      cPts.push(`${splitX.toFixed(1)},${yScale(compareCurve.spot_price).toFixed(1)}`);
    }
    for (const ce of compareCurve.expiries) {
      if (ce.median === null || ce.median === undefined) continue;
      const cx = xFwd(parseIsoDay(ce.delivery_date));
      if (cx >= splitX - 2 && cx <= pad.left + plotW + 2) {
        cPts.push(`${cx.toFixed(1)},${yScale(ce.median).toFixed(1)}`);
      }
    }
    if (cPts.length >= 2) {
      svg.appendChild(el("path", {
        d: `M ${cPts.join(" L ")}`,
        fill: "none",
        stroke: "var(--accent-amber)",
        "stroke-width": 1.7,
        "stroke-dasharray": "6 4",
        "stroke-opacity": "0.9",
      }));
    }
  }

  // Kern-Erwartungswert (mindestens P5 bis P95, je nach Strike-Abdeckung enger)
  if (showMeanTrimmed && expiries.length) {
    const pts = [];
    if (anchorY !== null) pts.push(`${anchorX.toFixed(1)},${anchorY.toFixed(1)}`);
    for (const e of expiries) {
      if (e.mean_trimmed !== null && e.mean_trimmed !== undefined) {
        pts.push(`${xFwd(parseIsoDay(e.delivery_date)).toFixed(1)},${yScale(e.mean_trimmed).toFixed(1)}`);
      }
    }
    if (pts.length >= 2) {
      svg.appendChild(el("path", {
        d: `M ${pts.join(" L ")}`,
        fill: "none",
        stroke: "#22d3ee",
        "stroke-width": 1.6,
        "stroke-dasharray": "4 3",
      }));
    }
  }

  // Roh-Erwartungswert (inkl. Tail)
  if (showMeanRaw && expiries.length) {
    const pts = [];
    if (anchorY !== null) pts.push(`${anchorX.toFixed(1)},${anchorY.toFixed(1)}`);
    for (const e of expiries) {
      if (e.mean_raw !== null && e.mean_raw !== undefined) {
        pts.push(`${xFwd(parseIsoDay(e.delivery_date)).toFixed(1)},${yScale(e.mean_raw).toFixed(1)}`);
      }
    }
    if (pts.length >= 2) {
      svg.appendChild(el("path", {
        d: `M ${pts.join(" L ")}`,
        fill: "none",
        stroke: "var(--text-muted)",
        "stroke-width": 1.3,
        "stroke-dasharray": "2 3",
        "stroke-opacity": "0.85",
      }));
    }
  }

  // Haupt-Forward-Kurve (Median P50)
  if (expiries.length) {
    const fPts = [];
    if (anchorY !== null) fPts.push(`${anchorX.toFixed(1)},${anchorY.toFixed(1)}`);
    for (const e of expiries) {
      fPts.push(`${xFwd(parseIsoDay(e.delivery_date)).toFixed(1)},${yScale(e.median).toFixed(1)}`);
    }
    svg.appendChild(el("path", {
      d: `M ${fPts.join(" L ")}`,
      fill: "none",
      stroke: color,
      "stroke-width": 2,
      "stroke-dasharray": "3 4",
    }));
  }

  const highlightG = el("g");
  svg.appendChild(highlightG);

  function drawActiveHighlight(expiry) {
    highlightG.innerHTML = "";
    if (!expiry) return;
    const ex = xFwd(parseIsoDay(expiry.delivery_date));
    const ey = yScale(expiry.median);

    highlightG.appendChild(el("line", {
      x1: ex,
      x2: ex,
      y1: pad.top - 4,
      y2: pad.top + plotH,
      stroke: color,
      "stroke-opacity": "0.45",
      "stroke-dasharray": "3 3",
      "stroke-width": 1.2,
    }));

    if (showIqr && expiry.p25 !== null && expiry.p75 !== null) {
      const yTop = yScale(expiry.p75);
      const yBot = yScale(expiry.p25);
      highlightG.appendChild(el("rect", {
        x: ex - 5.5,
        y: Math.min(yTop, yBot),
        width: 11,
        height: Math.max(8, Math.abs(yBot - yTop)),
        rx: 5.5,
        fill: `rgba(${bandRgb}, 0.34)`,
        stroke: `rgba(${bandRgb}, 0.72)`,
        "stroke-width": 1,
      }));
    }

    const pillText = `${expiry.long_label} · ${fmtUsd(expiry.median)}`;
    const estW = Math.max(150, pillText.length * 6.8 + 20);
    const pillX = Math.max(pad.left + 4, Math.min(pad.left + plotW - estW - 4, ex - estW / 2));
    highlightG.appendChild(el("rect", {
      x: pillX,
      y: 7,
      width: estW,
      height: 23,
      rx: 11.5,
      fill: color,
    }));
    highlightG.appendChild(el("text", {
      x: pillX + estW / 2,
      y: 22,
      "text-anchor": "middle",
      fill: "var(--on-accent)",
      "font-size": 11,
      "font-weight": 700,
      "font-family": "var(--font-mono)",
    }, pillText));

    highlightG.appendChild(el("circle", {
      cx: ex,
      cy: ey,
      r: 5.5,
      fill: "var(--bg-primary)",
      stroke: color,
      "stroke-width": 2.5,
    }));
  }

  const selectedExp = expiries.find((e) => e.slug === selectedExpirySlug) || expiries[0] || null;
  for (const e of expiries) {
    svg.appendChild(el("circle", {
      cx: xFwd(parseIsoDay(e.delivery_date)),
      cy: yScale(e.median),
      r: selectedExp && e.slug === selectedExp.slug ? 4.8 : 3.8,
      fill: "var(--bg-panel)",
      stroke: color,
      "stroke-width": 2,
    }));
  }
  drawActiveHighlight(selectedExp);

  // X-Achse
  const xLabels = [];
  if (hist.length >= 2) {
    xLabels.push({ x: pad.left + 4, anchor: "start", text: fmtDateDe(hist[0].date) });
  }
  xLabels.push({ x: splitX, anchor: "middle", text: "Heute" });
  const pickIdx = new Set([0, Math.floor(expiries.length / 2), expiries.length - 1]);
  let lastX = splitX + 44;
  expiries.forEach((e, i) => {
    const ex = xFwd(parseIsoDay(e.delivery_date));
    const isLast = i === expiries.length - 1;
    if ((pickIdx.has(i) && ex > lastX + 60) || (isLast && ex > lastX + 42)) {
      xLabels.push({ x: ex, anchor: isLast ? "end" : "middle", text: e.long_label });
      lastX = ex;
    }
  });
  for (const lbl of xLabels) {
    svg.appendChild(el("text", {
      x: lbl.x,
      y: height - 9,
      "text-anchor": lbl.anchor,
      fill: "var(--text-muted)",
      "font-size": 10.5,
    }, lbl.text));
  }

  // Legende oben rechts, wenn Vergleich aktiv
  if (compareCurve && compareCurve.label) {
    const txt = `Vergleich: ${compareCurve.label}`;
    svg.appendChild(el("text", {
      x: pad.left + plotW - 6,
      y: 22,
      "text-anchor": "end",
      fill: "var(--accent-amber)",
      "font-size": 11,
      "font-weight": 600,
    }, txt));
  }

  const hoverLine = el("line", {
    x1: 0, x2: 0, y1: pad.top, y2: pad.top + plotH,
    stroke: "var(--text-muted)", "stroke-opacity": "0", "stroke-width": 1, "stroke-dasharray": "2 2",
  });
  const hoverDot = el("circle", {
    cx: 0, cy: 0, r: 4.5, fill: color, stroke: "var(--bg-primary)", "stroke-width": 2, opacity: "0",
  });
  svg.appendChild(hoverLine);
  svg.appendChild(hoverDot);

  const overlay = el("rect", {
    x: pad.left, y: pad.top, width: plotW, height: plotH,
    fill: "transparent", style: "cursor:crosshair",
  });
  svg.appendChild(overlay);

  const histScreen = hist.map((r) => ({ x: xHist(parseIsoDay(r.date)), y: yScale(r.index_value), row: r }));
  const expScreen = expiries.map((e) => ({ x: xFwd(parseIsoDay(e.delivery_date)), y: yScale(e.median), exp: e }));

  const nearestExpiry = (mx) => {
    let best = null;
    let bestD = Infinity;
    for (const item of expScreen) {
      const d = Math.abs(item.x - mx);
      if (d < bestD) { bestD = d; best = item; }
    }
    return best;
  };

  overlay.addEventListener("mousemove", (evt) => {
    const rect = svg.getBoundingClientRect();
    const mx = ((evt.clientX - rect.left) / rect.width) * width;

    if (mx < splitX && histScreen.length) {
      let best = histScreen[0];
      let bestD = Infinity;
      for (const p of histScreen) {
        const d = Math.abs(p.x - mx);
        if (d < bestD) { bestD = d; best = p; }
      }
      hoverLine.setAttribute("x1", best.x);
      hoverLine.setAttribute("x2", best.x);
      hoverLine.setAttribute("stroke-opacity", "0.7");
      hoverDot.setAttribute("cx", best.x);
      hoverDot.setAttribute("cy", best.y);
      hoverDot.setAttribute("opacity", "1");

      const vsNow = spotPrice ? ((spotPrice / best.row.index_value) - 1) * 100 : null;
      showTooltip(tooltip, stage, evt.clientX, evt.clientY, `
        <div class="tt-title">${escapeHtml(fmtDateDe(best.row.date))} · Spot</div>
        <div class="tt-row"><span>Ornn-Index</span><strong>${fmtUsd(best.row.index_value)}</strong></div>
        ${vsNow !== null ? `<div class="tt-row"><span>Bis heute</span><strong>${fmtPct(vsNow)}</strong></div>` : ""}
      `);
      return;
    }

    const best = nearestExpiry(mx);
    if (!best) return;
    hoverLine.setAttribute("stroke-opacity", "0");
    hoverDot.setAttribute("opacity", "0");
    drawActiveHighlight(best.exp);

    const e = best.exp;
    let cmpRow = "";
    if (compareCurve && compareCurve.expiries) {
      const match = compareCurve.expiries.find((c) => c.slug === e.slug);
      if (match && match.median !== null && match.median !== undefined) {
        const diff = e.median - match.median;
        const pct = match.median > 0 ? (e.median / match.median - 1) * 100 : null;
        cmpRow = `<div class="tt-row"><span>${escapeHtml(compareCurve.shortLabel || "Vergleich")}</span><strong class="${diff >= 0 ? "is-pos" : "is-neg"}">${fmtUsd(match.median)} (${fmtSignedUsd(diff)}${pct !== null ? ` / ${fmtPct(pct)}` : ""})</strong></div>`;
      }
    }

    const bandLbl = e.trimmed_band_label ? ` (${escapeHtml(e.trimmed_band_label)})` : "";
    showTooltip(tooltip, stage, evt.clientX, evt.clientY, `
      <div class="tt-title">${escapeHtml(e.month_label)}</div>
      <div class="tt-row"><span>Median (P50)</span><strong>${fmtUsd(e.median)}</strong></div>
      <div class="tt-row"><span>ggü. Spot</span><strong class="${(e.vs_spot_pct || 0) >= 0 ? "is-pos" : "is-neg"}">${fmtPct(e.vs_spot_pct)}</strong></div>
      <div class="tt-row"><span>P25 bis P75</span><strong>${fmtUsd(e.p25)} bis ${fmtUsd(e.p75)}</strong></div>
      <div class="tt-row"><span>Kern-Mean${bandLbl}</span><strong>${fmtUsd(e.mean_trimmed)}</strong></div>
      <div class="tt-row"><span>Roh-Mean (inkl. Tail)</span><strong>${fmtUsd(e.mean_raw)}</strong></div>
      ${cmpRow}
      <div class="tt-row"><span>Liquidität</span><strong>${fmtCompactUsd(e.liquidity)}</strong></div>
    `);
  });

  overlay.addEventListener("mouseleave", () => {
    tooltip.hidden = true;
    hoverLine.setAttribute("stroke-opacity", "0");
    hoverDot.setAttribute("opacity", "0");
    drawActiveHighlight(selectedExp);
  });

  overlay.addEventListener("click", (evt) => {
    const rect = svg.getBoundingClientRect();
    const mx = ((evt.clientX - rect.left) / rect.width) * width;
    if (mx >= splitX) {
      const best = nearestExpiry(mx);
      if (best && onSelectExpiry) onSelectExpiry(best.exp.slug);
    }
  });
}

/** Deutsches Bucket-Label aus den Grenzen (das gespeicherte Rust-Label nutzt Dezimalpunkte). */
function bucketLabel(b) {
  if (b.high === null || b.high === undefined) return `über ${fmtUsd(b.low)}`;
  if (b.is_tail) return `unter ${fmtUsd(b.high)}`;
  return `${fmtUsd(b.low)} bis ${fmtUsd(b.high)}`;
}

export function renderDistributionChart({ svg, stage, tooltip, expiry, spotPrice, color, bandRgb }) {
  svg.innerHTML = "";
  tooltip.hidden = true;
  const width = Math.max(300, Math.round(stage.clientWidth || 460));
  const height = Math.max(190, Math.round(stage.clientHeight || 235));
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

  const buckets = (expiry && expiry.buckets) || [];
  if (!buckets.length) {
    svg.appendChild(el("text", { x: width / 2, y: height / 2, "text-anchor": "middle", fill: "var(--text-muted)", "font-size": 12.5 }, "Keine Verteilung verfügbar."));
    return;
  }

  // Preisachse statt gleich breiter Balken: Breite = Preisspanne, Höhe = Wahrscheinlichkeit je 1 $.
  // Damit entspricht die Fläche der Wahrscheinlichkeit und breite Spannen wirken nicht wie ein Gipfel.
  const interior = buckets.filter((b) => !b.is_tail && b.low !== null && b.high !== null);
  const lastInteriorW = interior.length ? interior[interior.length - 1].high - interior[interior.length - 1].low : 0;
  const shapes = buckets.map((b) => {
    const open = b.high === null || b.high === undefined;
    const lo = b.low ?? 0;
    const hi = open ? lo + Math.max(lastInteriorW, lo * 0.15, 0.1) : b.high;
    const w = Math.max(1e-6, hi - lo);
    return { b, lo, hi, open, dens: b.prob / w };
  });

  let xMin = Math.min(...shapes.map((sh) => sh.lo));
  let xMax = Math.max(...shapes.map((sh) => sh.hi));
  if (spotPrice !== null && spotPrice !== undefined && Number.isFinite(spotPrice)) {
    xMin = Math.min(xMin, spotPrice);
    xMax = Math.max(xMax, spotPrice);
  }
  const xSpan = Math.max(1e-6, xMax - xMin);

  const pad = { top: 24, right: 16, bottom: 40, left: 16 };
  const plotW = width - pad.left - pad.right;
  const xScale = (v) => pad.left + ((v - xMin) / xSpan) * plotW;

  // Markierungen: Median und heutiger Spot
  // (Spot neutral grau gestrichelt wie im Hauptchart, damit er nie mit der GPU-Farbe verschmilzt)
  const markers = [];
  if (expiry.median !== null && expiry.median !== undefined) markers.push({ v: expiry.median, label: "Median", stroke: "var(--text-primary)", dash: null });
  if (spotPrice !== null && spotPrice !== undefined && Number.isFinite(spotPrice)) markers.push({ v: spotPrice, label: "Spot heute", stroke: "var(--text-muted)", dash: "4 3" });
  markers.sort((a, b) => a.v - b.v);
  // Beschriftung mittig über der Linie, am Rand in die Fläche geschoben (geschätzte Textbreite)
  for (const m of markers) {
    m.x = xScale(m.v);
    m.text = `${m.label} ${fmtUsd(m.v)}`;
    m.w = m.text.length * 6.2;
    m.left = Math.min(Math.max(m.x - m.w / 2, pad.left), pad.left + plotW - m.w);
  }
  // Würden sich die Beschriftungen berühren, kommen sie auf zwei Zeilen
  const twoRows = markers.length === 2 && markers[0].left + markers[0].w + 8 > markers[1].left;
  markers.forEach((m, i) => { m.y = twoRows ? 11 + i * 12 : 13; });
  if (twoRows) pad.top = 36;
  // Linien beginnen unter der letzten Textzeile, kreuzen also keine Beschriftung
  const markerLineTop = (twoRows ? 23 : 13) + 4;

  const plotH = height - pad.top - pad.bottom;
  const maxDens = Math.max(1e-9, ...shapes.map((sh) => sh.dens));
  const yScale = (d) => pad.top + plotH - (d / maxDens) * plotH;
  const baseY = pad.top + plotH;

  for (const frac of [0.25, 0.5, 0.75, 1]) {
    const yy = pad.top + plotH - plotH * frac;
    svg.appendChild(el("line", { x1: pad.left, x2: pad.left + plotW, y1: yy, y2: yy, stroke: "var(--chart-grid)", "stroke-width": 1 }));
  }
  svg.appendChild(el("line", { x1: pad.left, x2: pad.left + plotW, y1: baseY, y2: baseY, stroke: "var(--border-strong)", "stroke-width": 1 }));

  const overlapsIqr = (b) =>
    expiry.p25 !== null &&
    expiry.p75 !== null &&
    (b.low === null || b.low < expiry.p75) &&
    (b.high === null || b.high > expiry.p25);

  for (const sh of shapes) {
    const x0 = xScale(sh.lo);
    const x1 = xScale(sh.hi);
    const bw = Math.max(1.5, x1 - x0 - 1);
    const by = yScale(sh.dens);
    const inIqr = overlapsIqr(sh.b);
    svg.appendChild(el("rect", {
      x: x0 + 0.5,
      y: by,
      width: bw,
      height: Math.max(1, baseY - by),
      rx: 1.5,
      fill: inIqr ? color : `rgba(${bandRgb}, 0.32)`,
      "fill-opacity": sh.b.is_tail ? 0.55 : 1,
      stroke: inIqr ? color : `rgba(${bandRgb}, 0.65)`,
      "stroke-width": 1,
      "stroke-dasharray": sh.b.is_tail ? "3 2" : null,
    }));
  }

  // Preis-Ticks
  const tickFmt = new Intl.NumberFormat("de-DE", { maximumFractionDigits: xSpan < 3 ? 2 : xSpan < 12 ? 1 : 0 });
  for (const t of niceTicks(xMin, xMax, Math.max(3, Math.floor(plotW / 70)))) {
    if (t < xMin - 1e-9 || t > xMax + 1e-9) continue;
    const xx = xScale(t);
    svg.appendChild(el("line", { x1: xx, x2: xx, y1: baseY, y2: baseY + 4, stroke: "var(--border-strong)", "stroke-width": 1 }));
    svg.appendChild(el("text", { x: xx, y: baseY + 15, "text-anchor": "middle", fill: "var(--text-muted)", "font-size": 9.5, "font-family": "var(--font-mono)" }, `${tickFmt.format(t)} $`));
  }

  for (const m of markers) {
    svg.appendChild(el("line", { x1: m.x, x2: m.x, y1: markerLineTop, y2: baseY, stroke: m.stroke, "stroke-width": 1.3, "stroke-dasharray": m.dash, "stroke-opacity": 0.9 }));
    svg.appendChild(el("text", { x: m.left, y: m.y, "text-anchor": "start", fill: m.stroke, "font-size": 10, "font-weight": 600 }, m.text));
  }

  svg.appendChild(el("text", { x: pad.left, y: height - 5, fill: "var(--text-dimmed)", "font-size": 10.5 },
    `Hervorgehoben: P25 bis P75 (${fmtUsd(expiry.p25)} bis ${fmtUsd(expiry.p75)}) · gestrichelt: Randbereiche`));

  // Hover über die ganze Fläche: findet den Bucket an der Mausposition (auch sehr schmale)
  const overlay = el("rect", { x: pad.left, y: pad.top, width: plotW, height: plotH, fill: "transparent", style: "cursor:crosshair" });
  overlay.addEventListener("mousemove", (evt) => {
    const rect = svg.getBoundingClientRect();
    const mx = ((evt.clientX - rect.left) / rect.width) * width;
    const v = xMin + ((mx - pad.left) / plotW) * xSpan;
    const hit = shapes.find((sh) => v >= sh.lo && v < sh.hi) || null;
    if (!hit) { tooltip.hidden = true; return; }
    const b = hit.b;
    const kind = hit.open ? "Oberer Rand (nach oben offen)" : b.is_tail ? "Unterer Rand (geschätzt)" : "Preisspanne";
    showTooltip(tooltip, stage, evt.clientX, evt.clientY, `
      <div class="tt-title">${escapeHtml(bucketLabel(b))}</div>
      <div class="tt-row"><span>Wahrscheinlichkeit</span><strong>${num1.format(b.prob * 100)} %</strong></div>
      <div class="tt-row"><span>Typ</span><strong>${kind}</strong></div>
    `);
  });
  overlay.addEventListener("mouseleave", () => { tooltip.hidden = true; });
  svg.appendChild(overlay);
}

/**
 * Gruppiertes Balkendiagramm: Kurvenverschiebung Δ 1T vs. Δ 1W je Verfall (in $/Std.).
 */
export function renderCurveShiftChart({ svg, stage, tooltip, gpu }) {
  svg.innerHTML = "";
  tooltip.hidden = true;
  const width = Math.max(360, Math.round(stage.clientWidth || 960));
  const height = Math.max(210, Math.round(stage.clientHeight || 270));
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

  const exps = gpu.expiries || [];
  const hasAny = exps.some((e) => e.delta_1d_usd !== null || e.delta_7d_usd !== null);
  if (!exps.length || !hasAny) {
    svg.appendChild(el("text", {
      x: width / 2, y: height / 2, "text-anchor": "middle", fill: "var(--text-muted)", "font-size": 12.5,
    }, "Noch keine historischen Vergleichstage in SQLite. Klicke rechts oben auf „14T-Backfill aktualisieren“."));
    return;
  }

  const pad = { top: 28, right: 64, bottom: 34, left: 20 };
  const plotW = width - pad.left - pad.right;
  const plotH = height - pad.top - pad.bottom;

  const vals = [0];
  for (const e of exps) {
    if (e.delta_1d_usd !== null) vals.push(e.delta_1d_usd);
    if (e.delta_7d_usd !== null) vals.push(e.delta_7d_usd);
  }
  const maxAbs = Math.max(0.1, ...vals.map((v) => Math.abs(v))) * 1.18;
  const yTicks = niceTicks(-maxAbs, maxAbs, 5);
  const yMin = Math.min(-maxAbs, yTicks[0] ?? -maxAbs);
  const yMax = Math.max(maxAbs, yTicks[yTicks.length - 1] ?? maxAbs);
  const yScale = (v) => pad.top + plotH - ((v - yMin) / Math.max(1e-6, yMax - yMin)) * plotH;
  const yZero = yScale(0);

  for (const t of yTicks) {
    const yy = yScale(t);
    if (yy < pad.top - 4 || yy > pad.top + plotH + 4) continue;
    svg.appendChild(el("line", {
      x1: pad.left, x2: pad.left + plotW, y1: yy, y2: yy,
      stroke: Math.abs(t) < 1e-6 ? "var(--border-strong)" : "var(--chart-grid)",
      "stroke-width": Math.abs(t) < 1e-6 ? 1.4 : 1,
    }));
    svg.appendChild(el("text", {
      x: pad.left + plotW + 8, y: yy + 3.5, fill: "var(--text-muted)", "font-size": 10.5, "font-family": "var(--font-mono)",
    }, fmtSignedUsd(t)));
  }

  const slotW = plotW / exps.length;
  const barW = Math.min(24, Math.max(8, slotW * 0.28));

  exps.forEach((e, idx) => {
    const cx = pad.left + idx * slotW + slotW / 2;

    const drawBar = (val, offsetX, fillColor, seriesName) => {
      if (val === null || val === undefined) return;
      const yy = yScale(val);
      const top = Math.min(yy, yZero);
      const h = Math.max(2, Math.abs(yy - yZero));
      const r = el("rect", {
        x: cx + offsetX, y: top, width: barW, height: h, rx: 3,
        fill: fillColor, style: "cursor:pointer",
      });
      r.addEventListener("mousemove", (evt) => {
        showTooltip(tooltip, stage, evt.clientX, evt.clientY, `
          <div class="tt-title">${escapeHtml(e.month_label)} · ${seriesName}</div>
          <div class="tt-row"><span>Aktuell (P50)</span><strong>${fmtUsd(e.median)}</strong></div>
          <div class="tt-row"><span>Δ 1T</span><strong class="${(e.delta_1d_usd || 0) >= 0 ? "is-pos" : "is-neg"}">${fmtSignedUsd(e.delta_1d_usd)} (${fmtPct(e.delta_1d_pct)})</strong></div>
          <div class="tt-row"><span>Δ 7T</span><strong class="${(e.delta_7d_usd || 0) >= 0 ? "is-pos" : "is-neg"}">${fmtSignedUsd(e.delta_7d_usd)} (${fmtPct(e.delta_7d_pct)})</strong></div>
        `);
      });
      r.addEventListener("mouseleave", () => { tooltip.hidden = true; });
      svg.appendChild(r);
    };

    drawBar(e.delta_1d_usd, -barW - 2, "#38b6ff", "Tagesvergleich (1T)");
    drawBar(e.delta_7d_usd, 2, "#a78bfa", "Wochenvergleich (1W)");

    svg.appendChild(el("text", {
      x: cx, y: height - 10, "text-anchor": "middle", fill: "var(--text-muted)", "font-size": 10.5,
    }, e.short_label));
  });

  // Kompakte Legende oben links
  svg.appendChild(el("rect", { x: pad.left + 4, y: 8, width: 10, height: 10, rx: 2, fill: "#38b6ff" }));
  svg.appendChild(el("text", { x: pad.left + 19, y: 17, fill: "var(--text-secondary)", "font-size": 11 }, "Δ 1T (Gestern)"));
  svg.appendChild(el("rect", { x: pad.left + 125, y: 8, width: 10, height: 10, rx: 2, fill: "#a78bfa" }));
  svg.appendChild(el("text", { x: pad.left + 140, y: 17, fill: "var(--text-secondary)", "font-size": 11 }, "Δ 1W (Vor 7T)"));
}

export function renderStrikeHistoryChart({ svg, points, color }) {
  svg.innerHTML = "";
  const width = Math.max(280, Math.round(svg.clientWidth || 520));
  const height = Math.max(120, Math.round(svg.clientHeight || 145));
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

  if (!points || points.length < 2) {
    svg.appendChild(el("text", { x: width / 2, y: height / 2, "text-anchor": "middle", fill: "var(--text-muted)", "font-size": 12 }, "Zu wenige historische CLOB-Datenpunkte für diesen Strike."));
    return;
  }

  const pad = { top: 12, right: 46, bottom: 22, left: 10 };
  const plotW = width - pad.left - pad.right;
  const plotH = height - pad.top - pad.bottom;

  const tMin = points[0].t;
  const tMax = points[points.length - 1].t;
  const tSpan = Math.max(60, tMax - tMin);

  const pVals = points.map((pt) => pt.p * 100);
  let yMin = Math.max(0, Math.floor(Math.min(...pVals) / 5) * 5 - 5);
  let yMax = Math.min(100, Math.ceil(Math.max(...pVals) / 5) * 5 + 5);
  if (yMax - yMin < 10) { yMin = Math.max(0, yMin - 5); yMax = Math.min(100, yMax + 5); }

  const xScale = (t) => pad.left + ((t - tMin) / tSpan) * plotW;
  const yScale = (pPct) => pad.top + plotH - ((pPct - yMin) / Math.max(1, yMax - yMin)) * plotH;

  for (const frac of [0, 0.5, 1]) {
    const val = yMin + (yMax - yMin) * frac;
    const yy = yScale(val);
    svg.appendChild(el("line", { x1: pad.left, x2: pad.left + plotW, y1: yy, y2: yy, stroke: "var(--chart-grid)", "stroke-width": 1 }));
    svg.appendChild(el("text", { x: pad.left + plotW + 6, y: yy + 3.5, fill: "var(--text-muted)", "font-size": 10, "font-family": "var(--font-mono)" }, `${Math.round(val)} %`));
  }

  const coords = points.map((pt) => `${xScale(pt.t).toFixed(1)},${yScale(pt.p * 100).toFixed(1)}`);
  svg.appendChild(el("path", { d: `M ${coords.join(" L ")}`, fill: "none", stroke: color, "stroke-width": 1.9, "stroke-linejoin": "round" }));

  const fmtShort = (ts) => new Intl.DateTimeFormat("de-DE", { day: "2-digit", month: "short" }).format(new Date(ts * 1000));
  svg.appendChild(el("text", { x: pad.left, y: height - 5, fill: "var(--text-muted)", "font-size": 10 }, fmtShort(tMin)));
  svg.appendChild(el("text", { x: pad.left + plotW, y: height - 5, "text-anchor": "end", fill: "var(--text-muted)", "font-size": 10 }, fmtShort(tMax)));
}
