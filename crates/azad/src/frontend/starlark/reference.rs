//! The reference of the Starlark built-ins, made from their docstrings. `docs/starlark.md` holds it, and a test checks
//! that the file is up to date.

use starlark::docs::markdown::LayoutRenderConfig;
use starlark::docs::markdown::render_doc_member;
use starlark::docs::multipage::RenderConfig;
use starlark::docs::{DocItem, DocMember, DocType};
use starlark::environment::GlobalsBuilder;
use starlark::typing::TypeRenderConfig;

use crate::frontend::starlark::builtins::{azad_globals, namespaces};
use crate::frontend::starlark::git::CommitValue;
use crate::frontend::starlark::semver::SemverValue;
use crate::frontend::starlark::values::ActionValue;

const INTRODUCTION: &str = "\
<!-- Made from the docstrings in crates/azad/src/frontend/starlark/; do not edit. \
To make it again: AZAD_UPDATE_REFERENCE=1 cargo test -p azad --test reference -->

# Starlark built-ins

A `tasks.star` file defines the actions and goals of its package: the directory that holds it.
Its label is `//<directory>`, such as `//services/orders`, and `//` for the root of the workspace.
`azad run //services/orders:check` runs the goal `check` of that package.
`azad run //services/...:check` runs the goal `check` of every package at or below `services` (decision 0023), and \
`azad run check` does the same for the current directory; with no package at or below it, it runs the goal of the \
package that holds it.
`--include` and `--exclude` choose among them by tags.

These functions are available in `tasks.star` files and in modules.
A module is a `.star` file that a `tasks.star` file loads, such as `load(\"//lib/go.star\", \"go_module\")`, or \
`load(\"@std//test.star\", \"split\")` for the standard library.
A module can define functions that declare actions, but it cannot declare actions when it is loaded.

`config` is a struct of the public names of the `config.star` files from the root of the workspace down to the \
package; a name in a deeper directory replaces the same name above it.
`tags` is the exception: the tags of every `config.star` from the root down to the package are added to each task and \
action of the package.

Paths:

- The patterns of `glob()` are relative to the package directory, and those of `files()` too, unless they start with \
`//`, which is the root of the workspace.
- `read()`, `workspace_path()` and `package_of()` take a path relative to the package directory, or one from the root \
after `//`.
- The keys of `inputs` and `caches` are absolute paths in the sandbox, such as `/src`.
- `outputs` are relative to `/work`, the directory in which the command starts.

Two namespaces give values that are not actions (decision 0021): `semver`, for semantic versions, and `git`, for the \
history of the workspace's repository.
`@std//version.star` builds versions on them: `commit_calver()` and `latest_release()`.

`@std//project.star` makes a project's goals from parts (decision 0025): `part()` puts tasks in roles, such as `fmt` or `test`, and `project()` makes a goal for each role, and the goal `ci`.

`read()` gives the text of a file of the workspace, and three namespaces make values of it, as Starlark's `json` does \
(decision 0031): `toml`, `yaml` and `xml`. With `files()`, `workspace_path()` and `package_of()`, modules of the \
standard library read project files: `@std//dotnet.star` (`project_info()`, `dependencies()` and the parts of a .NET \
project), `@std//python.star` and `@std//node.star` (`dependencies()` of a uv project and of a pnpm, npm or Yarn \
project), and `@std//paths.star` for `//` paths. A package takes them only when it loads them, as in \
`package(deps = dependencies(\"src/App/App.csproj\"))`.

A package can also be declared as data in `tasks.yaml`, which calls modules that return parts (decision 0027); `docs/yaml.md` describes it.";

/// The reference of the built-in functions, of the namespaces, and of the methods of the values they give, as Markdown.
pub fn reference() -> String {
    let config = RenderConfig { type_config: TypeRenderConfig::Default, layout_config: LayoutRenderConfig::Default };
    let globals = GlobalsBuilder::new().with(azad_globals).with(namespaces).build().documentation();
    let mut members: Vec<(String, DocMember)> = Vec::new();
    for (name, item) in &globals.members {
        match item {
            DocItem::Member(member) => members.push((name.clone(), member.clone())),
            DocItem::Module(namespace) => {
                let mut functions: Vec<(String, DocMember)> = namespace
                    .members
                    .iter()
                    .filter_map(|(function, item)| Some((format!("{name}.{function}"), item.try_as_member()?)))
                    .collect();
                functions.sort_by(|a, b| a.0.cmp(&b.0));
                members.extend(functions);
            }
            DocItem::Type(_) => {}
        }
    }
    for (value, doc) in [
        ("action", DocType::from_starlark_value::<ActionValue>()),
        ("semver", DocType::from_starlark_value::<SemverValue>()),
        ("commit", DocType::from_starlark_value::<CommitValue>()),
    ] {
        members.extend(doc.members.iter().map(|(name, member)| (format!("{value}.{name}"), member.clone())));
    }
    let sections: Vec<String> = members.iter().map(|(name, member)| render_doc_member(name, member, &config)).collect();
    tidy(&format!("{INTRODUCTION}\n\n{}\n", sections.join("\n\n---\n\n")))
}

/// starlark 0.14 shows a default that is not a literal, such as an empty dictionary, as `...`, which says only that
/// the parameter is optional; and it writes blank lines with spaces in them, and two or three blank lines in a row.
fn tidy(text: &str) -> String {
    const HIDDEN_DEFAULT: &str = "(defaults to: `...`)";
    const OPTIONAL: &str = "(optional)";
    let mut tidied = String::new();
    let mut previous_blank = false;
    for line in text.lines().map(|line| line.trim_end().replace(HIDDEN_DEFAULT, OPTIONAL)) {
        let blank = line.is_empty();
        if !(blank && previous_blank) {
            tidied.push_str(&line);
            tidied.push('\n');
        }
        previous_blank = blank;
    }
    tidied
}
