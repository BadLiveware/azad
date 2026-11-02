"""Actions for a Go module: download its dependencies once per go.sum, build it, and test it."""

# The image gives PATH. Go writes its build cache and home into /tmp, which Azad discards after each action.
_BASE_ENV = {
    "HOME": "/tmp/home",
    "GOPATH": "/tmp/gopath",
    "GOCACHE": "/tmp/gocache",
    "GOTOOLCHAIN": "local",
    "CGO_ENABLED": "0",
}

def go_module(name, root, src, tests, lock):
    """Declares <name>-deps, <name>-build and <name>-test, and returns them.

    src: the sources that the build reads (no _test.go files).
    tests: the sources that the tests read.
    lock: go.mod and go.sum, which pin the dependencies.
    """
    deps = fetch(
        name = name + "-deps",
        cmd = ["sh", "-c", "cd /lock && go mod download"],
        root = root,
        inputs = {"/lock": lock},
        outputs = ["mod"],
        env = dict(_BASE_ENV, GOMODCACHE = "/work/mod", GOFLAGS = "-modcacherw"),
    )
    offline = dict(_BASE_ENV, GOMODCACHE = "/deps", GOPROXY = "off", GOFLAGS = "-mod=readonly")
    build = action(
        name = name + "-build",
        cmd = ["sh", "-c", "cd /src && go build -trimpath -o /work/bin/ ./..."],
        root = root,
        inputs = {"/src": src, "/deps": deps.out("mod")},
        outputs = ["bin"],
        env = offline,
    )
    test = action(
        name = name + "-test",
        cmd = ["sh", "-c", "cd /src && go vet ./... && go test ./..."],
        root = root,
        inputs = {"/src": tests, "/deps": deps.out("mod")},
        env = offline,
    )
    return struct(deps = deps, build = build, test = test)
