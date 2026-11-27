set -euo pipefail
work=$1
mode=$2
pid=$3
expected=$4
seconds=$5
frequency=$6
call_graph=$7
record_kib=$8
stacks_kib=$9

fail() {
    printf '%s\n' "$1" >&2
    exit 2
}

read_process() {
    local stat_line fields
    stat_line=$(< "/proc/$pid/stat") || fail 'The target process is no longer visible.'
    # The parenthesized comm can contain parentheses, spaces and newlines.
    local tail="${stat_line##*) }"
    read -r -a fields <<< "$tail"
    started=${fields[19]-}
    [[ $started =~ ^[0-9]+$ ]] || fail 'Could not read the process start time.'
    comm=${stat_line%") $tail"}
    comm=${comm#*\(}
}

emit_process() {
    printf 'w11term-cpu-profile-v1\t%s\t' "$started"
    printf '%s' "$comm" | base64 -w 0
    printf '\n'
}

ulimit -c 0
read_process
if [[ $mode == inspect ]]; then
    perf --version >/dev/null
    emit_process
    exit 0
fi
[[ $started == "$expected" ]] || fail 'The target PID was reused. Preview the process again.'

# Set a hard file-size limit for each phase. stdout and stderr are also bounded
# by the SSH client. Do not update the user's perf build-id cache.
record_status=0
(
    ulimit -f "$record_kib" || exit 2
    exec perf record --no-buildid-cache --no-inherit -e cpu-clock:u \
        -F "$frequency" --call-graph "$call_graph" -p "$pid" \
        -o "$work/perf.data" -- sleep "$seconds"
) > /dev/null || record_status=$?
if [[ -f "$work/perf.data" && $(wc -c < "$work/perf.data") -ge $((record_kib * 1024)) ]]; then
    fail 'The perf recording reached its file size limit.'
fi
[[ $record_status -eq 0 && -s "$work/perf.data" ]] \
    || fail 'perf record failed to produce a profile. Check the perf diagnostics above.'
read_process
[[ $started == "$expected" ]] || fail 'The target PID changed during the profile.'

if ! (
    ulimit -f "$stacks_kib" || exit 2
    exec perf script -i "$work/perf.data" --pid "$pid" -F pid,time,event,ip,sym,dso
) > "$work/stacks"; then
    fail 'perf script failed or reached the stack output limit.'
fi
[[ $(wc -c < "$work/stacks") -lt $((stacks_kib * 1024)) ]] \
    || fail 'The perf stacks reached their file size limit.'
emit_process
cat -- "$work/stacks"
printf '\nw11term-cpu-profile-end\n'
