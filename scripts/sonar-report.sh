#!/usr/bin/env bash
# What SonarQube Cloud made of an analysis, printed where it can be read.
#
# The scanner uploads a report and exits; the server processes it afterwards,
# so the run that produced the analysis finishes knowing nothing about its
# result. Everything -- the quality gate, the ratings, the issue list -- lives
# on a dashboard behind a login. That is fine for a person with a browser and
# useless to anything else: CI cannot act on it, and neither can a reviewer
# reading the job log, or an agent whose network policy does not reach
# sonarcloud.io.
#
# So this asks the server, from the runner that just fed it, and prints the
# answer into the job log and the step summary. The numbers land beside the
# commit that earned them.
#
# It reports and never gates (docs/scope.md §8): the workflow marks the step
# continue-on-error, so a Sonar outage costs a warning, not a build.
#
# Usage:  scripts/sonar-report.sh [path/to/report-task.txt]
#
# The scanner writes that file at the end of a run. Everything needed is in
# it -- server, project, task, and which branch or pull request was analysed
# -- which is also what makes this testable against a stub server.
set -euo pipefail

TASK_FILE="${1:-.scannerwork/report-task.txt}"

if [ ! -f "$TASK_FILE" ]; then
    echo "no $TASK_FILE -- the scanner did not get as far as uploading" >&2
    exit 1
fi

field() { sed -n "s|^$1=||p" "$TASK_FILE" | head -1; }

SERVER=$(field serverUrl)
KEY=$(field projectKey)
TASK_URL=$(field ceTaskUrl)
DASHBOARD=$(field dashboardUrl)

# Which slice was analysed. Taken from the dashboard URL the scanner wrote
# rather than passed in, so this cannot disagree with what was uploaded.
SCOPE=$(printf '%s' "$DASHBOARD" | sed -n 's/.*[?&]\(pullRequest=[^&]*\).*/\1/p')
if [ -z "$SCOPE" ]; then
    SCOPE=$(printf '%s' "$DASHBOARD" | sed -n 's/.*[?&]\(branch=[^&]*\).*/\1/p')
fi

# Anonymous first. Every endpoint below is readable without credentials on a
# public project, and the SONAR_TOKEN a scan runs with is an ANALYSIS token,
# which the browse endpoints refuse -- sending it would turn a working request
# into a 403. The fallback is for a private project, where anonymous is the
# one that gets turned away.
body=""
api() {
    local url=$1 out code
    body=""
    out=$(curl -sS --max-time 30 -w '\n%{http_code}' "$url" 2>/dev/null) || return 1
    code=${out##*$'\n'}
    body=${out%$'\n'*}
    if { [ "$code" = "401" ] || [ "$code" = "403" ]; } && [ -n "${SONAR_TOKEN:-}" ]; then
        out=$(curl -sS --max-time 30 -w '\n%{http_code}' -u "$SONAR_TOKEN:" "$url" 2>/dev/null) || return 1
        code=${out##*$'\n'}
        body=${out%$'\n'*}
    fi
    [ "$code" = "200" ] || { echo "  $url -> HTTP $code" >&2; return 1; }
    return 0
}

# A rating is 1..5 on the wire and A..E everywhere a person reads it.
letter() { echo "$1" | sed 's/^1.*/A/; s/^2.*/B/; s/^3.*/C/; s/^4.*/D/; s/^5.*/E/'; }

# ------------------------------------------------------------ wait for it
#
# Analysis is asynchronous. Asking for measures before the server has finished
# processing returns the PREVIOUS run's numbers, which is worse than no
# numbers at all: they look right.
status=""
analysis=""
for _ in $(seq 60); do
    api "$TASK_URL" || break
    status=$(printf '%s' "$body" | jq -r '.task.status // "?"')
    case "$status" in
        SUCCESS)
            analysis=$(printf '%s' "$body" | jq -r '.task.analysisId // ""')
            break
            ;;
        FAILED|CANCELED) break ;;
    esac
    sleep 5
done

if [ "$status" != "SUCCESS" ]; then
    echo "the server did not finish processing the report (status: ${status:-unknown})" >&2
    exit 1
fi

out=$(mktemp)
trap 'rm -f "$out"' EXIT

{
    echo "## SonarQube Cloud"
    echo
    echo "\`$KEY\` — ${SCOPE:-default branch}"
    echo
} >"$out"

# ------------------------------------------------------------ quality gate
if api "$SERVER/api/qualitygates/project_status?analysisId=$analysis"; then
    # The parentheses are load-bearing: `|` binds looser than `,` in jq, so
    # without them the pipe is applied to the two heading strings as well and
    # the whole program dies on "Cannot index string with string".
    printf '%s' "$body" | jq -r '
        "### Quality gate: \(.projectStatus.status)", "",
        ((.projectStatus.conditions // [])[]
         | "- \(.status)  \(.metricKey) \(.comparator) \(.errorThreshold) (actual: \(.actualValue // "none"))")
    ' >>"$out"
    echo >>"$out"
fi

# ------------------------------------------------------------ measures
metrics=ncloc,coverage,line_coverage,duplicated_lines_density,violations,security_hotspots,security_rating,reliability_rating,sqale_rating,new_coverage,new_violations
if api "$SERVER/api/measures/component?component=$KEY&${SCOPE:+$SCOPE&}metricKeys=$metrics"; then
    {
        echo "### Measures"
        echo
        printf '%s' "$body" | jq -r '
            (.component.measures // [])[]
            | "\(.metric)=\(.value // .period.value // "-")"
        ' | while IFS='=' read -r metric value; do
            case "$metric" in
                *_rating) echo "- $metric: $(letter "$value")" ;;
                *)        echo "- $metric: $value" ;;
            esac
        done
        echo
    } >>"$out"
fi

# ------------------------------------------------------------ issues
if api "$SERVER/api/issues/search?componentKeys=$KEY&${SCOPE:+$SCOPE&}resolved=false&ps=100"; then
    total=$(printf '%s' "$body" | jq -r '.total // 0')
    {
        echo "### Open issues: $total"
        echo
        if [ "$total" = "0" ]; then
            echo "None."
        else
            echo '```'
            printf '%s' "$body" | jq -r '
                (.issues // [])[]
                | "\(.severity // (.impacts[0].severity? // "?"))  \(.rule)  \(.component | sub("^[^:]*:";""))\(if .line then ":\(.line)" else "" end)  \(.message)"
            '
            [ "$total" -gt 100 ] && echo "... $((total - 100)) more not listed"
            echo '```'
        fi
        echo
    } >>"$out"
fi

echo "$DASHBOARD" >>"$out"

cat "$out"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$out" >>"$GITHUB_STEP_SUMMARY"
fi
