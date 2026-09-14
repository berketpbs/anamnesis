//! Model keys, kept in the operating system's credential store.
//!
//! A server started at login needs its key from somewhere, and every place that
//! was available had a cost. The environment of a scheduled task is empty.
//! `settings.env` is plain text and refuses keys for that reason. On the machine
//! this project is developed on, the answer was a DPAPI-encrypted file and two
//! PowerShell scripts to decrypt it — one for the server, one so the CLI could
//! see the same model — and none of that was in the repository.
//!
//! The operating system already keeps secrets per account: Credential Manager
//! on Windows, the Keychain on macOS. `anamnesis key set` puts a key there under
//! the name of the variable it stands for, and [`crate::settings::var`] looks
//! there last, after the environment and the settings file, for those names
//! only. So a key set once is found by the server, by the MCP server a harness
//! starts, and by `reconsolidate`, and a key exported in a shell still wins for
//! that shell.
//!
//! Linux has no store that fits: Secret Service needs D-Bus to build and a
//! running keyring to use, which servers and containers do not have, and kernel
//! keyutils forgets at reboot. There the command says so, and the environment —
//! a systemd `EnvironmentFile` readable only by its owner — is the answer.

/// The names a key can be stored under: every model and embedding key
/// anamnesis reads.
///
/// Not the server tokens. `ANAMNESIS_TOKEN` is read by the hook, which runs on
/// every tool call and must not wait on a credential store, and a stored token
/// nothing reads would be a setting that says it applied and did not.
pub const KEY_NAMES: &[&str] = &[
    "ANAMNESIS_LLM_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENAI_API_KEY",
    "ANAMNESIS_EMBED_API_KEY",
];

/// What the credential store entries are filed under, unless
/// [`SERVICE_ENV`] names something else.
const SERVICE: &str = "anamnesis";

/// Files entries under another service name.
///
/// For two installs on one account that must not share keys, and for trying
/// the command without touching the entries a running server reads.
const SERVICE_ENV: &str = "ANAMNESIS_KEY_SERVICE";

fn service() -> String {
    std::env::var(SERVICE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| SERVICE.to_owned())
}

#[cfg(any(windows, target_os = "macos"))]
mod store {
    #[cfg(windows)]
    pub const NAME: &str = "Windows Credential Manager";
    #[cfg(target_os = "macos")]
    pub const NAME: &str = "the macOS Keychain";
    pub const AVAILABLE: bool = true;

    fn entry(service: &str, name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(service, name).map_err(|error| error.to_string())
    }

    pub fn get(service: &str, name: &str) -> Result<Option<String>, String> {
        match entry(service, name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn set(service: &str, name: &str, value: &str) -> Result<(), String> {
        entry(service, name)?
            .set_password(value)
            .map_err(|error| error.to_string())
    }

    pub fn delete(service: &str, name: &str) -> Result<bool, String> {
        match entry(service, name)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod store {
    pub const NAME: &str = "no credential store";
    pub const AVAILABLE: bool = false;

    const UNAVAILABLE: &str = "this platform has no credential store anamnesis uses";

    pub fn get(_: &str, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    pub fn set(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err(UNAVAILABLE.to_owned())
    }

    pub fn delete(_: &str, _: &str) -> Result<bool, String> {
        Err(UNAVAILABLE.to_owned())
    }
}

/// The stored value for a secret's name, for [`crate::settings::var`].
///
/// A store that cannot be read is said on stderr and read as nothing: the
/// command asking wanted a setting, and a model it cannot reach is something
/// every command already knows how to report.
pub fn stored(name: &str) -> Option<String> {
    match store::get(&service(), name) {
        Ok(value) => value.filter(|value| !value.trim().is_empty()),
        Err(error) => {
            tracing::warn!(%error, name, "the credential store could not be read");
            None
        }
    }
}

/// Refuse a name that is not one anamnesis reads a secret from.
fn checked(name: &str) -> anyhow::Result<&'static str> {
    KEY_NAMES
        .iter()
        .find(|known| **known == name)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{name} is not a key anamnesis reads from the store; one of: {}",
                KEY_NAMES.join(", ")
            )
        })
}

fn no_store(name: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "there is no credential store on this platform. Put {name} in the environment the \
         server starts in instead — for a systemd unit, an EnvironmentFile readable only by \
         its owner"
    )
}

/// `anamnesis key set NAME [--stdin]`.
pub fn cmd_key_set(name: &str, from_stdin: bool) -> anyhow::Result<()> {
    let name = checked(name)?;
    if !store::AVAILABLE {
        return Err(no_store(name));
    }

    // Typed without an echo, or read from a pipe. Never from an argument: an
    // argument is in the shell's history and in every process listing.
    let value = if from_stdin {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    } else {
        rpassword::prompt_password(format!("{name} (not shown as you type): "))?
    };
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("nothing was given, so nothing was stored");
    }

    store::set(&service(), name, value).map_err(|error| anyhow::anyhow!(error))?;

    println!("🔑 {name} stored in {}", store::NAME);
    println!();
    // The length and nothing else: enough to notice a paste that caught a
    // stray line, and nothing that narrows the key down.
    println!(
        "  {} characters, under service {:?}.",
        value.chars().count(),
        service()
    );
    println!("  Every anamnesis command reads it when {name} is not in its environment.");
    println!("  `anamnesis key check` asks the model with it, before a server depends on it.");
    println!("  A running server read its settings when it started: restart it to use this.");
    if std::env::var(name).is_ok() {
        println!();
        println!("  {name} is also set in this shell, and the environment wins here.");
    }
    Ok(())
}

/// `anamnesis key list`: which names have a value, and where from.
pub fn cmd_key_list() -> anyhow::Result<()> {
    println!("🔑 Keys ({})", store::NAME);
    println!();
    let service = service();
    let mut any = false;
    for name in KEY_NAMES {
        let in_env = std::env::var(name).is_ok();
        let in_store = match store::get(&service, name) {
            Ok(value) => value.is_some(),
            Err(error) => {
                println!("  {name:<24} the store could not be read: {error}");
                continue;
            }
        };
        let place = match (in_env, in_store) {
            (false, false) => continue,
            (true, false) => "this shell's environment",
            (false, true) => "stored",
            (true, true) => "stored, and this shell's environment (which wins here)",
        };
        any = true;
        println!("  {name:<24} {place}");
    }
    if !any {
        println!("  None stored, and none in this shell's environment.");
    }
    Ok(())
}

/// `anamnesis key forget NAME`.
pub fn cmd_key_forget(name: &str) -> anyhow::Result<()> {
    let name = checked(name)?;
    if !store::AVAILABLE {
        return Err(no_store(name));
    }
    let removed = store::delete(&service(), name).map_err(|error| anyhow::anyhow!(error))?;
    if removed {
        println!("🔑 {name} removed from {}.", store::NAME);
        println!("  A running server keeps the key it started with until it restarts.");
    } else {
        println!("🔑 {name} was not stored; nothing to remove.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the store serves is one the settings file refuses, so a key
    /// can never be in both places and disagree.
    #[test]
    fn every_storable_name_is_one_the_settings_file_refuses() {
        for name in KEY_NAMES {
            assert!(
                crate::settings::SECRET_NAMES.contains(name),
                "{name} is storable but settings.env would accept it"
            );
        }
    }

    #[test]
    fn only_a_name_anamnesis_reads_a_secret_from_is_accepted() {
        assert_eq!(checked("GEMINI_API_KEY").expect("known"), "GEMINI_API_KEY");
        let refused = checked("ANAMNESIS_LLM_MODEL").expect_err("not a secret");
        assert!(
            refused.to_string().contains("ANAMNESIS_LLM_API_KEY"),
            "{refused}"
        );
        assert!(
            checked("ANAMNESIS_TOKEN").is_err(),
            "the hook reads the token from its environment, never from a store"
        );
    }
}
