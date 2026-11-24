#!/bin/sh
# CV-199: apply migrations then serve. Skip migrate with CORVIN_MIGRATE_ON_START=0.
set -e
if [ "${CORVIN_MIGRATE_ON_START:-1}" = "1" ]; then
  corvin-server migrate up
fi
exec corvin-server
