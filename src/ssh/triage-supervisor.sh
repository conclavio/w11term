# w11term-linux-triage-v1
set -eu
export PATH=/usr/sbin:/usr/bin:/sbin:/bin LC_ALL=C TZ=UTC
unset BASH_ENV ENV POSIXLY_CORRECT
if [ "$(uname -s)" != Linux ]; then
    printf '%s\n' 'Performance triage requires Linux.' >&2
    exit 2
fi
for tool in bash timeout mktemp rm date base64; do
    if ! command -v "$tool" >/dev/null; then
        printf 'Performance triage requires %s.\n' "$tool" >&2
        exit 2
    fi
done
umask 077
work=$(mktemp -d /tmp/w11term-triage.XXXXXXXX)
trap 'rm -rf -- "$work"' EXIT
# Let timeout reap the workers and let the supervisor clean up if SSH drops.
trap '' HUP
trap 'exit 1' PIPE
started=$(date +%s%3N)
code=0
timeout -k 2s 50s bash -c "$1" w11term-triage "$work" || code=$?
finished=$(date +%s%3N)
printf 'w11term-linux-triage-v1\t%s\t%s\t%s\n' "$started" "$finished" "$code"
for probe in uptime dmesg vmstat mpstat pidstat iostat free sar-dev sar-tcp top; do
    begin=- end=- status=-
    if [ -s "$work/$probe.start" ]; then
        read -r begin < "$work/$probe.start" || :
    fi
    if [ -s "$work/$probe.done" ]; then
        read -r status end < "$work/$probe.done" || :
    fi
    printf '%s\t%s\t%s\t%s\t' "$probe" "$begin" "$end" "$status"
    if [ -f "$work/$probe.out" ]; then base64 -w 0 "$work/$probe.out"; fi
    printf '\t'
    if [ -f "$work/$probe.err" ]; then base64 -w 0 "$work/$probe.err"; fi
    printf '\n'
done
printf 'end\n'
