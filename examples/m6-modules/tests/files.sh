. "$(dirname "$0")/../lib/check.sh"
tmp=$(mktemp -d)
printf 'one\ntwo\n' > "$tmp/lines"
equal 2 "$(wc -l < "$tmp/lines" | tr -d ' ')" "line count"
