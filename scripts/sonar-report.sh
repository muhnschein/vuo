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

# `${1:-default}` and `${VAR:+...}` are avoided throughout. Not taste:
# SonarQube's own shell analyser cannot parse them -- it reported "Syntax
# error at 121:63" on the first version of this file and then analysed none
# of it. A script that turns off the checker it ships beside is not clever.
if [ "$#" -ge 1 ]; then
    TASK_FILE=$1
else
    TASK_FILE=.scannerwork/report-task.txt
fi

# printenv rather than ${VAR:-}, for the same reason, and `|| true` because
# `set -u` would otherwise make an absent variable fatal.
SONAR_TOKEN=$(printenv SONAR_TOKEN || true)
SUMMARY=$(printenv GITHUB_STEP_SUMMARY || true)

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
SCOPE_Q=""
SCOPE_LABEL="default branch"
if [ -n "$SCOPE" ]; then
    SCOPE_Q="$SCOPE&"
    SCOPE_LABEL=$SCOPE
fi

# Where every response body lands, so nothing has to survive a shell variable.
BODY=$(mktemp)

# Ask, with credentials when there are any.
#
# SonarQube Cloud answers 404 -- not 403 -- for a resource the caller may not
# read, and the first version of this script took that at face value: it
# treated 404 as "no such thing", never retried with the token, and reported
# nothing at all. So the token goes FIRST now. The scanner itself reads these
# same endpoints with the analysis token when `sonar.qualitygate.wait` is set,
# which makes it the likelier of the two to be allowed; anonymous is the
# fallback, for a public project where the token lacks browse rights.
code=""
fetch() {
    if [ -n "$2" ]; then
        code=$(curl -sS --max-time 30 -u "$2:" -o "$BODY" -w '%{http_code}' "$1") || return 1
    else
        code=$(curl -sS --max-time 30 -o "$BODY" -w '%{http_code}' "$1") || return 1
    fi
    [ "$code" = "200" ]
}

api() {
    if [ -n "$SONAR_TOKEN" ] && fetch "$1" "$SONAR_TOKEN"; then
        return 0
    fi
    if fetch "$1" ""; then
        return 0
    fi
    echo "  cannot read $1 (HTTP $code)" >&2
    return 1
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
misses=0
for _ in $(seq 60); do
    if api "$TASK_URL"; then
        misses=0
        status=$(jq -r '.task.status // "?"' "$BODY")
        if [ "$status" = "SUCCESS" ]; then
            analysis=$(jq -r '.task.analysisId // ""' "$BODY")
            break
        fi
        if [ "$status" = "FAILED" ] || [ "$status" = "CANCELED" ]; then
            break
        fi
    else
        # A task can be briefly invisible right after upload, so one bad
        # answer is not a verdict -- but a permanent one must not cost five
        # minutes of runner time either.
        misses=$((misses + 1))
        if [ "$misses" -ge 5 ]; then
            echo "the compute task cannot be read; giving up" >&2
            break
        fi
    fi
    sleep 5
done

if [ "$status" != "SUCCESS" ]; then
    if [ -z "$status" ]; then
        status=unknown
    fi
    echo "the server did not finish processing the report (status: $status)" >&2
    exit 1
fi

out=$(mktemp)
trap 'rm -f "$out" "$BODY"' EXIT

{
    echo "## SonarQube Cloud"
    echo
    echo "\`$KEY\` — $SCOPE_LABEL"
    echo
} >"$out"

# ------------------------------------------------------------ quality gate
if api "$SERVER/api/qualitygates/project_status?analysisId=$analysis"; then
    # The parentheses are load-bearing: `|` binds looser than `,` in jq, so
    # without them the pipe is applied to the two heading strings as well and
    # the whole program dies on "Cannot index string with string".
    jq -r '
        "### Quality gate: \(.projectStatus.status)", "",
        ((.projectStatus.conditions // [])[]
         | "- \(.status)  \(.metricKey) \(.comparator) \(.errorThreshold) (actual: \(.actualValue // "none"))")
    ' "$BODY" >>"$out"
    echo >>"$out"
fi

# ------------------------------------------------------------ measures
metrics=ncloc,coverage,line_coverage,duplicated_lines_density,violations,security_hotspots,security_rating,reliability_rating,sqale_rating,new_coverage,new_violations
if api "$SERVER/api/measures/component?component=$KEY&${SCOPE_Q}metricKeys=$metrics"; then
    {
        echo "### Measures"
        echo
        jq -r '
            (.component.measures // [])[]
            | "\(.metric)=\(.value // .period.value // "-")"
        ' "$BODY" | while IFS='=' read -r metric value; do
            case "$metric" in
                *_rating) echo "- $metric: $(letter "$value")" ;;
                *)        echo "- $metric: $value" ;;
            esac
        done
        echo
    } >>"$out"
fi

# ------------------------------------------------------------ issues
if api "$SERVER/api/issues/search?componentKeys=$KEY&${SCOPE_Q}resolved=false&ps=100"; then
    total=$(jq -r '.total // 0' "$BODY")
    {
        echo "### Open issues: $total"
        echo
        if [ "$total" = "0" ]; then
            echo "None."
        else
            echo '```'
            jq -r '
                (.issues // [])[]
                | "\(.severity // (.impacts[0].severity? // "?"))  \(.rule)  \(.component | sub("^[^:]*:";""))\(if .line then ":\(.line)" else "" end)  \(.message)"
            ' "$BODY"
            if [ "$total" -gt 100 ]; then
                echo "... $((total - 100)) more not listed"
            fi
            echo '```'
        fi
        echo
    } >>"$out"
fi

echo "$DASHBOARD" >>"$out"

cat "$out"
if [ -n "$SUMMARY" ]; then
    cat "$out" >>"$SUMMARY"
fi
