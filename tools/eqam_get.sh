#!/usr/bin/env bash
# GET one EQAM CGI page with the session id appended. Read-only helper.
# Usage: eqam_get.sh <sid> <page.cgi> [extra query, e.g. slotNumber=1] > out.html
# Refuses obviously dangerous endpoints / params.
set -euo pipefail
host=${EQAM_HOST:-10.42.0.184}
sid=$1; page=$2; extra=${3:-}
case "$page$extra" in
  *logout*|*reboot*|*reset*|*default*|*upgrade*|*update*|*restore*|*apply*|*save*|*delete*|*Submit*|*btn*)
    echo "refusing potentially mutating request: $page $extra" >&2; exit 2;;
esac
url="http://$host/cgi-bin/$page?session_id=$sid${extra:+&$extra}"
sleep "${EQAM_DELAY:-1.5}"
exec curl -sS --max-time 30 -b "session_id=$sid" "$url"
