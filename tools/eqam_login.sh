#!/usr/bin/env bash
# Log in to the EQAM-420B web UI. Read-only helper.
# Usage: source tools/eqam_login.sh   (sets EQAM_HOST, EQAM_SID, EQAM_JAR)
# Creds are fetched from rbw at runtime; never stored.
EQAM_HOST=${EQAM_HOST:-10.42.0.184}
EQAM_JAR=${EQAM_JAR:-$(mktemp)}
_eqam_pw=$(rbw get "$EQAM_HOST" | head -n1)
_eqam_user=$(rbw get --full "$EQAM_HOST" | sed -n 's/^Username: //p')
_page=$(curl -sS -c "$EQAM_JAR" "http://$EQAM_HOST/cgi-bin/login.cgi")
EQAM_SID=$(printf '%s' "$_page" | sed -n 's/.*name="session_id" value="\([0-9]*\)".*/\1/p' | head -n1)
printf 'session_id=%s\n' "$EQAM_SID" > /dev/stderr
curl -sS -b "session_id=$EQAM_SID" -c "$EQAM_JAR" -o "${EQAM_LOGIN_OUT:-/dev/null}" \
  --data-urlencode "txtUserName=$_eqam_user" \
  --data-urlencode "txtPassword=$_eqam_pw" \
  --data-urlencode "session_id=$EQAM_SID" \
  --data-urlencode "btnSubmit=Submit" \
  "http://$EQAM_HOST/cgi-bin/login.cgi"
unset _eqam_pw _eqam_user _page
export EQAM_HOST EQAM_SID EQAM_JAR
