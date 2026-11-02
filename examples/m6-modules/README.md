# M6: a module from Starlark and from GitHub Actions

`std/test.split` is a module of Azad's standard library (decision 0014): it makes one action per test file, so a change to one test runs only that test.

- `tasks.star` calls it from Starlark: `azad run //examples/m6-modules:example`.
- `.github/workflows/modules-example.yml` calls it from a GHA workflow with `uses: BadLiveware/azad/std/test-split@main`.
  GitHub runs the composite action in `std/test-split`; Azad recognizes the name and calls the module, and the job waits for its actions.
- Each test reads only its own file and `lib/`.
