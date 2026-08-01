"""Tests for the rules and the dispatch registry.

Under `tests/`, so the rule pack marks them as test files and `--exclude-tests` has
something to leave out.
"""

from __future__ import annotations

from alerting.dispatch import dispatch, handle_unknown, registered_reasons
from alerting.models import Alert, AlertStore
from alerting.rules import Judgement, classify, plausible, severity_rank, worst


def test_severe_cold_is_critical() -> None:
    j = classify(-30.0, 50.0)
    assert j.alarming
    assert j.severity == "critical"
    assert j.escalates


def test_nominal_is_not_alarming() -> None:
    assert not classify(12.0, 55.0).alarming


def test_freezing_fog_needs_both_conditions() -> None:
    assert classify(-1.0, 95.0).reason == "freezing fog likely"
    assert classify(-1.0, 40.0).reason == "frost"


def test_an_implausible_reading_is_ignored_not_alarming() -> None:
    j = classify(-300.0, 50.0)
    assert not j.alarming
    assert "implausible" in j.reason


def test_plausible_bounds() -> None:
    assert plausible(0.0, 0.0)
    assert not plausible(0.0, 140.0)


def test_worst_picks_the_highest_severity() -> None:
    picked = worst([Judgement(True, "warning", "a"), Judgement(True, "critical", "b")])
    assert picked is not None
    assert picked.severity == "critical"


def test_worst_of_nothing_is_none() -> None:
    assert worst([]) is None


def test_severity_rank_is_total() -> None:
    ranks = [severity_rank(s) for s in ("info", "warning", "severe", "critical")]
    assert ranks == sorted(ranks)


def test_dispatch_reaches_a_registered_handler() -> None:
    store = AlertStore()
    alert = store.open_alert("alp-01", "severe cold", -30.0)
    assert "alp-01" in dispatch(alert, store)


def test_dispatch_falls_back_for_an_unknown_reason() -> None:
    store = AlertStore()
    alert = store.open_alert("alp-01", "nothing anyone registered", 0.0)
    assert dispatch(alert, store) == handle_unknown(alert, store)


def test_every_reason_is_registered_once() -> None:
    reasons = registered_reasons()
    assert len(reasons) == len(set(reasons))
    assert "severe cold" in reasons


def test_an_alert_closes_once() -> None:
    store = AlertStore()
    store.open_alert("dune-02", "frost", -1.0)
    closed = store.close_alert("dune-02")
    assert closed is not None
    assert not closed.is_open()
    assert store.close_alert("nowhere") is None


def test_state_resets_after_a_calm_reading() -> None:
    state = AlertStore().state_for("alp-01")
    state.note_reading(-30.0, True)
    state.note_reading(-30.0, True)
    state.note_reading(5.0, False)
    assert state.consecutive_alarming == 0
    assert not state.should_escalate()


def test_alert_row_round_trips() -> None:
    alert = Alert(station_id="alp-01", reason="frost", celsius=-1.0)
    row = alert.as_row()
    assert row["station_id"] == "alp-01"
