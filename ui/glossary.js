/**
 * Begriffserklärungen (Hover/Fokus auf Elemente mit data-term) und
 * verständliche Übersetzung technischer Abruf-Fehlermeldungen.
 */

export const GLOSSARY = {
  spot: "Ornn Compute Price Index: aktueller durchschnittlicher Mietpreis für eine GPU-Stunde. Startpunkt der Kurve bei „Heute“.",
  forward: "Forward-Kurve: Was der Prognosemarkt Polymarket für den GPU-Mietpreis in den kommenden Monaten erwartet. Jeder Punkt ist der Median eines Liefermonats.",
  median: "Median (P50): Der Preis, bei dem der Markt es für gleich wahrscheinlich hält, dass der Mietpreis darüber oder darunter liegt. Die Hauptlinie der Forward-Kurve.",
  iqr: "P25 bis P75: die mittlere Hälfte der Markterwartung. Mit 50 % Wahrscheinlichkeit liegt der Preis in dieser Spanne. Je breiter, desto unsicherer ist der Markt.",
  outer: "P10 bis P90: Mit 80 % Wahrscheinlichkeit liegt der Preis in dieser Spanne.",
  meanTrimmed: "Kern-Mean: durchschnittlich erwarteter Preis, gerechnet nur über den mittleren Bereich der Verteilung (mindestens P5 bis P95, bei dünner Datenlage enger). Extreme Außenseiter-Wetten verzerren ihn nicht.",
  meanRaw: "Roh-Mean: Durchschnitt über die gesamte Verteilung inklusive sehr hoher Preise. Liegt bei langen Laufzeiten oft deutlich über dem Median, weil weit entfernte Wetten nie ganz auf null fallen.",
  skew: "Tail-Skew: Abstand zwischen Roh-Mean und Median. Ein großer positiver Wert heißt: Der Markt hält deutlich höhere Preise eher für möglich als deutlich niedrigere.",
  slope: "Slope: Um wie viel Prozent der Median des letzten Liefermonats über (+) oder unter (−) dem heutigen Spotpreis liegt. Negativ heißt: Der Markt erwartet fallende Mietpreise.",
  vsSpot: "Abstand des Forward-Medians zum heutigen Spotpreis in Prozent.",
  front: "Front: der nächstgelegene Liefermonat.",
  marketDepth: "Markttiefe: Kapital, das auf Polymarket über alle Liefermonate dieser GPU im Orderbuch bereitsteht. Mehr Liquidität bedeutet verlässlichere Preise.",
  liquidity: "Liquidität: Kapital, das im Orderbuch für diese Wette bereitsteht. Mehr Liquidität bedeutet verlässlichere Preise.",
  volume24: "Handelsvolumen der letzten 24 Stunden.",
  strikeLadder: "Strike-Leiter: alle Ja/Nein-Wetten eines Liefermonats, aufsteigend nach Preisschwelle. Aus ihnen berechnet die App Median, Bänder und Verteilung.",
  strike: "Strike: Preisschwelle einer Ja/Nein-Wette, zum Beispiel „Mietpreis im Liefermonat über 3,00 $ pro GPU-Stunde“.",
  probYes: "P(Ja): Marktpreis der Ja-Wette, also die Wahrscheinlichkeit, dass der Mietpreis über diesem Strike liegt.",
  pava: "Geglättet (PAVA): Höhere Preisschwellen müssen immer unwahrscheinlicher sein. Wo Marktpreise das wegen breiter Spreads verletzen, glättet die App sie. Alle Kennzahlen nutzen diese geglätteten Werte.",
  bidAsk: "Bid / Ask: bestes Kauf- und Verkaufsgebot für die Ja-Wette, in Cent je Anteil.",
  spread: "Spread: Abstand zwischen Bid und Ask. Ein großer Spread bedeutet wenig Handel und einen unsichereren Preis.",
  chg24: "Veränderung der Ja-Wahrscheinlichkeit in den letzten 24 Stunden, in Prozentpunkten (Pp).",
  distribution: "Implizite Verteilung: wie wahrscheinlich der Markt die einzelnen Preisspannen im Liefermonat hält. Die Fläche eines Balkens entspricht seiner Wahrscheinlichkeit.",
  compareCurve: "Kurvenvergleich: legt eine frühere Forward-Kurve (gestern, vor einer Woche, vor einem Monat oder einen gespeicherten Snapshot) gestrichelt über die aktuelle.",
  delta1d: "Δ 1T: Veränderung des Medians gegenüber dem letzten Stand von gestern.",
  delta7d: "Δ 7T: Veränderung des Medians gegenüber dem Stand von vor etwa einer Woche.",
  curveShift: "Ø Kurven-Shift: durchschnittliche Veränderung der Mediane über alle Liefermonate. Positiv heißt: Die ganze Kurve ist gestiegen.",
  vsH100: "Spotpreis im Verhältnis zur H100. 2,00x heißt: doppelt so teuer pro Stunde.",
  backfill: "Backfill: rekonstruiert vergangene Tageskurven aus dem Polymarket-Kursverlauf, damit Tages- und Wochenvergleiche ohne wochenlanges Sammeln funktionieren.",
  snapshot: "Snapshot: gespeicherter Stand der kompletten Forward-Kurve. Live-Snapshots entstehen alle 15 Minuten, nach 48 Stunden bleibt je Tag der letzte.",
  spotDays: "Anzahl gespeicherter Ornn-Tagesschlusskurse.",
  regime: "Finanzierungsregime: verbindet Kreditaufschläge mit der GPU-Forward-Kurve. Ein neues Regime gilt erst nach drei Tagen in Folge, eine Übertragung ohne Bruchsignal erst nach sieben.",
  fsi: "Stressindex: Mittel aus Makroblock (robuste z-Werte der Hochzins-, CCC- und BBB-Aufschläge über drei Jahre) und KI-Block (Abstand zwischen Marktniveau und Agenturrating der Emittenten). Stress beginnt ab 1,0 und endet erst unter 0,5.",
  macroBlock: "Makroblock: Wie weit die Kreditaufschläge des gesamten US-Unternehmensanleihemarkts über ihrem üblichen Niveau der letzten drei Jahre liegen, in robusten Standardabweichungen.",
  aiBlock: "KI-Block: durchschnittlicher Abstand zwischen der Ratingstufe, die der Anleihemarkt den Emittenten einpreist, und ihrem Agenturrating. 1 Stufe entspricht etwa drei Notches.",
  gSpread: "Spread: Rendite der Anleihen aus dem Geldkurs minus Rendite der US-Staatsanleihe gleicher Restlaufzeit, in Basispunkten. Bei mehreren Anleihen verkettet, damit Wechsel im Korb keine Sprünge erzeugen.",
  impliedRating: "Marktniveau: die Ratingstufe, deren durchschnittlicher Kreditaufschlag (ICE BofA OAS) dem Spread des Emittenten entspricht.",
  ratingGap: "Abstand: Marktniveau minus Agenturrating in Stufen. Positiv heißt: Der Anleihemarkt bewertet den Emittenten schlechter als die Ratingagentur.",
  oas: "OAS: optionsbereinigter Kreditaufschlag eines ICE-BofA-Anleiheindex über US-Staatsanleihen, in Prozentpunkten.",
  zScore: "z-Wert: Abstand zum Median der letzten drei Jahre in robusten Standardabweichungen (Median der absoluten Abweichungen), auf ±4 begrenzt.",
  coupling: "Kopplung: Regression der 7-Tage-Veränderung des nahen Kurvenmedians auf die 7-Tage-Veränderung des Stressindex eine Woche zuvor, über 90 Tage. Negativ heißt: Steigender Stress ging fallenden Mietpreisen voraus.",
  breakProb: "Bruchwahrscheinlichkeit: Wahrscheinlichkeit nach Adams und MacKay (2007), dass die täglichen Veränderungen in den letzten sieben Tagen ein neues Muster begonnen haben. Erst ab 30 Beobachtungen aussagekräftig.",
  nearCurve: "Nahe Kurve: Median und unteres Quartil bei fester Laufzeit von 90 Tagen, linear zwischen den Liefermonaten. So entsteht kein Sprung, wenn ein Liefermonat ausläuft.",
  basis: "Basis: Spot relativ zum nahen Kurvenmedian, in Prozent. Sinkt die Basis, fällt der Spot stärker als die Kurve.",
};

let tipEl = null;
let activeTarget = null;

function positionTip(target) {
  const rect = target.getBoundingClientRect();
  const tw = tipEl.offsetWidth;
  const th = tipEl.offsetHeight;
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  let x = rect.left + rect.width / 2 - tw / 2;
  let y = rect.bottom + 8;
  if (y + th > vh - 8) y = rect.top - th - 8;
  x = Math.max(8, Math.min(vw - tw - 8, x));
  y = Math.max(8, y);
  tipEl.style.left = `${x}px`;
  tipEl.style.top = `${y}px`;
}

function show(target) {
  const text = GLOSSARY[target.dataset.term];
  if (!text) return;
  activeTarget = target;
  tipEl.textContent = text;
  tipEl.hidden = false;
  positionTip(target);
}

function hide() {
  activeTarget = null;
  if (tipEl) tipEl.hidden = true;
}

/** Ergänzt Screenreader-Beschreibungen für neu gerenderte Begriffe. */
export function annotateTerms(root = document) {
  for (const node of root.querySelectorAll("[data-term]:not([aria-description])")) {
    const text = GLOSSARY[node.dataset.term];
    if (text) node.setAttribute("aria-description", text);
  }
}

/** Ein einziges, fixiertes Tooltip-Element: wird nie von scrollenden Tabellen abgeschnitten. */
export function installTermTooltips() {
  tipEl = document.createElement("div");
  tipEl.className = "term-tip";
  tipEl.hidden = true;
  tipEl.setAttribute("role", "tooltip");
  document.body.appendChild(tipEl);

  document.addEventListener("mouseover", (evt) => {
    const target = evt.target instanceof Element ? evt.target.closest("[data-term]") : null;
    if (target && target !== activeTarget) show(target);
    else if (!target && activeTarget) hide();
  });
  document.addEventListener("focusin", (evt) => {
    const target = evt.target instanceof Element ? evt.target.closest("[data-term]") : null;
    if (target) show(target);
  });
  document.addEventListener("mouseout", (evt) => { if (!evt.relatedTarget) hide(); });
  document.addEventListener("focusout", hide);
  document.addEventListener("scroll", hide, true);
  document.addEventListener("keydown", (evt) => { if (evt.key === "Escape") hide(); });
}

/* ---------- Verständliche Fehlermeldungen ---------- */

function describeSource(raw) {
  if (/gamma-api\.polymarket|Polymarket Gamma/i.test(raw)) return "Polymarket-Kurven";
  if (/clob\.polymarket|CLOB/i.test(raw)) return "Polymarket-Kursverlauf";
  const url = raw.match(/api\.ornnai\.com\/api\/gpu\/([^/?\s:]+)(\/index-history)?/);
  if (url) {
    let name = url[1];
    try { name = decodeURIComponent(name); } catch { /* Rohname behalten */ }
    return `${url[2] ? "Ornn-Historie" : "Ornn-Spot"} ${name}`;
  }
  const own = raw.match(/Ornn (Spot|Historie) \(([^)]+)\)/);
  if (own) return `Ornn-${own[1]} ${own[2]}`;
  if (/ornn/i.test(raw)) return "Ornn-Index";
  if (/^FRED\b|fred\.stlouisfed/i.test(raw)) return "FRED-Kreditaufschläge";
  if (/Treasury\.gov|home\.treasury\.gov/i.test(raw)) return "Treasury-Zinskurve";
  if (/^IB\b/.test(raw)) return "Interactive Brokers";
  return null;
}

function describeProblem(raw) {
  if (/Netzwerkfehler/i.test(raw)) return "nicht erreichbar, Internetverbindung prüfen";
  if (/HTTP 429/.test(raw)) return "Abruflimit erreicht, nächster Versuch läuft automatisch";
  if (/HTTP 5\d\d/.test(raw)) return "Dienst vorübergehend gestört";
  if (/HTTP 4\d\d/.test(raw)) return "Anfrage abgelehnt";
  if (/JSON|Feld|ungültig|kein Event-Array|ohne history/i.test(raw)) return "unerwartete Antwort";
  return null;
}

/** Wandelt eine technische Backend-Meldung in einen kurzen deutschen Hinweis um. */
export function friendlyError(raw) {
  const text = String(raw ?? "").replace(/^Error:\s*/, "");
  const src = describeSource(text);
  const prob = describeProblem(text);
  if (src && prob) return `${src}: ${prob}`;
  if (src) return `${src}: Abruf fehlgeschlagen`;
  if (prob) return prob.charAt(0).toUpperCase() + prob.slice(1);
  return text;
}
