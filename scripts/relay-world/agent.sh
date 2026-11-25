#!/bin/sh
# Machine entrypoint: keep the probe's env (PARTII_KEY from the app
# secrets, PARTII_EDGE, FLY_REGION) for probe.sh runs started with
# `flyctl machine exec`, then idle until the run destroys the machine.
umask 077
env | grep -E '^(PARTII_KEY|PARTII_EDGE|FLY_REGION|FLY_MACHINE_ID)=' | sed 's/^/export /' > /run/probe.env
exec sleep infinity
