set -eu
work=$1
seconds=$2
file_limit=$3
core_limit=$4
shift 4
ulimit -Sf "$file_limit"
ulimit -Sc "$core_limit"

# timeout starts its own process group. Remove lingering children in that
# group even if the foreground workload returned before its deadline.
exec 8<&0
timeout -k 2s -- "${seconds}s" /bin/bash -c '
    work=$1
    shift
    status=0
    "$@" </dev/null >/dev/null 2>&1 || status=$?
    printf "exit\t%s\n" "$status" > "$work/exited"
    exit "$status"
' w11term-workload "$work" "$@" </dev/null 8<&- &
job=$!
trap 'kill -KILL -- "-$job" 2>/dev/null || :' EXIT
(
    if IFS= read -r request <&8 && [[ $request == stop ]]; then
        : > "$work/stop-requested"
        kill -TERM "$job" 2>/dev/null || :
    fi
) &
monitor=$!
exec 8<&-
status=0
wait "$job" || status=$?
kill -TERM "$monitor" 2>/dev/null || :
wait "$monitor" 2>/dev/null || :
if [[ -s $work/exited ]]; then
    mv -- "$work/exited" "$work/outcome"
elif [[ -f $work/stop-requested && ( $status == 124 || $status == 143 || $status == 137 ) ]]; then
    # GNU timeout can also return 124 after receiving our explicit TERM.
    printf 'stopped\n' > "$work/outcome"
elif [[ $status == 124 || $status == 137 ]]; then
    printf 'timeout\n' > "$work/outcome"
else
    printf 'Could not launch the reviewed command (status %s).\n' "$status" >&2
    exit 2
fi
