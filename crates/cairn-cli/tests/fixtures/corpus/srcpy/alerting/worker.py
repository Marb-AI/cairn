"""Entrypoint for `python3 -m alerting.worker`, which is how compose starts this service.

Reaching this module from a symbol is what proves the second deployment shape: the rule
pack has to turn `python3 -m alerting.worker` into this file, the same way it turns a
built binary into a Go main.
"""

from __future__ import annotations

import asyncio
import sys

from alerting.service import AlertService, build_service
from schema.telemetry import AlertRequest, ReadingBatch, TelemetryServiceStub

POLL_SECONDS = 30.0


class Worker:
    """Polls the collector and feeds anything alarming into the service."""

    def __init__(self, service: AlertService, stub: TelemetryServiceStub) -> None:
        self.service = service
        self.stub = stub
        self.polls = 0

    async def poll_once(self) -> int:
        """One pass. Returns how many alerts it raised."""
        ack = await self.stub.upload_readings(ReadingBatch(source="worker"))
        self.polls += 1
        return int(ack.accepted)

    async def feed(self, station_id: str, celsius: float) -> str:
        ack = await self.service.raise_alert(
            AlertRequest(station_id=station_id, celsius=celsius, reason="")
        )
        return ack.note

    async def run_forever(self) -> None:
        while True:
            await self.poll_once()
            await asyncio.sleep(POLL_SECONDS)


def build_worker() -> Worker:
    return Worker(build_service(), TelemetryServiceStub(channel=None))


def main(argv: list[str] | None = None) -> int:
    args = argv if argv is not None else sys.argv[1:]
    worker = build_worker()
    if "--once" in args:
        asyncio.run(worker.poll_once())
        print(worker.service.summary())
        return 0
    asyncio.run(worker.run_forever())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
