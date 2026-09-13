//! Settings that live in the data directory, beside the memory they configure.
//!
//! Every model and embedding setting is an environment variable, and that was
//! fine for a server started from a terminal somebody had configured. It is not
//! fine for a server started by the operating system. A scheduled task, a
//! systemd unit or a launchd agent starts with an environment nobody chose, so
//! the settings have to be put there by something — and on the machine this
//! project is developed on that something grew into a PowerShell wrapper, a
//! VBScript to hide its window, and a second wrapper so the CLI could see the
//! same model the server did. None of it was in the repository. A server set up
//! from the documentation alone summarised every session by counting, and said
//! so only in a banner printed to a console nobody saw.
//!
//! So the settings can be written once, to `<data_dir>/settings.env`, and every
//! command reads them: the server, the MCP server a harness starts, `status`,
//! `reconsolidate`. The environment still wins, variable by variable, so a
//! shell that exports something for one run changes that run and nothing else.
//!
//! What the file refuses is as deliberate as what it holds. **No secrets**: a
//! key in a plain file sits unencrypted beside the memory, and goes wherever
//! anything that copies the directory takes it; the credential store exists for
//! that. The file is not in `anamnesis backup` either, for the reason `models/`
//! and `logs/` are not: an address like a local Ollama's is one machine's.
//! **No `ANAMNESIS_DATA_DIR`**: the file is found through the data directory, so
//! a line naming another one could only ever be ignored. A refused or unreadable line is reported, and `serve`
//! will not start with one, because a setting that was typed and silently not
//! applied is the failure this file is here to end.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anamnesis_core::datadir::DataDir;

/// The file's name inside the data directory.
pub const FILE: &str = "settings.env";

/// Variables the file will not hold, because they are secrets.
///
/// Matched by name rather than by looking at values: a value cannot say whether
/// it is a key, and a name can.
pub(crate) const SECRET_NAMES: &[&str] = &[
    "ANAMNESIS_LLM_API_KEY",
    "ANAMNESIS_EMBED_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "ANAMNESIS_TOKEN",
    "ANAMNESIS_TOKENS",
];

/// What was read from the file, and what could not be used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// Where the file is, whether or not it exists.
    pub path: PathBuf,
    /// Whether it exists.
    pub present: bool,
    /// Every line that was accepted, by name.
    pub values: BTreeMap<String, String>,
    /// Every line that was not, as a sentence naming the line.
    pub problems: Vec<String>,
}

static LOADED: OnceLock<Settings> = OnceLock::new();

/// Read the file for this run. Called once, before any command.
///
/// A data directory that cannot be resolved leaves nothing loaded; the command
/// that needs it will say why in its own terms.
pub fn init(data_dir: Option<PathBuf>) -> &'static Settings {
    LOADED.get_or_init(|| match DataDir::resolve(data_dir) {
        Ok(data) => read(&data.root().join(FILE)),
        Err(_) => Settings::default(),
    })
}

/// The settings loaded for this run, if [`init`] has been called.
pub fn loaded() -> Option<&'static Settings> {
    LOADED.get()
}

/// One setting: the environment's value, or else the file's, or — for a
/// secret — the credential store's.
///
/// The shape `LlmConfig::from_vars` and `EmbedConfig::from_vars` take, so every
/// command reads the same places in the same order.
pub fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| {
            LOADED
                .get()
                .and_then(|settings| settings.values.get(name).cloned())
        })
        // Last, and for secrets only: the file refuses them, so the store is
        // where one set for an unattended server lives.
        .or_else(|| {
            crate::keys::KEY_NAMES
                .contains(&name)
                .then(|| crate::keys::stored(name))
                .flatten()
        })
}

/// Read and check one settings file.
pub fn read(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let (values, problems) = parse(&text, path);
            Settings {
                path: path.to_path_buf(),
                present: true,
                values,
                problems,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings {
            path: path.to_path_buf(),
            ..Settings::default()
        },
        Err(error) => Settings {
            path: path.to_path_buf(),
            present: true,
            problems: vec![format!("{} could not be read: {error}", path.display())],
            ..Settings::default()
        },
    }
}

/// `NAME=value` lines, `#` comments and blank lines.
///
/// An optional `export ` in front, and one pair of matching quotes around the
/// value, so a file written for a shell reads the same here. Nothing else is
/// interpreted: no `$VAR` expansion, no escapes. A value is what is between the
/// `=` and the end of the line, and a setting that meant something else is one
/// to fix in the file rather than one this should guess at.
fn parse(text: &str, path: &Path) -> (BTreeMap<String, String>, Vec<String>) {
    let mut values = BTreeMap::new();
    let mut problems = Vec::new();
    let file = path.display();

    for (index, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            problems.push(format!("{file}:{number} is not NAME=value"));
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            problems.push(format!(
                "{file}:{number}: {name:?} is not a variable name (capitals, digits and _)"
            ));
            continue;
        }
        if SECRET_NAMES.contains(&name) {
            problems.push(format!(
                "{file}:{number}: {name} is a secret and this file is plain text; \
                 store it with `anamnesis key set {name}` and delete the line"
            ));
            continue;
        }
        if name == anamnesis_core::datadir::DATA_DIR_ENV {
            problems.push(format!(
                "{file}:{number}: {name} cannot be set here: this file is found through the data \
                 directory, so it could only ever name the one it is already in"
            ));
            continue;
        }
        let value = unquote(value.trim());
        if values.insert(name.to_owned(), value.to_owned()).is_some() {
            problems.push(format!(
                "{file}:{number}: {name} is set twice; the later line is the one used"
            ));
        }
    }
    (values, problems)
}

/// One pair of matching quotes, removed.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> (BTreeMap<String, String>, Vec<String>) {
        parse(text, Path::new("settings.env"))
    }

    #[test]
    fn a_file_written_for_a_shell_reads_the_same_here() {
        let (values, problems) = parsed(
            "\u{feff}# the model\n\
             ANAMNESIS_LLM_PROVIDER=google\n\
             export ANAMNESIS_LLM_MODEL=\"gemini-3.5-flash\"\n\
             \n\
             ANAMNESIS_LLM_FALLBACK_PROVIDERS = 'google:gemini-3.6-flash'\n\
             ANAMNESIS_EMBED_URL=http://127.0.0.1:11434/v1/embeddings\n",
        );

        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(values["ANAMNESIS_LLM_PROVIDER"], "google");
        assert_eq!(values["ANAMNESIS_LLM_MODEL"], "gemini-3.5-flash");
        assert_eq!(
            values["ANAMNESIS_LLM_FALLBACK_PROVIDERS"],
            "google:gemini-3.6-flash"
        );
        assert_eq!(
            values["ANAMNESIS_EMBED_URL"], "http://127.0.0.1:11434/v1/embeddings",
            "only the first = splits"
        );
    }

    /// A key in plain text beside the memory goes wherever the directory does.
    #[test]
    fn a_secret_is_refused_by_name_and_the_line_says_where_it_goes() {
        let (values, problems) = parsed("ANAMNESIS_LLM_API_KEY=AQ.secret\nGEMINI_API_KEY=x\n");

        assert!(values.is_empty(), "{values:?}");
        assert_eq!(problems.len(), 2);
        assert!(
            problems[0].contains("anamnesis key set ANAMNESIS_LLM_API_KEY"),
            "{}",
            problems[0]
        );
        assert!(
            problems
                .iter()
                .all(|problem| !problem.contains("AQ.secret")),
            "a refusal must not repeat the secret: {problems:?}"
        );
    }

    #[test]
    fn a_line_that_cannot_mean_anything_is_named_by_its_number() {
        let (values, problems) = parsed(
            "ANAMNESIS_LLM_MODEL\nanamnesis_llm_model=x\nANAMNESIS_DATA_DIR=/elsewhere\n\
             ANAMNESIS_LLM_MODEL=a\nANAMNESIS_LLM_MODEL=b\n",
        );

        assert_eq!(values["ANAMNESIS_LLM_MODEL"], "b");
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems[0].contains("settings.env:1"));
        assert!(problems[1].contains(":2"));
        assert!(problems[2].contains("ANAMNESIS_DATA_DIR"));
        assert!(problems[3].contains("set twice"));
    }

    #[test]
    fn no_file_is_no_settings_and_no_problem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = read(&dir.path().join(FILE));

        assert!(!settings.present);
        assert!(settings.values.is_empty());
        assert!(settings.problems.is_empty());
    }

    #[test]
    fn a_file_on_disk_is_read_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE);
        std::fs::write(
            &path,
            "ANAMNESIS_EMBED_ENABLED=1\r\nANAMNESIS_EMBED_MODEL=nomic-embed-text\r\n",
        )
        .expect("write");

        let settings = read(&path);

        assert!(settings.present);
        assert!(settings.problems.is_empty(), "{:?}", settings.problems);
        assert_eq!(settings.values["ANAMNESIS_EMBED_MODEL"], "nomic-embed-text");
    }
}
