#!/bin/sh
# probe.sh <label> <net-probe args...>: start net-probe in the background;
# the result lands in /tmp/<label>.json (stderr in /tmp/<label>.log), and
# /tmp/<label>.done when it exits. Starting a label twice is a no-op (the
# orchestrator retries flaky execs).
label="$1"; shift
if [ -e "/tmp/${label}.started" ]; then echo started; exit 0; fi
touch "/tmp/${label}.started"
. /run/probe.env
setsid sh -c "net-probe \"\$@\" --label '${label}' > '/tmp/${label}.json' 2> '/tmp/${label}.log'; touch '/tmp/${label}.done'" probe "$@" < /dev/null > /dev/null 2>&1 &
echo started
