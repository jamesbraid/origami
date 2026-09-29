gitlink_revision() {
    name=$1
    entry=$(git -C "$product" ls-files --stage -- "$name") || return
    set -- $entry
    if [ "$#" -ne 4 ] || [ "$1" != 160000 ] || [ "$3" != 0 ] || [ "$4" != "$name" ]; then
        printf 'frontend index has no stage-zero Git submodule entry for %s\n' "$name" >&2
        return 2
    fi
    printf '%s\n' "$2"
}
