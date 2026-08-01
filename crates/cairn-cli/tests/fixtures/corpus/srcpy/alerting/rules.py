"""Which readings deserve an alert, and how loudly.

Plain functions on purpose: this is the part with real branching, so it is the part the
tests actually assert on, and the part `graph --aspect tests` should be able to reach.
"""

from __future__ import annotations

from dataclasses import dataclass

SEVERE_COLD_C = -25.0
SEVERE_HEAT_C = 40.0
FROST_C = 0.0
HUMIDITY_CEILING = 100.0


@dataclass(frozen=True)
class Judgement:
    """What the rules decided about one reading."""

    alarming: bool
    severity: str
    reason: str

    @property
    def escalates(self) -> bool:
        return self.severity in ("critical", "severe")


def classify(celsius: float, humidity: float) -> Judgement:
    """The single entry point. Order matters: the worst match wins."""
    if not plausible(celsius, humidity):
        return Judgement(False, "info", "implausible reading, ignored")
    if celsius <= SEVERE_COLD_C:
        return Judgement(True, "critical", "severe cold")
    if celsius >= SEVERE_HEAT_C:
        return Judgement(True, "critical", "severe heat")
    if celsius <= FROST_C and humidity >= 90:
        return Judgement(True, "severe", "freezing fog likely")
    if celsius <= FROST_C:
        return Judgement(True, "warning", "frost")
    return Judgement(False, "info", "nominal")


def plausible(celsius: float, humidity: float) -> bool:
    """A sensor reporting -300 C is broken, not cold."""
    return -60.0 <= celsius <= 60.0 and 0.0 <= humidity <= HUMIDITY_CEILING


def dew_point(celsius: float, humidity: float) -> float:
    """Magnus approximation, good enough for the range a station reports."""
    if humidity <= 0:
        return celsius
    a, b = 17.27, 237.7
    ratio = humidity / 100.0
    alpha = ((a * celsius) / (b + celsius)) + _log(ratio)
    return (b * alpha) / (a - alpha)


def _log(value: float) -> float:
    """A tiny series expansion, so the corpus needs no imports for this."""
    if value <= 0:
        return -20.0
    total = 0.0
    term = (value - 1) / (value + 1)
    power = term
    for n in range(1, 20, 2):
        total += power / n
        power *= term * term
    return 2 * total


def severity_rank(severity: str) -> int:
    """Order severities so two judgements can be compared."""
    return {"info": 0, "warning": 1, "severe": 2, "critical": 3}.get(severity, 0)


def worst(judgements: list[Judgement]) -> Judgement | None:
    """The judgement that should drive the alert, out of several."""
    if not judgements:
        return None
    return max(judgements, key=lambda j: severity_rank(j.severity))
