emit_perf_data() {
    local recording=$1 size digest
    size=$(wc -c < "$recording")
    digest=$(sha256sum < "$recording")
    printf 'w11term-perf-data-v1\t%s\t%s\n' "$size" "${digest%% *}"
    cat -- "$recording"
}
