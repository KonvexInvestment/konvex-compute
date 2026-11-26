# Konvex Compute

Schlanke native macOS-Desktop-App (**Tauri 2 + Rust + SQLite**) zum privaten Tracken von GPU-Mietpreis-Forward-Kurven (**H100, H200, B200, B300**) aus Polymarket-Märkten und dem Ornn Compute Price Index (OCPI).

Entstanden als Projekt für das **Konvex KI-Lab**: 100 % lokales Rust-Backend ohne Python-Sidecar und ohne npm-Bundler, gebündeltes SQLite im WAL-Modus und ein leichtgewichtiges ES-Modul-Frontend mit interaktiven SVG-Charts.

## Funktionen

- **Forward-Kurve & Bänder:** Historische Ornn-Spot-Kurve links, Polymarket-Forward-Kurve bis Dez. 2028 rechts, Interquartilsband (`P25 bis P75`), äußeres Band (`P10 bis P90`), getrimmter **Kern-Mean** (mit ausgewiesenem Band) sowie **Roh-Mean (inkl. Tail)** und Skew-Kennzahl.
- **PAVA-Kurvenmathematik ([`math.rs`](src-tauri/src/math.rs)):** Gewichtete isotonische Regression gegen Spread-Zacken im Orderbuch, konsistente Überlebensfunktion für Quantile und Kern-Erwartungswert sowie vollständige Wahrscheinlichkeits-Buckets (`PDF`) für das Verteilungs-Histogramm.
- **Tages- & Wochenvergleiche (1T / 1W / 1M):** Rekonstruiert beim ersten Start bis zu 14 Tage historische Forward-Kurven aus der Polymarket-CLOB-Historie (`/prices-history`) und speichert ab Start alle 15 Minuten eigene Live-Snapshots lokal in SQLite. Eigene Ansicht zur Kurvenverschiebung (`Δ 1T` vs. `Δ 1W`).
- **Strike-Leiter mit Live-Verlauf:** Klick auf einen beliebigen Strike lädt den 30-Tage-Wahrscheinlichkeitsverlauf direkt aus der Polymarket-CLOB-API.
- **Cross-GPU-Matrix & Data Store:** Relative-Value-Überblick über alle vier GPU-Generationen (inkl. H100-Verhältnis, Kurvensteigung und zusätzlicher Ornn-Spot-Indizes wie `A100 SXM4`, `RTX 5090`, `RTX PRO 6000 WS`) sowie CSV-Export in `~/Downloads/`.
- **Dunkel- & Hell-Modus:** Basiert auf dem Design-System von Konvex Analytics.

## Warum der Roh-Mean bei langen Laufzeiten über dem Median liegt

Bei fernen Verfällen (z. B. Dezember 2028) ist ein GPU-Preis nach unten bei `0 $` begrenzt, nach oben aber offen. Zusätzlich binden weit entfernte binäre Polymarket-Kontrakte Kapital über rund zwei Jahre, wodurch ferne Strikes kaum unter den impliziten Zinsboden (oft 8 bis 10 Cent) fallen. Der volle Integral-Erwartungswert (`Roh-Mean`) gewichtet diesen oberen Tail mit voller Dollarbreite und wandert dadurch nach oben, während **Median (P50)** und **Kern-Mean** die zentrale Markterwartung abbilden.

## Voraussetzungen & Build (macOS)

Benötigt werden Rust (`cargo`, `rustc`) und die Apple Command Line Tools (`xcode-select --install`). Node.js oder npm sind für den Build nicht erforderlich.

```bash
# Unit-Tests ausführen
cargo test --locked --manifest-path src-tauri/Cargo.toml

# Release-App bauen
./build_mac_app.sh

# Release-App bauen und sicher nach /Applications/Konvex Compute.app installieren
./build_mac_app.sh --install
```

Die lokale SQLite-Datenbank liegt unter:

```text
~/Library/Application Support/de.konvexinvestment.compute/compute.db
```

## Datenquellen & Nutzungshinweis

Die App ruft ausschließlich öffentliche Lese-Endpunkte direkt lokal vom eigenen Rechner ab:
- **Polymarket Gamma & CLOB API** (`gamma-api.polymarket.com`, `clob.polymarket.com`)
- **Ornn Compute Price Index Free API** (`api.ornnai.com`)

Die kostenlose Ornn-API ist laut Ornn-Nutzungsbedingungen für die eigene private bzw. interne Nutzung bestimmt und schließt eine kommerzielle Weiterverteilung an Dritte aus. Diese App ist daher ausschließlich als lokales Werkzeug für die eigene Nutzung gedacht.

## Lizenz

Code unter [MIT-Lizenz](LICENSE). Die gebündelte Schriftart *Inter Tight* steht unter der SIL Open Font License ([`ui/assets/Inter-Tight-LICENSE.txt`](ui/assets/Inter-Tight-LICENSE.txt)).
