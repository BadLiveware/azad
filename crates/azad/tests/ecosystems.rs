//! Decision 0031: modules of the standard library read project files of .NET, Python and Node with generic
//! built-ins, and give the packages that a project needs, so that the dependency graph (decision 0028) comes from the
//! files that the tools read.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use azad::engine::{Engine, EngineConfig, Status, host_platform};
use azad::frontend::starlark::evaluate;
use azad::frontend::{NoteLevel, Package};
use azad::sandbox::Limits;
use serde_json::{Value, json};
use tempfile::TempDir;

const PLATFORM: &str = "linux/amd64";

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn workspace(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for (path, content) in files {
        write(dir.path(), path, content);
    }
    dir
}

fn evaluated(root: &Path, package: &str) -> Result<Package, String> {
    evaluate(root, package, PLATFORM).map_err(|error| error.to_string())
}

/// A `tasks.star` that shows a value as the environment of a task, so that a test can read it.
fn showing(load: &str, value: &str) -> String {
    format!("{load}\ntask(name = \"show\", cmd = \"true\", env = {{\"VALUE\": json.encode({value})}})\n")
}

fn shown(root: &Path, package: &str) -> Value {
    let package_label = format!("//{package}:show");
    let package = evaluated(root, package).unwrap();
    serde_json::from_str(&package.tasks[&package_label].env["VALUE"]).unwrap()
}

const DOTNET: &str = "load(\"@std//dotnet.star\", \"dependencies\", \"project_info\")";
const PYTHON: &str = "load(\"@std//python.star\", \"dependencies\")";
const NODE: &str = "load(\"@std//node.star\", \"dependencies\")";

const ROOT_PROPS: &str = r#"<Project>
  <PropertyGroup>
    <DefaultTargetFramework>net8.0</DefaultTargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <AdditionalFiles Include="$(MSBuildThisFileDirectory)stylecop.json" />
    <PackageReference Include="StyleCop.Analyzers" Version="$(StyleCopVersion)" />
  </ItemGroup>
</Project>
"#;

const LIBS_PROPS: &str = r#"<Project>
  <Import Project="$([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildThisFileDirectory)../'))" />
  <PropertyGroup>
    <IsPackable>false</IsPackable>
    <TargetFramework>$(DefaultTargetFramework)</TargetFramework>
  </PropertyGroup>
</Project>
"#;

/// An SDK-style web project with references in Windows form, under a Condition, into its own package, and a wildcard
/// `Compile` include of another package's files.
const API: &str = r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <PropertyGroup>
    <TargetFrameworks>net8.0;net48</TargetFrameworks>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="..\..\..\..\libs\core\src\Core\Core.csproj" />
    <ProjectReference Include="..\Api.Contracts\Api.Contracts.csproj" />
    <PackageReference Include="Serilog" Version="4.0.0" />
    <Compile Include="../../../../libs/orders/src/Orders.TestBuilders/*.cs" />
  </ItemGroup>
  <ItemGroup Condition="'$(TargetFramework)' == 'net48'">
    <ProjectReference Include="$(MSBuildThisFileDirectory)../../../../libs/legacy/Legacy.csproj" />
  </ItemGroup>
</Project>
"#;

fn dotnet_workspace() -> TempDir {
    workspace(&[
        ("Directory.Build.props", ROOT_PROPS),
        ("stylecop.json", "{}\n"),
        ("libs/Directory.Build.props", LIBS_PROPS),
        ("libs/core/tasks.star", ""),
        (
            "libs/core/src/Core/Core.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><IsPackable>true</IsPackable></PropertyGroup></Project>",
        ),
        (
            "libs/core/tests/Core.Tests.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk"><ItemGroup><PackageReference Include="Microsoft.NET.Test.Sdk" Version="17.11.0" /></ItemGroup></Project>"#,
        ),
        ("libs/orders/tasks.star", ""),
        ("libs/orders/src/Orders.TestBuilders/OrderBuilder.cs", "class OrderBuilder {}\n"),
        ("libs/legacy/tasks.star", ""),
        ("libs/legacy/Legacy.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />"),
        ("apps/api/src/Api/Api.csproj", API),
        ("apps/api/src/Api.Contracts/Api.Contracts.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />"),
        (
            "apps/tool/Tool.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><OutputType>Exe</OutputType></PropertyGroup></Project>",
        ),
    ])
}

#[test]
fn dotnet_dependencies_are_the_packages_of_references_and_includes_under_every_condition() {
    let dir = dotnet_workspace();
    let root = dir.path();
    write(root, "apps/api/tasks.star", &format!("{DOTNET}\npackage(deps = dependencies(\"src/Api/Api.csproj\"))\n"));
    let package = evaluated(root, "apps/api").unwrap();
    assert_eq!(package.declared.deps, ["//libs/core", "//libs/legacy", "//libs/orders"]);

    let read: Vec<&str> = package.read_files.keys().map(String::as_str).collect();
    assert_eq!(read, ["//Directory.Build.props", "//apps/api/src/Api/Api.csproj"]);
    let digest = package.read_files["//apps/api/src/Api/Api.csproj"];
    assert_eq!(digest, azad::ir::Digest::of_bytes(API.as_bytes()));
    let note = format!("read: //apps/api/src/Api/Api.csproj is {digest}");
    assert!(package.notes.iter().any(|n| n.level == NoteLevel::Diagnostic && n.text == note), "{:?}", package.notes);
}

#[test]
fn dotnet_project_info_inherits_directory_build_props_through_the_chain() {
    let dir = dotnet_workspace();
    let root = dir.path();
    let projects = [
        "//libs/core/src/Core/Core.csproj",
        "//libs/core/tests/Core.Tests.csproj",
        "//apps/api/src/Api/Api.csproj",
        "//apps/tool/Tool.csproj",
    ];
    let infos: Vec<String> = projects.iter().map(|path| format!("project_info({path:?})")).collect();
    write(root, "tasks.star", &showing(DOTNET, &format!("[{}]", infos.join(", "))));
    let shown = shown(root, "");
    let summary = |info: &Value| {
        json!([info["name"], info["kind"], info["packable"], info["frameworks"], info["references"], info["includes"]])
    };
    let summaries: Vec<Value> = shown.as_array().unwrap().iter().map(summary).collect();
    assert_eq!(
        summaries,
        [
            json!(["Core", "library", true, ["net8.0"], [], ["//stylecop.json"]]),
            json!(["Core.Tests", "test", false, ["net8.0"], [], ["//stylecop.json"]]),
            json!([
                "Api",
                "web",
                false,
                ["net48", "net8.0"],
                [
                    "//apps/api/src/Api.Contracts/Api.Contracts.csproj",
                    "//libs/core/src/Core/Core.csproj",
                    "//libs/legacy/Legacy.csproj"
                ],
                ["//libs/orders/src/Orders.TestBuilders/*.cs", "//stylecop.json"]
            ]),
            json!(["Tool", "exe", true, [], [], ["//stylecop.json"]]),
        ]
    );
    assert_eq!(
        shown[0]["files"],
        json!(["//libs/Directory.Build.props", "//Directory.Build.props", "//libs/core/src/Core/Core.csproj"])
    );
}

#[test]
fn dotnet_fails_and_says_why_when_it_cannot_know_a_dependency() {
    let cases = [
        (
            r#"<Project Sdk="Microsoft.NET.Sdk"><ItemGroup><ProjectReference Include="$(RepoRoot)libs/core/Core.csproj" /></ItemGroup></Project>"#,
            vec![
                "//apps/api/Api.csproj: <ProjectReference Include=\"$(RepoRoot)libs/core/Core.csproj\">: $(RepoRoot) is not set",
                "declare the dependency in package(deps = ...) by hand",
            ],
        ),
        (
            r#"<Project Sdk="Microsoft.NET.Sdk"><ItemGroup><ProjectReference Include="../../tools/Gen/Gen.csproj" /></ItemGroup></Project>"#,
            vec!["<ProjectReference Include=\"../../tools/Gen/Gen.csproj\">: no package holds //tools/Gen/Gen.csproj"],
        ),
        (
            r#"<Project Sdk="Microsoft.NET.Sdk"><Import Project="$(RepoRoot)build\common.props" /></Project>"#,
            vec![
                "//apps/api/Api.csproj: <Import Project=\"$(RepoRoot)build\\common.props\">",
                "cannot know what this import adds",
            ],
        ),
        (
            r#"<Project Sdk="Microsoft.NET.Sdk"><ItemGroup><Compile Include="$([System.IO.Path]::GetFullPath('x'))/*.cs" /></ItemGroup></Project>"#,
            vec!["is a property function, which the module does not run"],
        ),
    ];
    for (project, expected) in cases {
        let dir = workspace(&[
            ("apps/api/Api.csproj", project),
            ("apps/api/tasks.star", &format!("{DOTNET}\npackage(deps = dependencies(\"Api.csproj\"))\n")),
            ("tools/Gen/Gen.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />"),
        ]);
        let error = evaluated(dir.path(), "apps/api").unwrap_err();
        for text in expected {
            assert!(error.contains(text), "{project}\nexpected {text:?} in:\n{error}");
        }
    }
}

#[test]
fn dotnet_standard_parts_choose_tasks_from_what_the_project_says() {
    let dir = dotnet_workspace();
    let root = dir.path();
    write(root, "libs/core/global.json", "{}\n");
    let standard = "load(\"@std//dotnet.star\", \"standard\")\nload(\"@std//project.star\", \"project\")\n";
    write(root, "libs/core/tasks.star", &format!("{standard}project(standard(project = \"src/Core/Core.csproj\"))\n"));
    let package = evaluated(root, "libs/core").unwrap();
    let names: Vec<&str> = package.tasks.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "//libs/core:dotnet-build",
            "//libs/core:dotnet-format",
            "//libs/core:dotnet-format-check",
            "//libs/core:dotnet-pack"
        ]
    );
    let build = &package.tasks["//libs/core:dotnet-build"];
    assert_eq!(build.command, ["dotnet", "build", "src/Core/Core.csproj"]);
    assert_eq!(build.sources, ["**", "//Directory.Build.props", "//libs/Directory.Build.props", "//stylecop.json"]);
    assert_eq!(package.goals["//libs/core:ci"].len(), 2);

    write(root, "apps/api/tasks.star", &format!("{standard}project(standard(project = \"src/Api/Api.csproj\"))\n"));
    let package = evaluated(root, "apps/api").unwrap();
    let names: Vec<&str> = package.tasks.keys().map(String::as_str).collect();
    assert_eq!(names, ["//apps/api:dotnet-build", "//apps/api:dotnet-format", "//apps/api:dotnet-format-check"]);
}

const UV_ROOT: &str = r#"
[tool.uv.workspace]
members = ["packages/*"]
exclude = ["packages/legacy"]

[tool.uv.sources]
shared = { workspace = true }
unused = { workspace = true }
"#;

const UV_APP: &str = r#"
[project]
name = "app"
dependencies = ["Core_Lib>=1.0", "shared", "requests>=2"]

[dependency-groups]
dev = ["cli-tools"]

[tool.uv.sources]
core-lib = { workspace = true }
cli-tools = { path = "../../tools/cli", editable = true }
requests = { git = "https://example.org/requests.git" }
"#;

fn uv_workspace(app: &str) -> TempDir {
    let project = |name: &str| format!("[project]\nname = \"{name}\"\n");
    workspace(&[
        ("pyproject.toml", UV_ROOT),
        ("packages/app/pyproject.toml", app),
        ("packages/app/tasks.star", &format!("{PYTHON}\npackage(deps = dependencies())\n")),
        ("packages/core-lib/pyproject.toml", &project("core.lib")),
        ("packages/core-lib/tasks.star", ""),
        ("packages/shared/pyproject.toml", &project("shared")),
        ("packages/shared/tasks.star", ""),
        ("packages/unused/pyproject.toml", &project("unused")),
        ("packages/unused/tasks.star", ""),
        ("packages/legacy/pyproject.toml", &project("legacy")),
        ("packages/legacy/tasks.star", ""),
        ("tools/cli/pyproject.toml", &project("cli-tools")),
        ("tools/cli/tasks.star", ""),
    ])
}

#[test]
fn python_dependencies_are_path_sources_and_members_of_the_uv_workspace() {
    let dir = uv_workspace(UV_APP);
    let package = evaluated(dir.path(), "packages/app").unwrap();
    assert_eq!(package.declared.deps, ["//packages/core-lib", "//packages/shared", "//tools/cli"]);
}

#[test]
fn python_fails_for_a_member_that_the_workspace_does_not_have() {
    let app =
        "[project]\nname = \"app\"\ndependencies = [\"legacy\"]\n[tool.uv.sources]\nlegacy = { workspace = true }\n";
    let dir = uv_workspace(app);
    let error = evaluated(dir.path(), "packages/app").unwrap_err();
    assert!(
        error.contains("//packages/app/pyproject.toml: [tool.uv.sources] legacy: the uv workspace at //pyproject.toml has no member named \"legacy\""),
        "{error}"
    );
}

#[test]
fn node_dependencies_in_a_pnpm_workspace_are_paths_and_workspace_packages() {
    let manifest = r#"{
        "name": "@acme/web",
        "dependencies": {"@acme/ui": "workspace:*", "lodash": "^4.17.21"},
        "devDependencies": {"local-lib": "file:../../vendor/local-lib"},
        "optionalDependencies": {"utils": "workspace:@acme/utils@^1.0.0"}
    }"#;
    let dir = workspace(&[
        ("pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n  - '!packages/ignored'\n"),
        ("packages/web/package.json", manifest),
        ("packages/web/tasks.star", &format!("{NODE}\npackage(deps = dependencies())\n")),
        ("packages/ui/package.json", r#"{"name": "@acme/ui"}"#),
        ("packages/ui/tasks.star", ""),
        ("packages/utils/package.json", r#"{"name": "@acme/utils"}"#),
        ("packages/utils/tasks.star", ""),
        ("packages/ui/node_modules/lodash/package.json", r#"{"name": "lodash"}"#),
        ("packages/ignored/package.json", r#"{"name": "ignored"}"#),
        ("vendor/local-lib/package.json", r#"{"name": "local-lib"}"#),
        ("vendor/tasks.yaml", "tasks:\n  noop:\n    cmd: \"true\"\n"),
    ]);
    let root = dir.path();
    let package = evaluated(root, "packages/web").unwrap();
    assert_eq!(package.declared.deps, ["//packages/ui", "//packages/utils", "//vendor"]);

    write(root, "packages/web/package.json", r#"{"dependencies": {"ignored": "workspace:^"}}"#);
    let error = evaluated(root, "packages/web").unwrap_err();
    let expected = "//packages/web/package.json: dependencies[\"ignored\"] is \"workspace:^\", but the workspace of //pnpm-workspace.yaml has no package named \"ignored\"";
    assert!(error.contains(expected), "{error}");
}

#[test]
fn node_dependencies_in_an_npm_workspace_are_its_packages_by_name() {
    for workspaces in [r#"["apps/*", "libs/*"]"#, r#"{"packages": ["apps/*", "libs/*"]}"#] {
        let dir = workspace(&[
            ("package.json", &format!(r#"{{"name": "root", "private": true, "workspaces": {workspaces}}}"#)),
            ("apps/site/package.json", r#"{"dependencies": {"shared-ui": "^1.0.0", "react": "^18.0.0"}}"#),
            ("apps/site/tasks.star", &format!("{NODE}\npackage(deps = dependencies())\n")),
            ("libs/shared-ui/package.json", r#"{"name": "shared-ui", "version": "1.2.0"}"#),
            ("libs/shared-ui/tasks.star", ""),
        ]);
        let package = evaluated(dir.path(), "apps/site").unwrap();
        assert_eq!(package.declared.deps, ["//libs/shared-ui"], "{workspaces}");
    }
}

#[test]
fn files_of_the_workspace_read_and_package_of_work_from_the_root() {
    let dir = workspace(&[
        ("libs/core/tasks.star", ""),
        ("libs/core/a.txt", "a"),
        ("libs/core/sub/b.txt", "b"),
        ("libs/web/c.txt", "\u{feff}c"),
        ("apps/app/tasks.star", ""),
    ]);
    let root = dir.path();
    let value = "[files([\"//libs/*/*.txt\"]), files([\"//libs/**/*.txt\"], exclude = [\"//libs/web/**\"]), \
                 read(\"../../libs/web/c.txt\"), workspace_path(\"../x/../y\"), package_of(\"//libs/core/sub/b.txt\"), \
                 package_of(\"//libs/web\"), package_of(\".\")]";
    write(root, "apps/app/tasks.star", &showing("", value));
    assert_eq!(
        shown(root, "apps/app"),
        json!([
            ["//libs/core/a.txt", "//libs/web/c.txt"],
            ["//libs/core/a.txt", "//libs/core/sub/b.txt"],
            "c",
            "//apps/y",
            "//libs/core",
            null,
            "//apps/app"
        ])
    );
}

#[test]
fn reading_a_file_that_is_missing_outside_or_not_text_fails_and_names_it() {
    let dir = workspace(&[("app/tasks.star", ""), ("app/binary.bin", "")]);
    let root = dir.path();
    fs::write(root.join("app/binary.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret.txt"), "not of the workspace\n").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("app/outside")).unwrap();
    let cases = [
        ("read(\"missing.txt\")", "read(): //app/missing.txt does not exist"),
        ("read(\"/etc/hostname\")", "is absolute"),
        ("read(\"../../etc/hostname\")", "leaves the workspace"),
        ("read(\"outside\")", "read(): //app/outside is a link to a file outside the workspace"),
        ("read(\"binary.bin\")", "read(): //app/binary.bin is not UTF-8 text"),
        ("files([\"//a/*\", \"b/*\"])", "the patterns of one call are all of the package, or all of the workspace"),
    ];
    for (call, expected) in cases {
        write(root, "app/tasks.star", &format!("{call}\n"));
        let error = evaluated(root, "app").unwrap_err();
        assert!(error.contains(expected), "{call}: {error}");
    }
}

fn engine(root: &Path, store: &Path) -> Engine {
    Engine::new(EngineConfig {
        store: store.to_path_buf(),
        root: root.to_path_buf(),
        platform: host_platform(),
        limits: Limits { timeout: Duration::from_secs(30), ..Limits::default() },
        tools: Default::default(),
        isolated: false,
        color: false,
        force: false,
        sandbox: azad::SandboxKind::Bubblewrap,
        executor: None,
        trust: azad::Trust::Main,
        history: None,
        withheld: Default::default(),
        filter: Default::default(),
        local: None,
    })
    .unwrap()
}

/// What each task did in a run of `//apps/api:check`: "ran" or "cached".
fn run(engine: &Engine) -> BTreeMap<String, &'static str> {
    let goal = engine.load("//apps/api:check", &mut |_| {}).unwrap();
    let report = engine.run(&goal, 1, &mut |_| {}).unwrap();
    assert!(report.outcome.goal_holds(), "{:?}", report.outcome.statuses);
    let what = |status: &Status| match status {
        Status::Ran { .. } => "ran",
        Status::Cached { .. } => "cached",
        other => panic!("{other:?}"),
    };
    report.outcome.statuses.iter().map(|(label, status)| (label.clone(), what(status))).collect()
}

/// `apps/api` takes its dependencies from its project file, and a task that reads them gets the files of the
/// referenced project's package, and of the package that its wildcard `Compile` include names, and runs again when
/// they change (decision 0028).
#[test]
fn dependencies_from_a_project_file_feed_the_dependency_graph() {
    let dir = dotnet_workspace();
    let root = dir.path();
    let status = std::process::Command::new("git").args(["init", "-q"]).current_dir(root).status().unwrap();
    assert!(status.success());
    fs::write(root.join(".gitignore"), "out/\n").unwrap();
    write(root, "libs/core/core.txt", "core\n");
    write(
        root,
        "apps/api/tasks.star",
        &format!(
            r#"{DOTNET}
package(deps = dependencies("src/Api/Api.csproj"))
use = task(
    name = "use",
    cmd = "mkdir -p out && cat ../../libs/core/core.txt ../../libs/orders/src/Orders.TestBuilders/OrderBuilder.cs > out/seen.txt",
    generates = ["out"],
    dependencies = True,
)
goal("check", require = [use])
"#
        ),
    );
    let store = TempDir::new().unwrap();
    let engine = engine(root, store.path());
    let use_task = |what: &'static str| BTreeMap::from([("//apps/api:use".to_owned(), what)]);
    assert_eq!(run(&engine), use_task("ran"));
    assert_eq!(fs::read_to_string(root.join("apps/api/out/seen.txt")).unwrap(), "core\nclass OrderBuilder {}\n");
    assert_eq!(run(&engine), use_task("cached"));
    write(root, "libs/core/core.txt", "core, changed\n");
    assert_eq!(run(&engine), use_task("ran"));
    write(root, "libs/orders/src/Orders.TestBuilders/OrderBuilder.cs", "class OrderBuilder { int changed; }\n");
    assert_eq!(run(&engine), use_task("ran"));
}

/// An import by a plain path is read as MSBuild reads it, relative to the importing file, and adds what it names (an
/// item in it is relative to the project, so a shared file starts its paths with `$(MSBuildThisFileDirectory)`); an
/// import of a file that does not exist adds nothing; an item in NuGet's folder of packages names no file of the
/// workspace, so it is neither a dependency nor an error.
#[test]
fn dotnet_follows_imports_by_path_and_leaves_out_nuget_s_folder() {
    let dir = workspace(&[
        (
            "build/Shared.targets",
            "<Project><ItemGroup><ProjectReference Include=\"$(MSBuildThisFileDirectory)../libs/core/Core.csproj\" /></ItemGroup></Project>",
        ),
        ("libs/core/tasks.star", ""),
        ("libs/core/Core.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />"),
        (
            "apps/api/Api.csproj",
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <Import Project="..\..\build\Shared.targets" />
  <Import Project="$(MSBuildThisFileDirectory)missing.props" />
  <ItemGroup>
    <None Include="$(NuGetPackageRoot)some.package/1.0.0/lib/netstandard2.0/Some.dll" Link="Baseline/Some.dll" />
    <None Include="$(PkgSome_Package)/tools/tool.exe" />
  </ItemGroup>
</Project>"#,
        ),
        ("apps/api/tasks.star", &format!("{DOTNET}\npackage(deps = dependencies(\"Api.csproj\"))\n")),
    ]);
    let package = evaluated(dir.path(), "apps/api").unwrap();
    assert_eq!(package.declared.deps, ["//libs/core"]);
}
