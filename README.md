# Konvex Compute

Lokale macOS-Software, die die GPU-Mietpreis-Märkte von Polymarket als Forward-Kurve darstellt, für H100, H200, B200 und B300. Startpunkt der Kurve ist der Ornn Compute Price Index (OCPI), auf den alle diese Polymarket-Märkte abrechnen.

Beispielprojekt aus dem [KI-Lab von Konvex Investment](https://konvexinvestment.de/ki-lab/). Die erste Fassung entstand mit Gemini Neon in der Antigravity CLI, die Überarbeitung mit Claude Code (Opus 5.5) und einem Review durch Gemini Neon. Technik: Tauri 2, Rust und SQLite, ohne Python, Node.js oder npm.

## Funktionen

- **Forward-Kurve mit Bändern:** links die Ornn-Spot-Historie, rechts die Polymarket-Forward-Kurve bis zum fernsten gelisteten Verfall, dazu Interquartilsband (`P25` bis `P75`), äußeres Band (`P10` bis `P90`), getrimmter Kern-Mean, Roh-Mean inklusive Tail und Skew.
- **Kurvenmathematik ([`math.rs`](src-tauri/src/math.rs)):** Gewichtete isotonische Regression (PAVA) glättet Spread-Zacken im Orderbuch. Daraus entstehen eine konsistente Survival Function für Quantile und Kern-Mean sowie die Wahrscheinlichkeits-Buckets (`PDF`) für das Verteilungs-Histogramm.
- **Tages-, Wochen- und Monatsvergleich (1T, 1W, 1M):** Beim ersten Start rekonstruiert die Software bis zu 14 Tage Forward-Kurven aus der Polymarket-CLOB-Historie (`/prices-history`). Danach speichert sie alle 15 Minuten einen eigenen Snapshot in SQLite. Nach 48 Stunden bleibt je GPU und Tag nur der letzte Stand erhalten. Lücken nach einer Pause füllt ein automatischer Backfill, höchstens alle 20 Stunden.
- **Strike-Leiter:** Ein Klick auf einen Strike lädt dessen 30-Tage-Wahrscheinlichkeitsverlauf aus der CLOB-API, in 14-Tage-Abschnitten, weil die API längere Fenster ablehnt.
- **Erklärungen in der Oberfläche:** Gepunktet unterstrichene Fachbegriffe erklären sich beim Überfahren mit der Maus, der Hauptchart hat eine Legende, Abruffehler erscheinen als kurze Hinweise mit aufklappbaren Details.
- **Alle GPUs im Vergleich:** Relative-Value-Matrix über die vier GPU-Generationen (Verhältnis zur H100, Kurvensteigung) und weitere Ornn-Spot-Indizes (`A100 SXM4`, `RTX 5090`, `RTX PRO 6000 WS`).
- **Data Store:** Übersicht über die lokale Datenbank, manueller Backfill und CSV-Export nach `~/Downloads/`.

## Warum der Roh-Mean bei fernen Verfällen über dem Median liegt

Ein GPU-Mietpreis ist nach unten bei `0 $` begrenzt, nach oben offen. Ferne binäre Polymarket-Kontrakte binden zudem lange Kapital, deshalb fallen weit entfernte Strikes kaum unter einen impliziten Zinsboden (Beispielwert: 8 bis 10 Cent). Der volle Erwartungswert (`Roh-Mean`) gewichtet diesen oberen Tail mit der ganzen Dollarbreite und steigt dadurch, während Median (`P50`) und Kern-Mean die zentrale Markterwartung zeigen.

## Voraussetzungen

- macOS 14 oder neuer (getestet mit macOS 27.2)
- Apple Command Line Tools: `xcode-select --install`
- Rust über rustup (getestet mit Rust 1.95):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Node.js, npm, Python und API-Schlüssel sind nicht nötig. Die Software ruft nur öffentliche Endpunkte ohne Anmeldung ab.

## Bauen und installieren

Quellcode per git holen:

```bash
git clone https://github.com/KonvexInvestment/konvex-compute.git
cd konvex-compute
```

Alternativ das ZIP von GitHub („Code“, „Download ZIP“) entpacken und im Terminal in den Ordner `konvex-compute-main` wechseln.

Bauen und nach `/Applications/Konvex Compute.app` installieren:

```bash
bash build_mac_app.sh --install
```

Der erste Build lädt die Rust-Abhängigkeiten und dauert einige Minuten. Ohne `--install` bleibt das Ergebnis unter `src-tauri/target/release/bundle/macos/`. Die Unit-Tests laufen mit:

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

Updates kommen per `git pull`, danach erneut `bash build_mac_app.sh --install`.

## Gatekeeper

Das Build-Skript signiert die Software ad hoc, ohne Apple-Entwicklerzertifikat und ohne Notarisierung. Auf dem Mac, auf dem sie gebaut wurde, startet sie ohne Warnung, weil selbst gebaute Programme kein Quarantäne-Attribut tragen. Wird die fertige `.app` per Download, AirDrop oder Messenger auf einen anderen Mac übertragen, blockiert Gatekeeper sie. Deshalb gibt es kein fertiges Download-Paket, die Software wird auf jedem Mac aus dem Quellcode gebaut.

## Lokale Daten

Alle Daten liegen nur auf dem eigenen Mac:

```text
~/Library/Application Support/de.konvexinvestment.compute/compute.db
```

Zum vollständigen Entfernen genügt es, `/Applications/Konvex Compute.app` und diesen Ordner zu löschen.

## Datenquellen und Bedingungen

Die Software fragt alle Quellen direkt vom eigenen Mac ab, alle 15 Minuten, solange sie läuft. Das Repository enthält nur Code, keine Marktdaten.

- **Ornn Compute Price Index** ([data.ornn.com](https://data.ornn.com)), kostenlose API unter `api.ornnai.com`. Der OCPI ist der Abrechnungsindex aller Polymarket-GPU-Mietpreis-Märkte. Ornns [Nutzungsbedingungen](https://ornn.com/terms-of-use) erlauben die kostenlose API nur für die eigene private oder interne Nutzung und verlangen für das Speichern von Antworten und für abgeleitete Berechnungen eine schriftliche Erlaubnis. Ornn hat Konvex Investment im November 2026 schriftlich bestätigt, dass die Nutzung in dieser Software in Ordnung ist: Abruf vom eigenen Mac, Speicherung nur lokal, Vergleich mit der Forward-Kurve, Ornn als genannte Quelle. Eine Weitergabe der Daten ist davon nicht gedeckt, Datenbank und CSV-Exporte bleiben auf dem eigenen Rechner.
- **Polymarket** (Gamma API und CLOB API, `gamma-api.polymarket.com`, `clob.polymarket.com`): öffentliche Lese-Endpunkte ohne Konto. Es gelten die [Nutzungsbedingungen von Polymarket](https://polymarket.com/tos). Die Software liest nur Kurse und handelt nicht.

## Rahmen

Konvex Compute ist ein Lernprojekt zum Erfahrungsaustausch. Software und Anzeigen sind keine Anlageberatung und keine Empfehlung zum Kauf oder Verkauf. Konvex Investment bietet keinen Einzel-Support und keine Einzelbetreuung für eigene oder fremde Codebasen an.

## Lizenz

Code unter [MIT-Lizenz](LICENSE). Die gebündelte Schriftart Inter Tight steht unter der SIL Open Font License ([`ui/assets/Inter-Tight-LICENSE.txt`](ui/assets/Inter-Tight-LICENSE.txt)).
