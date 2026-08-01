"""The AlertService implementation — the Python side of the boundary.

The Go collector calls this over gRPC. Nothing here names the collector, and nothing in
the collector names this class; the only thing joining them is the pair of generated
artefacts, which is exactly the edge `reaches` exists to report.
"""

from __future__ import annotations

from alerting import dispatch
from alerting.models import Alert, AlertStore
from alerting.rules import classify
from schema.telemetry import AlertAck, AlertRequest, AlertServiceBase


class AlertService(AlertServiceBase):
    """Serves RaiseAlert and ClearAlert."""

    def __init__(self, store: AlertStore | None = None) -> None:
        self.store = store or AlertStore()
        self.raised = 0
        self.cleared = 0

    async def raise_alert(self, request: AlertRequest) -> AlertAck:
        judgement = classify(request.celsius, 50.0)
        state = self.store.state_for(request.station_id)
        state.note_reading(request.celsius, judgement.alarming)

        if not judgement.alarming:
            return AlertAck(raised=False, note="not alarming on this side")

        alert = self.store.open_alert(request.station_id, judgement.reason, request.celsius)
        note = dispatch.dispatch(alert, self.store)
        self.raised += 1
        return AlertAck(raised=True, note=note)

    async def clear_alert(self, request: AlertRequest) -> AlertAck:
        alert = self.store.close_alert(request.station_id)
        if alert is None:
            return AlertAck(raised=False, note="nothing open for that station")
        self.cleared += 1
        return AlertAck(raised=False, note=f"cleared after {alert.duration_seconds():.0f}s")

    def summary(self) -> str:
        return f"{len(self.store.open_alerts())} open, {self.raised} raised, {self.cleared} cleared"


def build_service() -> AlertService:
    """Single construction point, so the worker and the tests agree on the wiring."""
    return AlertService(AlertStore())
