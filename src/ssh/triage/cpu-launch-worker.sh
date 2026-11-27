set -euo pipefail
work=$1
mode=$2
runner=$3
seconds=$4
frequency=$5
call_graph=$6
record_kib=$7
stacks_kib=$8
directory=$9
shift 9
program=$1
expected_directory=$2
expected_program=$3
shift 3

fail() {
    printf '%s\n' "$1" >&2
    exit 2
}

cd -P -- "${directory:-$HOME}" || fail 'The working directory is unavailable.'
directory=$PWD
case "$program" in
    /*) ;;
    */*) program=${directory%/}/${program#./} ;;
    *) program=$(type -P -- "$program") || fail 'The executable is not on the SSH diagnostic PATH.' ;;
esac
[[ -f $program && -x $program ]] || fail 'The reviewed program is not executable.'

emit_command() {
    printf 'w11term-cpu-launch-v1\t'
    printf '%s' "$directory" | base64 -w 0
    printf '\t'
    printf '%s' "$program" | base64 -w 0
    printf '\n'
}

if [[ $mode == inspect ]]; then
    perf --version >/dev/null
    emit_command
    exit 0
fi

[[ $directory == "$expected_directory" && $program == "$expected_program" ]] \
    || fail 'The command path or working directory changed. Review the command again.'

# Cap the profiler's file size without reducing the workload's inherited limits.
workload_file_limit=$(ulimit -Sf)
workload_core_limit=$(ulimit -Sc)
perf_limit=$record_kib
if [[ $workload_file_limit != unlimited ]]; then
    (( workload_file_limit >= perf_limit )) || perf_limit=$workload_file_limit
fi
record_status=0
(
    ulimit -Sf "$perf_limit" && ulimit -Sc 0 || exit 2
    exec perf record --no-buildid-cache --all-user --user-callchains -e cpu-clock:u \
        -F "$frequency" --call-graph "$call_graph" -o "$work/perf.data" \
        -- /bin/bash -c "$runner" w11term-cpu-workload "$work" "$seconds" \
        "$workload_file_limit" "$workload_core_limit" "$program" "$@"
) > /dev/null || record_status=$?

if [[ -f "$work/perf.data" && $(wc -c < "$work/perf.data") -ge $((perf_limit * 1024)) ]]; then
    fail 'The perf recording reached its file size limit.'
fi
[[ $record_status == 0 && -s "$work/perf.data" && -s "$work/outcome" ]] \
    || fail 'perf record did not produce a complete command profile. Check the perf diagnostics above.'

if ! (
    ulimit -f "$stacks_kib" && ulimit -c 0 || exit 2
    exec perf script -i "$work/perf.data" -F pid,time,event,ip,sym,dso
) > "$work/stacks"; then
    fail 'perf script failed or reached the stack output limit.'
fi
[[ $(wc -c < "$work/stacks") -lt $((stacks_kib * 1024)) ]] \
    || fail 'The perf stacks reached their file size limit.'
emit_command
cat -- "$work/outcome" "$work/stacks"
printf '\nw11term-cpu-profile-end\n'
