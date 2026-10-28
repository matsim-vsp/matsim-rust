# Agentenrollen: Traits zusammenführen, Plan-Typen kapseln

Bezug: PR #331, Issue #203. Arbeitsdokument, wird vor dem Merge entfernt.

## Ausgangslage

Der PR trennt die Agentenlogik in Phasen (`Activity`, `Leg`) und Rollen (Netzwerkfahrer, Teleportation,
Passagier). Die Review hat vier strukturelle Probleme gezeigt:

- Jede Phase und Rolle ist doppelt modelliert (`*Logic` + `*Commands`). Das bringt keine Sicherheit, weil
  `&dyn Trait` ohnehin nur `&self`-Methoden erlaubt.
- Die Traits geben Plan-Typen heraus (`InternalActivity`, `InternalLeg`, `InternalRoute`). Engines lesen
  daraus Felder und entscheiden anhand des Routentyps. Damit hängt die Simulation am Planmodell statt an der
  Rolle.
- Default-Methoden und gleichnamige Methoden in mehreren Rollen (`route`, `mode`, `curr_link_id`,
  `leave_link`) machen Dekoratoren fehleranfällig und erzwingen UFCS.
- Der Vertrag „der Fahrer übernimmt den gepeekten Link“ steht nur im Kommentar und wird nur per
  `debug_assert` geprüft.

## Ziele

1. Ein Trait pro Phase bzw. Rolle mit `&self`-Beobachtungen und `&mut self`-Kommandos.
2. Keine Plan-Typen in Trait-Signaturen: weder `InternalActivity` noch `InternalLeg` noch `InternalRoute`.
   Die Traits liefern stattdessen genau die Werte, die die Engines brauchen.
3. Keine Default-Methoden auf Phasen- und Rollen-Traits. Jede Implementierung, auch jeder Dekorator,
   entscheidet explizit.
4. Jede Information hat genau eine Quelle: kein Methodenname kommt in zwei Rollen vor.
5. Der Netzwerkfahrer folgt MATSims `DriverAgent`: `chooseNextLinkId` → `peek_next_link_id`,
   `notifyMoveOverNode(linkId)` → `move_over_node(next, now)`, `isWantingToArriveOnCurrentLink`,
   `getPlannedVehicleId` → `planned_vehicle_id`.
6. Die spekulative Passagier-Rolle entfällt.

## Leitplanken

- Events, Zeiten und Reihenfolgen bleiben gleich. `agent_phase_refactor_preserves_ordered_events` und die
  Three-Links-Vergleiche laufen unverändert. Der Determinismus bleibt unberührt.
- Ownership bleibt: `Box<dyn SimulationAgentLogic>` wandert zwischen Engines, Fahrzeugen und Partitionen.
  Kein Typestate, kein Enum-Dispatch.
- Die Namen `*Logic` bleiben. `Activity`/`Leg` als Trait-Namen würden mit `ResponsibleEngine::Leg` und
  Domänentypen kollidieren.
- Der Netzwerk-Hot-Path wird gegen `main` gemessen, nicht nur gegen den vorherigen PR-Stand.

## Ziel-API

```rust
pub trait SimulationAgentLogic: Identifiable<InternalPerson> + Send {
    fn phase(&self) -> AgentPhase<'_>;
    fn phase_mut(&mut self) -> AgentPhaseMut<'_>;
    fn into_person(self: Box<Self>) -> InternalPerson;
}

pub trait ActivityLogic {
    fn act_type(&self) -> &Id<String>;
    fn link_id(&self) -> &Id<Link>;
    fn coord(&self) -> &Coordinate;
    fn has_next_leg(&self) -> bool;
    /// `activity_start` is the time when the activity engine received the agent.
    fn end_time(&self, activity_start: SimTime) -> SimTime;
    fn wakeup_time(&self, now: SimTime) -> SimTime;

    fn on_started(&mut self, comp_env: &mut ThreadLocalComputationalEnvironment, now: SimTime);
    fn on_wakeup(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        end_time: SimTime,
        now: SimTime,
    );
    fn on_finished(&mut self, now: SimTime);
    fn start_next_leg(&mut self, now: SimTime);
}

pub trait LegLogic {
    fn mode(&self) -> &Id<String>;
    fn routing_mode(&self) -> Option<&Id<String>>;
    fn departure_link(&self) -> &Id<Link>;
    /// The person's position. For a network driver this is the vehicle's link.
    fn curr_link_id(&self) -> Option<&Id<Link>>;

    fn network_driver(&self) -> Option<&dyn NetworkDriverLogic>;
    fn network_driver_mut(&mut self) -> Option<&mut dyn NetworkDriverLogic>;
    fn teleportation(&self) -> Option<&dyn TeleportationLogic>;
    fn teleportation_mut(&mut self) -> Option<&mut dyn TeleportationLogic>;

    fn start_next_activity(&mut self, now: SimTime);
}

pub trait NetworkDriverLogic {
    fn planned_vehicle_id(&self) -> Option<&Id<InternalVehicle>>;
    fn peek_next_link_id(&self) -> Option<&Id<Link>>;
    fn is_wanting_to_arrive_on_current_link(&self) -> bool;
    /// Called after the network has moved the vehicle onto `next_link`.
    fn move_over_node(&mut self, next_link: &Id<Link>, now: SimTime);
}

pub trait TeleportationLogic {
    fn destination_link(&self) -> &Id<Link>;
    fn arrival_time(&self, departure: SimTime) -> SimTime;
    fn distance(&self) -> Option<f64>;
    fn transit(&self) -> Option<TransitTeleportation<'_>>;
    fn begin_teleportation(&mut self, now: SimTime);
}

/// The data a PT teleportation arrival event needs, without exposing the route type.
pub struct TransitTeleportation<'a> {
    pub line: &'a str,
    pub route: &'a str,
    pub boarding_time: Option<SimTime>,
    pub access_facility: &'a str,
    pub egress_facility: &'a str,
}
```

Folgen für die bestehenden Typen:

- `ActivityMut`: behält `on_started`, `on_wakeup`, `on_finished` und das konsumierende `start_next_leg(self)`.
  `logic()` entfällt.
- `LegMut`: behält `into_network_driver(self)`, `into_teleportation(self)` und `start_next_activity(self)`.
  `logic()`, `network_driver(&mut self)` und `teleportation(&mut self)` entfallen.
- `AgentPhase::expect_activity`/`expect_leg` werden von `SimulationAgent::activity()`/`leg()` genutzt, statt
  den Match dort zu duplizieren.
- `SimulationAgent::phase()` wird `pub`. `vehicle_passenger_mut()` entfällt.
- `EndTime` bleibt für Queue-Einträge (`TimeQueue`, `TeleportedAgent`, `AwakeSimulationAgent`), ist aber kein
  Supertrait einer Rolle mehr. Aktivitätsende und Ankunftszeit sind getrennte Methoden mit eigener Bedeutung.
- `PlanLogic` behält private Hilfsmethoden (`curr_act`, `curr_leg`, `route`, `next_leg`), die intern
  Plan-Typen nutzen dürfen. `AdaptiveLogic` arbeitet weiter direkt auf ihrem Delegate.
- `VehiclePassengerCommands` entfällt. `SimulationVehicle::leave_link(now)` wird zu
  `move_over_node(next, now)` und informiert nur den Fahrer.

Bewusst ohne Duplikat: `NetworkDriverLogic` hat weder `mode` noch `curr_link_id`. Beides kommt aus
`LegLogic`, das der Fahrer als Plan-Agent immer hat. Das betrifft die Events in `link.rs` und das
Partition-Enter-Event, die künftig `driver().leg().mode()` lesen. Ein Fahrer ohne Plan bräuchte später
beides in seiner Rolle; das wird entschieden, wenn es ihn gibt.

## Umstellung der Engines

| Stelle | heute | künftig |
| --- | --- | --- |
| `ActivityEngine` Start-/End-Events | `curr_act().link_id()/act_type/coord()` | `link_id()`, `act_type()`, `coord()` |
| `Simulation::emit_stuck_events` | `next_leg().is_none()`, `curr_leg().mode` | `has_next_leg()`, `mode()` |
| `LegEngine::receive_agent` | `route.start_link()`, `leg.mode`, `leg.routing_mode` | `departure_link()`, `mode()`, `routing_mode()` |
| `LegEngine::find_responsible_engine` | Main-Mode + `InternalRoute::Network` | Main-Mode + `network_driver().is_some()` |
| `VehicularDepartureHandler` | `route.as_generic().vehicle()`, Assert auf Netzwerkroute | `network_driver()` + `planned_vehicle_id()` |
| Arrival-Events (Fahrer/Teleport) | `curr_leg().mode`, `curr_link_id()` | `mode()`, `curr_link_id()` |
| Arrival-Events (Passagiere) | eigener Routen-Cursor | Link des Fahrzeugs |
| `TeleportationEngine::do_step` | Match auf Routentyp | `transit()` ist `Some` oder `None` |
| Teleport-Events | `route.as_generic().distance()`, `as_pt().description` | `distance()`, `transit()` |
| `TeleportationEngine::receive_agent` | `end_time(now)` | `arrival_time(now)` |
| `Simulation::is_local_route` | `route.end_link()` | `destination_link()` |
| `SimNetworkPartition::move_vehicle` | `leave_link(now)` + `debug_assert` im Netz | `move_over_node(&next_link, now)` |

`PlanLogic::move_over_node` prüft per `debug_assert_eq!(self.peek_next_link_id(), Some(next_link))` und
erhöht den Cursor. Das Netz nutzt den Link, den es selbst geprüft hat. Der Vertrag steht damit in der
Signatur, und der Post-Commit-Assert im Netz entfällt.

## Schritte

Nach jedem Schritt laufen `cargo test --workspace -- --test-threads=1 --skip berlin::` und `cargo fmt --all`.

0. **Baseline messen.** `agent_network_benchmark` aus `fa8a5515` als temporären, ignorierten Test
   zurückholen. Er läuft auf `main` und auf dem aktuellen Stand, weil alle Hilfsfunktionen dort existieren.
   Gemessen werden `main` (7fc31259), der aktuelle Branch-Stand und später der Endstand, gepaart und mit
   100 Samples. Der Test darf zwischendurch committet werden und wird vor dem Merge entfernt.
1. **Traits zusammenführen.** `*Commands` in `*Logic` aufgehen lassen, Defaults entfernen, Wrapper und
   Projektionen aufräumen, `phase()` öffentlich machen. Die Signaturen mit Plan-Typen bleiben in diesem
   Schritt noch, damit er ein reines Umbenennen und Zusammenlegen ist. `AdaptiveLogic` reicht `on_started`
   explizit weiter.
2. **`ActivityLogic` ohne `InternalActivity`.** Neue Methoden, Activity-Engine und Stuck-Events umstellen,
   `curr_act`/`next_act`/`next_leg` aus dem Trait entfernen. Tests, die den ersetzten Plan prüfen, lesen
   ihn über `into_person()`.
3. **`LegLogic`/`TeleportationLogic` ohne `InternalLeg`/`InternalRoute`.** Leg- und Teleportation-Engine,
   Departure-Handler und `is_local_route` umstellen, `TransitTeleportation` einführen.
4. **Netzwerkfahrer.** `move_over_node`, `planned_vehicle_id`, `mode`/`curr_link_id` aus der Fahrerrolle
   entfernen, Passagier-Rolle löschen, Passagier-Arrival auf den Fahrzeug-Link umstellen.
5. **Messen.** Endstand gegen `main` und gegen Schritt 0. Liegt der Median spürbar über `main`, wird als
   gezielter Fast-Path eine bereitgestellte Methode `SimulationAgentLogic::network_driver()` ergänzt, die
   die Plan-Logiken direkt beantworten (ein dyn-Aufruf weniger). Ohne Messbedarf bleibt sie weg.
6. **Abschluss.** CI-äquivalenter Lauf (`RUSTFLAGS="-D warnings"`, Release, `--features http`), Benchmark
   und dieses Dokument entfernen, PR-Beschreibung aktualisieren.

## Tests

- Bestehende Agent-Tests in `agent_logic.rs` auf die neue API umstellen: Phasenwechsel, Routenpositionen
  inklusive Rundkurs und Ein-Link-Route, PT nur mit Teleportation, Fehlermeldungen bei falscher Phase oder
  fehlender Rolle.
- Neu: Lese- und Schreibzugriff einer Rolle sind für Generic-, Netzwerk- und PT-Routen gleich verfügbar.
- Neu: `transit()` liefert die PT-Beschreibung, `distance()` und `arrival_time()` liefern die
  Routenwerte.
- `CommittingDriver` in `sim_network.rs`: Statt Getter-Aufrufe zu zählen, prüft der Test, dass
  `move_over_node` den geprüften Link erhält.
- `CountingTeleportationLogic` implementiert alle Methoden explizit.
- Vorschlag: Mit `agent_roles.rs` ist die Abdeckung für mehrere Pläne weggefallen (nicht ausgewählter Plan,
  Score, unfertige Route bleiben bei `into_person` erhalten). `selected_plan` als Index ist neu in diesem PR;
  der Fall sollte als Unit-Test in `agent_logic.rs` zurückkommen.

## Verhaltensänderungen

- `is_local_route` nutzt `destination_link()`. Das ändert nur etwas, wenn bei einer Netzwerkroute
  `end_link` vom letzten Link des Vektors abweicht. Bisher widersprachen sich in diesem Fall Lokalitätsprüfung
  und Zielpartition.
- Passagiere bewegen ihren eigenen Routen-Cursor nicht mehr, und ihr Arrival-Link ist der des Fahrzeugs. Wie
  in MATSim bleibt die Position eines Passagiers bis zum Aussteigen unverändert. In Produktion gibt es heute
  keine Passagiere (`unpark_veh` übergibt `vec![]`).
- Öffentliche Rust-API: `*Commands`-Traits, `next_act`, `curr_act`, `curr_leg`, `route`,
  `VehiclePassengerCommands` und `SimulationVehicle::leave_link` entfallen oder werden ersetzt.
  Eingabeformate und Konfiguration bleiben unverändert.

## Bewusst nicht in diesem Schritt

- **Typestate oder typisierte Engine-Queues.** Engines prüfen Phase und Rolle weiter zur Laufzeit.
- **`end_time` aus `on_wakeup` entfernen.** Der Kommentar dazu bleibt; das wäre eine eigene Änderung.
- **Teleportations-Position wie in MATSim.** Dort wird die Position erst bei der Ankunft gesetzt
  (`notifyArrivalOnLinkByNonNetworkMode`); hier geschieht das weiterhin schon bei `begin_teleportation`.
- **Fahrer ohne Plan (z. B. ÖV-Fahrer).** `SimulationVehicle.driver` bleibt ein `SimulationAgent`.

## Offene Entscheidungen

1. `InternalRoute` wird ebenfalls verborgen. Das folgt aus Ziel 2, geht aber über die Nennung von
   `InternalLeg`/`InternalActivity` hinaus.
2. `NetworkDriverLogic` ohne `mode`/`curr_link_id`, also genau eine Quelle in `LegLogic`, statt einer in sich
   geschlossenen Fahrerrolle.
3. Die Schwelle für den Fast-Path in Schritt 5, z. B. Median mehr als 2 % über `main`.
4. Den Mehr-Plan-Materialisierungstest in `agent_logic.rs` wieder aufnehmen.

## Ergebnis

Die Schritte 1 bis 4 sind in einem Durchgang umgesetzt und danach gemeinsam getestet worden, nicht einzeln
nacheinander. Offene Entscheidung 1 bis 4 sind wie vorgeschlagen umgesetzt.

Messung mit `agent_network_benchmark` im Release-Modus, 30 Runden mit je einem Prozess pro Variante und
5 Samples pro Prozess. Verglichen wird der Median je Runde mit `main` (7fc31259):

| Variante | Median | gegenüber `main` (Median, IQR) |
| --- | --- | --- |
| `main` | 4,78 ms | – |
| Branch vor dem Umbau (6799154b) | 5,00 ms | +4,3 % (+2,7 % bis +6,5 %) |
| Umbau ohne Fast-Path | 5,12 ms | +6,9 % (+5,3 % bis +8,9 %) |
| Umbau mit `SimulationAgentLogic::network_driver()` | 4,90 ms | +2,3 % (+0,4 % bis +4,3 %) |

Läufe mit wenigen langen Prozessen streuten um mehrere Prozentpunkte zwischen Wiederholungen. Belastbar
sind deshalb nur die Runden-Vergleiche oben.

Danach wurden auf Wunsch alle `#[inline]` (auf `main` gibt es keine) und der Shortcut
`SimulationAgentLogic::network_driver()` entfernt. Messung in einer eigenen Sitzung, gleiche Methode:

| Variante | gegenüber `main` (Median, IQR) |
| --- | --- |
| ohne `#[inline]`, mit Shortcut | +5,9 % (+4,2 % bis +7,5 %) |
| ohne `#[inline]`, ohne Shortcut (aktueller Stand) | +9,6 % (+8,3 % bis +12,2 %) |

Im direkten Vergleich kosteten die `#[inline]` allein 2,3 % (+0,1 % bis +4,5 %, 40 Runden).

Pro Linkwechsel ruft das Netz den Fahrer dreimal auf: `is_wanting_to_arrive_on_current_link` in
`LocalLink`, `peek_next_link_id` in `evaluate_front_vehicle` und `move_over_node`. Ohne Shortcut kostet jeder
lesende Zugriff drei dyn-Aufrufe (`phase()`, `LegLogic::network_driver()`, Rollenmethode), der schreibende
ebenfalls drei (`phase_mut()`, `network_driver_mut()`, `move_over_node`). Das sind neun pro Linkwechsel; auf
`main` waren es vier (`is_wanting…`, `peek…`, `notify_event`, `curr_link_id`). Der Shortcut sparte den
`phase()`-Aufruf bei den beiden lesenden Zugriffen.
