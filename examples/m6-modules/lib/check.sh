# Test helpers: equal <expected> <actual> <what>
equal() {
    if [ "$1" != "$2" ]; then
        echo "$3: expected '$1', got '$2'" >&2
        exit 1
    fi
    echo "$3: ok"
}
