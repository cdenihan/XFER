#!/bin/sh

set -eu

release_date=${1:-"$(date -u +'%Y.%m.%d')"}
case "$release_date" in
    [0-9][0-9][0-9][0-9].[0-9][0-9].[0-9][0-9]) ;;
    *)
        printf '%s\n' "invalid release date: $release_date" >&2
        exit 1
        ;;
esac

prefix="refs/tags/v${release_date}."
highest=0
while IFS= read -r reference || [ -n "$reference" ]; do
    case "$reference" in
        "$prefix"*)
            number=${reference#"$prefix"}
            case "$number" in
                "" | *[!0-9]*) continue ;;
            esac
            if [ "$number" -gt "$highest" ]; then
                highest=$number
            fi
            ;;
    esac
done

printf '%s.%s\n' "$release_date" "$((highest + 1))"
