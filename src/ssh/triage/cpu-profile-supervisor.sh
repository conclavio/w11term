# w11term-cpu-profile-v1
set -eu
export PATH=/usr/sbin:/usr/bin:/sbin:/bin LC_ALL=C
export PERF_CONFIG=/dev/null DEBUGINFOD_URLS=
unset BASH_ENV ENV POSIXLY_CORRECT
if [ "$(uname -s)" != Linux ]; then
    printf '%s\n' 'CPU profiling requires Linux.' >&2
    exit 2
fi
for tool in bash timeout perf base64 mktemp rm cat wc sleep; do
    if ! command -v "$tool" >/dev/null; then
        printf 'CPU profiling requires %s.\n' "$tool" >&2
        exit 2
    fi
done
worker=$1
deadline=$2
shift 2
umask 077
work=
if [ "$1" != inspect ]; then
    work=$(mktemp -d /tmp/w11term-cpu-profile.XXXXXXXX)
    trap 'rm -rf -- "$work"' EXIT
fi
# timeout owns the worker group. Keep the supervisor alive to remove its files.
trap '' HUP
trap 'exit 1' PIPE
code=0
timeout -k 2s "${deadline}s" bash -c "$worker" w11term-cpu-profile "$work" "$@" || code=$?
case "$code" in
    124|137) printf '%s\n' 'The remote CPU profile worker exceeded its deadline.' >&2 ;;
esac
exit "$code"
