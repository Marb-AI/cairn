"""Handlers reached through a registry rather than by name.

This is here to be hard on purpose. Nothing calls ``handle_severe_cold`` directly — it is
registered under a string and looked up at run time — so a call graph sees it as dead
code. It is not, and a tool that reports it as unreached is wrong in the way that costs a
reader the most: confidently.
"""

from __future__ import annotations

from collections.abc import Callable

from alerting.models import Alert, AlertStore

Handler = Callable[[Alert, AlertStore], str]

_REGISTRY: dict[str, Handler] = {}


def register(reason: str) -> Callable[[Handler], Handler]:
    """Decorator: bind a handler to the reason string that selects it."""

    def wrap(fn: Handler) -> Handler:
        _REGISTRY[reason] = fn
        return fn

    return wrap


def dispatch(alert: Alert, store: AlertStore) -> str:
    """Route an alert to whatever handles its reason."""
    handler = _REGISTRY.get(alert.reason, handle_unknown)
    return handler(alert, store)


def registered_reasons() -> list[str]:
    return sorted(_REGISTRY)


@register("severe cold")
def handle_severe_cold(alert: Alert, store: AlertStore) -> str:
    state = store.state_for(alert.station_id)
    if state.should_escalate():
        return f"escalate: {alert.station_id} has been freezing for {state.consecutive_alarming} readings"
    return f"watch: {alert.station_id} at {alert.celsius:.1f} C"


@register("severe heat")
def handle_severe_heat(alert: Alert, store: AlertStore) -> str:
    state = store.state_for(alert.station_id)
    if state.should_escalate(threshold=2):
        return f"escalate: {alert.station_id} is overheating"
    return f"watch: {alert.station_id} at {alert.celsius:.1f} C"


@register("frost")
def handle_frost(alert: Alert, store: AlertStore) -> str:
    return f"advise: frost at {alert.station_id}"


@register("freezing fog likely")
def handle_freezing_fog(alert: Alert, store: AlertStore) -> str:
    return f"advise: freezing fog at {alert.station_id}"


def handle_unknown(alert: Alert, store: AlertStore) -> str:
    """The fallback, and the only handler anything calls by name."""
    return f"ignored: no handler for {alert.reason!r}"
