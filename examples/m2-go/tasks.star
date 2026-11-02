load("//examples/modules/go.star", "go_module")

words = go_module(
    name = "words",
    root = config.go_image,
    src = glob(["**/*.go", "go.mod", "go.sum"], exclude = ["**/*_test.go"]),
    tests = glob(["**/*.go", "go.mod", "go.sum"]),
    lock = glob(["go.mod", "go.sum"]),
)

goal("example", require = [words.build, words.test])
