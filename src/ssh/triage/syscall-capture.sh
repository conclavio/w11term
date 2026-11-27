# w11term-syscall-capture-v1
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin LC_ALL=C
unset CDPATH
mode=$1 seconds=$2 bytes=$3 string_size=$4 filter=$5 directory=$6
program=$7 expected_dir=$8 expected_program=$9
shift 9
identity=$1 job=$2
shift 2

fail() { printf '%s\n' "$*" >&2; exit 2; }
[[ $(uname -s) == Linux ]] || fail 'Syscall capture requires Linux.'
for tool in strace timeout flock mkdir sha256sum stat mkfifo wc head cat df tail id rm; do
    command -v "$tool" >/dev/null || fail "Syscall capture requires $tool."
done
cd -P -- "${directory:-$HOME}" || fail 'The working directory is unavailable.'
directory=$PWD
case $program in
    /*) ;;
    */*) program=${directory%/}/${program#./} ;;
    *) program=$(type -P -- "$program") || fail 'The executable is not on the SSH diagnostic PATH.' ;;
esac
[[ -f $program && -x $program ]] || fail 'The reviewed program is not executable.'
# Follow symbolic links so replacing the target invalidates the review.
current_id=$(stat -Lc '%d:%i:%s:%y:%z:%f' -- "$program")
flags=(-f -I 1 --kill-on-exit -ttt -T -yy -s "$string_size" -e "trace=$filter")
if [[ $mode == inspect ]]; then
    timeout -k 1s 3s strace "${flags[@]}" -o /dev/null -- /bin/true \
        || fail 'The remote strace must support descendant tracing and --kill-on-exit, and ptrace must be available.'
    printf 'w11term-syscall-plan-v1\t%s\t%s\t%s\n' "$directory" "$program" "$current_id"
    exit 0
fi
[[ $directory == "$expected_dir" && $program == "$expected_program" && $current_id == "$identity" ]] \
    || fail 'The reviewed executable or working directory changed.'
[[ $job =~ ^[0-9a-f]{32}$ ]] || fail 'Invalid syscall capture job identifier.'
kib=$(df -k --output=avail -- /tmp | tail -n 1)
kib=${kib//[[:space:]]/}
[[ $kib =~ ^[0-9]+$ ]] && (( kib >= bytes / 1024 + 1024 )) \
    || fail 'Linux scratch storage cannot meet the syscall capture budget and 1 MiB headroom.'

workload_umask=$(umask)
umask 077
guard=/tmp/w11term-strace-lock-$(id -u)
mkdir -m 700 -- "$guard" 2>/dev/null || :
[[ -d $guard && ! -L $guard && -O $guard && $(stat -c %a -- "$guard") == 700 ]] \
    || fail 'The account syscall capture lock directory is unavailable.'
exec 9<"$guard"
flock -n 9 || fail 'Another syscall capture is active for this Linux account.'
work=/tmp/w11term-strace-$job
mkdir -m 700 -- "$work" || fail 'Could not create private syscall capture storage.'
capture= monitor= sink=
cleanup() {
    if [[ -n $capture ]]; then kill -TERM "$capture" 2>/dev/null || :; fi
    if [[ -n $monitor ]]; then kill "$monitor" 2>/dev/null || :; fi
    if [[ -n $sink ]]; then kill "$sink" 2>/dev/null || :; fi
    rm -rf -- "$work"
}
trap cleanup EXIT
trap 'exit 1' PIPE HUP TERM
exec 8<&0
mkfifo -- "$work/trace.fifo"
# The supervisor holds the writer until strace exits. An early stop or
# startup failure reaches EOF without passing extra FDs to the workload.
exec 5<>"$work/trace.fifo"
exec 6<"$work/trace.fifo" 7>"$work/trace.fifo"
exec 5>&-
(
    exec 6<&- 7>&- 8<&- 9<&-
    umask "$workload_umask"
    ulimit -c 0
    exec timeout -k 2s "${seconds}s" /bin/bash -c '
        work=$1; shift
        status=0
        strace "$@" || status=$?
        printf "%s\n" "$status" > "$work/exit"
    ' w11term-tracer "$work" "${flags[@]}" -o "$work/trace.fifo" -- "$program" "$@"
) </dev/null >/dev/null 2>/dev/null &
capture=$!
(
    exec 7>&- 6<&- 8<&- 9<&-
    head -c "$bytes" > "$work/trace.strace"
    if [[ $(wc -c < "$work/trace.strace") == "$bytes" ]]; then
        head -c 1 > "$work/excess"
        if [[ -s $work/excess ]]; then
            kill -TERM "$capture" 2>/dev/null || :
        fi
    fi
) <&6 &
sink=$!
exec 6<&-
(
    exec 7>&- 9<&-
    if IFS= read -r request <&8; then
        if [[ $request == stop ]]; then
            : > "$work/stopped"
            kill -TERM "$capture" 2>/dev/null || :
        fi
    else
        : > "$work/disconnected"
        kill -TERM "$capture" 2>/dev/null || :
    fi
) &
monitor=$!
status=0
wait "$capture" || status=$?
exec 7>&-
# GNU timeout puts its child in its own group. Clean up any tracer still
# unwinding after the timeout wrapper exited. EXITKILL also applies to
# descendants which changed process group.
if [[ $status != 0 ]]; then kill -KILL -- "-$capture" 2>/dev/null || :; fi
capture=
kill "$monitor" 2>/dev/null || :
wait "$monitor" 2>/dev/null || :
monitor=
wait "$sink"
sink=
[[ ! -e $work/disconnected ]] || exit 1
exit_code=-
if [[ -s $work/excess ]]; then
    reason=bytes
elif [[ -e $work/stopped ]]; then
    reason=stopped
elif [[ -s $work/exit ]]; then
    reason=exit
    exit_code=$(<"$work/exit")
elif [[ $status == 124 || $status == 137 ]]; then
    reason=time
else
    fail "strace failed (status $status)."
fi
size=$(wc -c < "$work/trace.strace")
(( size > 0 && size <= bytes )) || fail 'strace produced no syscall records. Check the executable and Linux tracing permissions.'
digest=$(sha256sum < "$work/trace.strace")
printf 'w11term-syscall-ready-v1\t%s\t%s\t%s\t%s\n' "$reason" "$exit_code" "$size" "${digest%% *}"
lease_end=$((SECONDS + 60))
if IFS= read -r -t 60 acknowledgement <&8; then
    # A Stop sent after completion may have missed the monitor.
    if [[ $acknowledgement == stop ]]; then
        remaining=$((lease_end - SECONDS))
        if (( remaining <= 0 )) || ! IFS= read -r -t "$remaining" acknowledgement <&8; then acknowledgement=; fi
    fi
    if [[ $acknowledgement == verified ]]; then
        rm -rf -- "$work"
        trap - EXIT
        printf 'w11term-syscall-complete-v1\n'
        exit 0
    fi
fi
fail 'The syscall capture transfer lease ended.'
