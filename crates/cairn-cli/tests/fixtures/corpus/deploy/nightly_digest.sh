#!/bin/sh
# Runs inside the alert-worker container, on the schedule installed by install-cron.sh.
# The container's own start command is the worker loop; this is the other thing it runs.
set -eu

exec python3 -m alerting.dispatch
