#!/bin/sh
# Schedules the nightly digest on the host. Not run by anything in the tree, which is
# the point: nothing in the source calls this, so only reading the deployment finds it.
#
# The service name lives in a variable defined above the schedule, because that is the
# shape a real installer has and the shape the parser has to survive.
set -eu

WORKER=alert-worker
LOGDIR=/var/log/telemetry

crontab - <<EOF
# Digest yesterday's alerts and hand them to the dispatcher.
30 3 * * * docker exec "\$(docker ps -q -f name=${WORKER} | head -1)" /app/nightly_digest.sh >> ${LOGDIR}/digest.log 2>&1
EOF
