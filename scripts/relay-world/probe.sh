#!/bin/sh
# probe.sh <label> <net-probe args...>: start net-probe in the background;
# the result lands in /tmp/<label>.json (stderr in /tmp/<label>.log), and
# /tmp/<label>.done when it exits.
label="$1"; shift
. /run/probe.env
rm -f "/tmp/${label}.json" "/tmp/${label}.done"
setsid sh -c "net-probe \"\$@\" --label '${label}' > '/tmp/${label}.json' 2> '/tmp/${label}.log'; touch '/tmp/${label}.done'" probe "$@" < /dev/null > /dev/null 2>&1 &
echo started
