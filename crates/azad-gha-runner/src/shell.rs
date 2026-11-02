//! The shells of `run:` steps, as GitHub runs them on Linux.

use std::path::Path;

use crate::error::Error;

const SCRIPT_PLACEHOLDER: &str = "{0}";
const DEFAULT_BASH: &str = "bash -e {0}";
const DEFAULT_SH: &str = "sh -e {0}";
const BASH: &str = "bash";
const BASH_TEMPLATE: &str = "bash --noprofile --norc -eo pipefail {0}";
const SH: &str = "sh";
const SH_TEMPLATE: &str = "sh -e {0}";
const PYTHON: &str = "python";
const PYTHON_TEMPLATE: &str = "python {0}";
const POWERSHELL_SHELLS: [&str; 2] = ["pwsh", "powershell"];
const CMD: &str = "cmd";
const SHELL_EXTENSION: &str = "sh";
const PYTHON_EXTENSION: &str = "py";

/// How to run a script file: a program, its arguments, and the extension that the script file gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellCommand {
    template: String,
    pub(crate) extension: Option<&'static str>,
}

impl ShellCommand {
    fn from_template(template: &str) -> Self {
        let program = template.split_whitespace().next().unwrap_or_default();
        Self { template: template.to_string(), extension: extension_of(program) }
    }

    /// The program and its arguments for a script file.
    pub(crate) fn command(&self, script: &Path) -> (String, Vec<String>) {
        let script = script.to_string_lossy();
        let mut words = self.template.split_whitespace().map(|word| word.replace(SCRIPT_PLACEHOLDER, &script));
        let program = words.next().unwrap_or_default();
        (program, words.collect())
    }
}

fn extension_of(program: &str) -> Option<&'static str> {
    let name = Path::new(program).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    match name.as_str() {
        BASH | SH => Some(SHELL_EXTENSION),
        name if name.starts_with(PYTHON) => Some(PYTHON_EXTENSION),
        _ => None,
    }
}

/// The shell of a step: its `shell:` (or the default one), and whether `bash` is on the step's `PATH`.
pub(crate) fn resolve_shell(shell: Option<&str>, bash_on_path: bool) -> Result<ShellCommand, Error> {
    let Some(shell) = shell.map(str::trim) else {
        return Ok(ShellCommand::from_template(default_template(bash_on_path)));
    };
    match shell {
        BASH => Ok(ShellCommand::from_template(BASH_TEMPLATE)),
        SH => Ok(ShellCommand::from_template(SH_TEMPLATE)),
        PYTHON => Ok(ShellCommand::from_template(PYTHON_TEMPLATE)),
        name if POWERSHELL_SHELLS.contains(&name) => Err(Error::new(format!(
            "shell `{name}` is not supported: Azad runs Linux images without PowerShell; use bash, sh or python"
        ))),
        CMD => Err(Error::new("shell `cmd` is only for Windows; Azad runs Linux")),
        template if template.contains(SCRIPT_PLACEHOLDER) => Ok(ShellCommand::from_template(template)),
        other => Err(Error::new(format!(
            "shell `{other}` is not known: use bash, sh, python, or a command with `{{0}}` for the script file"
        ))),
    }
}

fn default_template(bash_on_path: bool) -> &'static str {
    match bash_on_path {
        true => DEFAULT_BASH,
        false => DEFAULT_SH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(shell: Option<&str>, bash_on_path: bool) -> (String, Vec<String>) {
        resolve_shell(shell, bash_on_path).expect("known shell").command(Path::new("/tmp/s.sh"))
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_default_is_bash_when_bash_is_on_the_path() {
        assert_eq!(command(None, true), ("bash".to_string(), words("-e /tmp/s.sh")));
    }

    #[test]
    fn the_default_is_sh_without_bash() {
        assert_eq!(command(None, false), ("sh".to_string(), words("-e /tmp/s.sh")));
    }

    #[test]
    fn named_bash_has_pipefail_and_no_profile() {
        assert_eq!(
            command(Some("bash"), true),
            ("bash".to_string(), words("--noprofile --norc -eo pipefail /tmp/s.sh"))
        );
    }

    #[test]
    fn named_sh_and_python() {
        assert_eq!(command(Some("sh"), true), ("sh".to_string(), words("-e /tmp/s.sh")));
        assert_eq!(command(Some("python"), true), ("python".to_string(), words("/tmp/s.sh")));
    }

    #[test]
    fn a_template_puts_the_script_in_place_of_the_placeholder() {
        assert_eq!(command(Some("perl -w {0}"), true), ("perl".to_string(), words("-w /tmp/s.sh")));
    }

    #[test]
    fn extensions_follow_the_program() {
        let extension = |shell| resolve_shell(Some(shell), true).expect("known shell").extension;
        assert_eq!(extension("bash"), Some("sh"));
        assert_eq!(extension("python"), Some("py"));
        assert_eq!(extension("python3 -u {0}"), Some("py"));
        assert_eq!(extension("perl {0}"), None);
    }

    #[test]
    fn powershell_is_a_clear_error() {
        for shell in ["pwsh", "powershell"] {
            let error = resolve_shell(Some(shell), true).expect_err("not supported");
            assert!(error.to_string().contains("not supported"), "{error}");
        }
    }

    #[test]
    fn an_unknown_shell_without_a_placeholder_is_an_error() {
        assert!(resolve_shell(Some("zsh"), true).is_err());
        assert!(resolve_shell(Some("cmd"), true).is_err());
    }
}
