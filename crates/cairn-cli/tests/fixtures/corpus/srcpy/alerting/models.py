"""Stored shape of an alert.

Written as a small descriptor-backed record rather than a real ORM so the corpus stays
dependency-free. The shape is what matters: attribute access goes through a descriptor,
which is exactly the case where a reference search under-reports and the tool is supposed
to hand over to grep instead of printing a confident blast radius.
"""

from __future__ import annotations

import time
from typing import Any, ClassVar


class Field:
    """A stored attribute, reached through the descriptor protocol."""

    def __init__(self, default: Any = None) -> None:
        self.default = default
        self.name = ""

    def __set_name__(self, owner: type, name: str) -> None:
        self.name = name

    def __get__(self, obj: Any, objtype: type | None = None) -> Any:
        if obj is None:
            return self
        return obj.__dict__.get(self.name, self.default)

    def __set__(self, obj: Any, value: Any) -> None:
        obj.__dict__[self.name] = value


class Record:
    """Base for anything the worker persists."""

    table: ClassVar[str] = ""

    def __init__(self, **values: Any) -> None:
        for key, value in values.items():
            setattr(self, key, value)

    def as_row(self) -> dict[str, Any]:
        return {k: v for k, v in self.__dict__.items() if not k.startswith("_")}

    def __repr__(self) -> str:
        return f"<{type(self).__name__} {self.as_row()}>"


class Alert(Record):
    """One open or closed alert for one station."""

    table = "alerts"

    station_id = Field("")
    reason = Field("")
    celsius = Field(0.0)
    raised_at = Field(0.0)
    cleared_at = Field(0.0)
    severity = Field("info")

    def is_open(self) -> bool:
        return self.raised_at > 0 and self.cleared_at == 0

    def clear(self) -> None:
        self.cleared_at = time.time()

    def duration_seconds(self) -> float:
        if self.raised_at == 0:
            return 0.0
        end = self.cleared_at or time.time()
        return max(0.0, end - self.raised_at)


class StationState(Record):
    """What the worker remembers between readings."""

    table = "station_state"

    station_id = Field("")
    last_celsius = Field(0.0)
    consecutive_alarming = Field(0)
    muted = Field(False)

    def note_reading(self, celsius: float, alarming: bool) -> None:
        self.last_celsius = celsius
        self.consecutive_alarming = self.consecutive_alarming + 1 if alarming else 0

    def should_escalate(self, threshold: int = 3) -> bool:
        return not self.muted and self.consecutive_alarming >= threshold


class AlertStore:
    """In-memory store, keyed by station."""

    def __init__(self) -> None:
        self._alerts: dict[str, Alert] = {}
        self._state: dict[str, StationState] = {}

    def open_alert(self, station_id: str, reason: str, celsius: float) -> Alert:
        alert = Alert(
            station_id=station_id,
            reason=reason,
            celsius=celsius,
            raised_at=time.time(),
        )
        self._alerts[station_id] = alert
        return alert

    def close_alert(self, station_id: str) -> Alert | None:
        alert = self._alerts.get(station_id)
        if alert is None:
            return None
        alert.clear()
        return alert

    def state_for(self, station_id: str) -> StationState:
        state = self._state.get(station_id)
        if state is None:
            state = StationState(station_id=station_id)
            self._state[station_id] = state
        return state

    def open_alerts(self) -> list[Alert]:
        return [a for a in self._alerts.values() if a.is_open()]
