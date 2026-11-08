#!/bin/sh
# A fixture connector: the protocol, with nothing behind it.
#
# It stands in for a real tool (w17ctl, say) by printing a fixed set of facts about the
# fixture corpus. That is the point — the protocol and the validation are what this
# repository tests; the meaning of a `domain` belongs to whoever wrote the connector.
#
# The first argument chooses what kind of connector it is today, so one script can stand
# for every failure the core has to survive:
#
#   ok       declares two nodes and three edges, one of them into real code
#   expected an edge into a SCIP symbol this index does not have
#   broken   a malformed line, an undeclared node kind, an undeclared edge kind
#   fail     exits non-zero after printing nothing
#   hang     never exits, so the timeout has to
#
# Deterministic by construction (I3): no clock, no network, no randomness. Two runs over
# unchanged inputs print the same bytes, which is what makes the cache key mean anything.
set -eu

# The core sets both. Checked rather than used, because a connector that silently ignored
# the protocol version would be the first thing to break on a protocol change.
: "${CAIRN_PROTOCOL:?the core sets CAIRN_PROTOCOL}"
: "${CAIRN_REPO:?the core sets CAIRN_REPO}"
[ "$CAIRN_PROTOCOL" = "1" ] || { echo "protocol $CAIRN_PROTOCOL is not 1" >&2; exit 2; }

# A real symbol of the fixture corpus, so one declared edge resolves against the index.
REGISTRY_ADD='scip-go gomod telemetry.example/srcgo . `telemetry.example/srcgo/internal/station`/Registry#Add().'

# The core asks for a version by appending the flag to the whole command, so it arrives
# after the mode rather than instead of it.
case " $* " in
*" --version "*)
	echo "w17-fixture 1.0.0"
	exit 0
	;;
esac

case "${1:-ok}" in
ok)
	echo '{"node":{"id":"w17:domain/telemetry","kind":"domain","name":"telemetry"}}'
	echo '{"node":{"id":"w17:rpc/Station.Register","kind":"rpc","anchor":"proto/telemetry.proto:7"}}'
	echo '{"edge":{"from":"w17:rpc/Station.Register","to":"w17:domain/telemetry","kind":"part_of"}}'
	echo "{\"edge\":{\"from\":\"w17:rpc/Station.Register\",\"to\":\"scip:$REGISTRY_ADD\",\"kind\":\"implemented_by\"}}"
	echo '{"unknown":"rpc Station.Retire: no handler found"}'
	;;
expected)
	echo '{"node":{"id":"w17:rpc/Station.Retire","kind":"rpc"}}'
	echo '{"edge":{"from":"w17:rpc/Station.Retire","to":"scip:scip-go gomod telemetry.example/srcgo . `x`/NotIndexed#Gone().","kind":"implemented_by"}}'
	;;
broken)
	echo '{"node":{"id":"w17:domain/telemetry","kind":"domain"}}'
	echo 'this line is not JSON'
	echo '{"node":{"id":"w17:thing/x","kind":"invented"}}'
	echo '{"edge":{"from":"w17:domain/telemetry","to":"w17:domain/telemetry","kind":"invented"}}'
	;;
fail)
	echo "w17-fixture: the project model could not be read" >&2
	exit 1
	;;
hang)
	while true; do
		sleep 1
	done
	;;
*)
	echo "unknown mode $1" >&2
	exit 2
	;;
esac
