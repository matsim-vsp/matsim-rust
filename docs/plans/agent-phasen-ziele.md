# Agentenlogik nach Phase und Rolle: architektonische Ziele

Bezug: Issue #203. Ausführliche Umsetzungsnotizen stehen in `agent-phasen-variante-c.md`.

## Problem

`SimulationAgentLogic` ist eine einzige Trait für alles, was die Engines von einem Agenten brauchen. Viele
Methoden gelten aber nur in einem bestimmten Zustand: `curr_act` nur während einer Aktivität, `curr_leg`
nur auf einer Strecke, `peek_next_link_id` nur bei einer Netzwerkroute. Dass man sie nur im richtigen
Zustand aufrufen darf, steht nirgends; durchgesetzt wird es nur durch `unwrap`s. Dasselbe gilt für
`notify_event`, über das auch Zustandsänderungen laufen. Mit jeder neuen Engine wächst die Trait weiter.

## Ziele

1. **Gültigkeit folgt aus dem Typ, nicht aus einer Konvention.** Eine Engine bekommt vom Agenten nur die
   Logik, die in seiner aktuellen Phase gültig ist. Der Zustand wird einmal geprüft, und zwar dort, wo er
   bekannt ist: in der Agentenlogik, nicht implizit in jeder Engine.
2. **Rollen statt einer Sammel-Trait.** Ein Agent ist entweder in einer Aktivität oder auf einer Strecke. Auf
   einer Strecke spielt er je nach Route eine oder mehrere Rollen (teleportiert, Fahrer im Netz, später
   ÖV-Fahrgast). Jede Engine hängt nur von der Rolle ab, die sie braucht. Eine neue Rolle ist eine neue
   Trait und lässt bestehende Implementierungen unberührt.
3. **Zustandsänderungen gehören zur Rolle.** Was den Agenten verändert (über einen Link fahren, eine
   Teleportation beginnen, eine Aktivität beenden), ist nur über die Rolle erreichbar, in der es gültig ist.
   Befehle und reine Beobachtungen werden getrennt.
4. **Die Logik bleibt verborgen und austauschbar.** Plan-basierte und adaptive Logik bleiben hinter Traits.
   Ein Dekorator überschreibt nur die Rolle, die er tatsächlich ändert (die adaptive Logik also nur die
   Aktivität), statt jede Methode durchzureichen.
5. **Das Environment entscheidet über die Engine.** Der Agent sagt, welche Rollen er auf seiner Strecke
   spielen kann; welche Engine die Strecke simuliert, entscheidet weiter die Leg-Engine, wie die
   Departure-Handler in MATSim.
6. **Nur Agenten mit Plan sind `SimulationAgent`s.** Wer keine Aktivitäten hat, etwa ein ÖV-Fahrer, bekommt
   eine eigene, schmale Logik und muss keine Methoden implementieren, die für ihn keinen Sinn ergeben.

## Leitplanken

- Keine Verhaltensänderung: Events, Zeiten und Reihenfolgen bleiben gleich; Determinismus bleibt unberührt.
- Ownership und Transport bleiben: Agenten wandern weiter als `Box<dyn SimulationAgentLogic>` zwischen
  Engines, Fahrzeugen und Partitionen.
- Der Hot Path der Netzwerk-Engine wird nicht messbar langsamer.

## Form in groben Zügen

`SimulationAgentLogic` gibt nur noch die Identität und die aktuelle Phase heraus. Die Phase ist entweder
`Activity` oder `Leg` und trägt die dazu passende Logik; eine Strecke bietet ihre Rollen optional an (z. B.
"Fahrer im Netz" nur bei einer Netzwerkroute). Die Engines fragen nach der Phase bzw. Rolle, die sie
brauchen, und haben damit an einer Stelle die einzige Laufzeitprüfung.

## Bewusst nicht (vorerst)

- **Nur Supertraits** (#203 wörtlich): ordnet die Methoden, löst aber das Zustandsproblem nicht.
- **Typestate mit konsumierenden Übergängen**: stärkste Garantie, aber großer Umbau. Er bleibt später möglich,
  weil die Rollen-Traits dieselben wären; nur die Art, wie man an sie kommt, ändert sich.
- **Enum statt Trait-Objekt**: betrifft nur den Dispatch und würde die Austauschbarkeit einschränken.

## Vorgehen

Schrittweise: zuerst die neuen Traits neben der bestehenden API, dann Engine für Engine umstellen, zuletzt
die alte API entfernen. Nach jedem Schritt laufen die bestehenden Tests unverändert durch.
