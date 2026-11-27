# w11term-packet-capture-v1
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin LC_ALL=C
unset CDPATH
mode=$1 interface=$2 snaplen=$3 count=$4 seconds=$5 budget_kib=$6
require_tmpfs=$7 root=$8 filter=$9
shift 9
expected_mount=$1 job=$2

fail() {
    printf '%s\n' "$*" >&2
    exit 2
}
[[ $(uname -s) == Linux ]] || fail 'Packet capture requires Linux.'
for tool in tcpdump flock timeout mkdir stat df tail id sha256sum mkfifo head cat wc rm; do
    command -v "$tool" >/dev/null || fail "Packet capture requires $tool."
done
cd -P -- "$root" || fail 'The selected scratch directory is unavailable.'
root=$PWD
[[ -w $root ]] || fail 'The selected scratch directory is not writable.'
mount_kind=$(stat -f -c %T -- "$root")
mount_id=$(stat -c '%d:%i' -- "$root")
if [[ $require_tmpfs == yes && $mount_kind != tmpfs ]]; then
    fail 'The selected RAM scratch directory is not on tmpfs.'
fi
capacity() {
    local kib
    kib=$(df -k --output=avail -- "$root" | tail -n 1)
    kib=${kib//[[:space:]]/}
    [[ $kib =~ ^[0-9]+$ ]] || fail 'Could not read Linux scratch capacity.'
    (( kib >= budget_kib + 1024 )) || fail 'Scratch capacity cannot meet the capture budget and 1 MiB headroom.'
}
args=(-i "$interface" -nn -p -s "$snaplen")
expression=()
if [[ -n $filter ]]; then
    expression=("$filter")
fi
capacity
if [[ $mode == inspect ]]; then
    timeout -k 1s 4s tcpdump "${args[@]}" -d -- "${expression[@]}" >/dev/null
    printf 'w11term-packet-plan-v1\t%s\t%s\t%s\n' "$mount_kind" "$mount_id" "$root"
    exit 0
fi
[[ $mount_id == "$expected_mount" ]] || fail 'The reviewed scratch directory changed.'

# Hold one lease across this Linux account's disk and RAM captures.
umask 077
guard=/tmp/w11term-pcap-lock-$(id -u)
mkdir -m 700 -- "$guard" 2>/dev/null || :
[[ -d $guard && ! -L $guard && -O $guard && $(stat -c %a -- "$guard") == 700 ]] \
    || fail 'The account packet capture lock directory is unavailable.'
exec 9<"$guard"
flock -n 9 || fail 'Another packet capture is active for this Linux account.'
capacity
work=${root%/}/w11term-pcap-$job
mkdir -m 700 -- "$work" || fail 'Could not create private capture storage.'
capture= monitor= sink=
cleanup() {
    if [[ -n $capture ]]; then kill -TERM "$capture" 2>/dev/null || :; fi
    if [[ -n $monitor ]]; then kill "$monitor" 2>/dev/null || :; fi
    if [[ -n $sink ]]; then kill "$sink" 2>/dev/null || :; fi
    rm -rf -- "$work"
}
trap cleanup EXIT
trap 'exit 1' PIPE TERM HUP
exec 8<&0
mkfifo -- "$work/stderr"
(head -c 8192; cat >/dev/null) < "$work/stderr" > "$work/diagnostics" &
sink=$!
(
    ulimit -f "$budget_kib"
    ulimit -c 0
    exec timeout -k 2s "${seconds}s" tcpdump "${args[@]}" \
        -c "$count" -w - -- "${expression[@]}"
) </dev/null > "$work/capture.pcap" 2> "$work/stderr" &
capture=$!
(
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
capture=
kill "$monitor" 2>/dev/null || :
wait "$monitor" 2>/dev/null || :
monitor=
wait "$sink"
sink=
[[ ! -e $work/disconnected ]] || exit 1
cat -- "$work/diagnostics" >&2
case "$status" in
    0)
        if [[ -e $work/stopped ]]; then stop=stopped; else stop=packets; fi
        ;;
    143)
        [[ -e $work/stopped ]] || fail 'tcpdump was terminated unexpectedly.'
        stop=stopped
        ;;
    124) stop=time ;;
    *) fail "tcpdump failed (status $status)." ;;
esac
size=$(wc -c < "$work/capture.pcap")
if [[ $stop == stopped ]] && (( size < 24 )); then
    fail 'Packet capture stopped before tcpdump produced a capture.'
fi
(( size >= 24 && size <= budget_kib * 1024 )) || fail 'The capture is empty or exceeded its byte budget.'
digest=$(sha256sum < "$work/capture.pcap")
printf 'w11term-packet-ready-v1\t%s\t%s\t%s\n' "$stop" "$size" "${digest%% *}"
# The SSH client verifies the SFTP artifact before acknowledging. EOF also
# releases the lease, and the deadline cleans abandoned files on a broken link.
read_acknowledgement() {
    local start=$SECONDS remaining acknowledgement
    IFS= read -r -t 60 acknowledgement <&8 || return 1
    # A Stop crossing the ready record must not discard the completed capture.
    if [[ $acknowledgement == stop ]]; then
        remaining=$((60 - (SECONDS - start)))
        (( remaining > 0 )) && IFS= read -r -t "$remaining" acknowledgement <&8 || return 1
    fi
    [[ $acknowledgement == verified ]]
}
if read_acknowledgement; then
    rm -rf -- "$work" || fail 'Could not clean up the remote capture.'
    trap - EXIT
    printf 'w11term-packet-complete-v1\n'
else
    fail 'The packet capture transfer lease ended.'
fi
