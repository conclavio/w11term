work=$1
ulimit -c 0 || exit 125
run_check() {
    local id=$1 status=0
    shift
    date +%s%3N > "$work/$id.start" || return
    (
        # Bash's file limit is in 1024-byte blocks. Each stream has its own file.
        ulimit -f 16 || exit 125
        "$@"
    ) > "$work/$id.out" 2> "$work/$id.err" || status=$?
    printf '%s %s\n' "$status" "$(date +%s%3N)" > "$work/$id.done"
}
kernel_log() {
    set -o pipefail
    dmesg -T | tail -n 50
}

run_check uptime uptime &
run_check dmesg kernel_log &
run_check vmstat vmstat 1 5 &
run_check mpstat mpstat -P ALL 1 5 &
run_check pidstat pidstat 1 5 &
run_check iostat iostat -xz -y 1 5 &
run_check free free -m &
run_check sar-dev sar -n DEV 1 5 &
run_check sar-tcp sar -n TCP,ETCP 1 5 &
run_check top top -b -n 1 -w 120 &
wait
