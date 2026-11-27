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
timeout -k 2s -- "${seconds}s" /bin/bash -c '
    work=$1
    shift
    status=0
    "$@" </dev/null >/dev/null 2>&1 || status=$?
    printf "exit\t%s\n" "$status" > "$work/exited"
    exit "$status"
' w11term-workload "$work" "$@" &
job=$!
trap 'kill -KILL -- "-$job" 2>/dev/null || :' EXIT
status=0
wait "$job" || status=$?
if [[ -s $work/exited ]]; then
    mv -- "$work/exited" "$work/outcome"
elif [[ $status == 124 || $status == 137 ]]; then
    printf 'timeout\n' > "$work/outcome"
else
    printf 'Could not launch the reviewed command (status %s).\n' "$status" >&2
    exit 2
fi
