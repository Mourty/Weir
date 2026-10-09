//! `weirctl export` and `weirctl import`: settings to and from files.
//!
//! The daemon reads and writes the files; it is given whole paths, since
//! its working folder is not this one. The window keeps its look in its
//! own `gui.toml`, so the window look is read from there to export, and
//! written there on import when no window is open to take it.

use crate::args::{ExportArgs, ImportArgs};
use crate::client::Client;
use crate::commands::print_json;
use crate::show;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use weir_protocol::*;

/// `path`, whole.
fn whole(path: &Path) -> Result<String> {
    let path = std::path::absolute(path).with_context(|| format!("{}", path.display()))?;
    Ok(path.display().to_string())
}

/// A word such as `keep-both` as one of the protocol's choices.
fn choice<T: serde::de::DeserializeOwned>(text: &str, what: &str, choices: &str) -> Result<T> {
    serde_json::from_value(Value::String(text.trim().replace('-', "_")))
        .map_err(|_| anyhow!("{what} is one of {choices}, not '{text}'"))
}

/// `FROM=TO` as a pair.
fn pair(text: &str, what: &str) -> Result<(String, String)> {
    match text.split_once('=') {
        Some((from, to)) if !from.trim().is_empty() && !to.trim().is_empty() => {
            Ok((from.trim().to_string(), to.trim().to_string()))
        }
        _ => bail!("{what} is written FROM=TO, not '{text}'"),
    }
}

/// The window's own preferences, from `gui.toml`.
fn gui_toml() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("weir")
        .join("gui.toml")
}

/// The window look as `gui.toml` has it, if the window ever saved one.
fn window_look() -> Option<Value> {
    let text = std::fs::read_to_string(gui_toml()).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    let look: serde_json::Map<String, Value> = table
        .into_iter()
        .filter(|(k, _)| WINDOW_LOOK_KEYS.contains(&k.as_str()))
        .filter_map(|(k, v)| serde_json::to_value(v).ok().map(|v| (k, v)))
        .collect();
    Some(Value::Object(look))
}

/// Save an imported window look in `gui.toml`, for the window's next
/// start.
fn save_window_look(look: &Value) -> Result<()> {
    let path = gui_toml();
    let mut table: toml::Table = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?,
        Err(_) => toml::Table::new(),
    };
    if let Value::Object(look) = look {
        for (k, v) in look {
            if WINDOW_LOOK_KEYS.contains(&k.as_str()) {
                if let Ok(v) = toml::Value::try_from(v) {
                    table.insert(k.clone(), v);
                }
            }
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, toml::to_string_pretty(&table)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// Write the settings `a` names to a file.
pub fn export(c: &mut Client, a: ExportArgs, json: bool) -> Result<()> {
    let preferences: Vec<PreferencePart> = a
        .preferences
        .iter()
        .map(|p| {
            choice(
                p,
                "a part of the preferences",
                "window-look, mixer, audio-timing and start-at-login",
            )
        })
        .collect::<Result<_>>()?;
    let wants_look = a.all || preferences.contains(&PreferencePart::WindowLook);
    let p = ExportParams {
        path: whole(&a.file)?,
        all: a.all,
        scenes: a.scenes,
        setups: a.setups,
        hotkeys: a
            .hotkeys
            .iter()
            .map(|h| match h.trim().parse::<HotkeyId>() {
                Ok(id) => HotkeyKey::Id(id),
                Err(_) => HotkeyKey::Name(h.clone()),
            })
            .collect(),
        eq_presets: a.eq_presets,
        app_rules: a.app_rules,
        preferences,
        window_look: wants_look.then(window_look).flatten(),
    };
    let result: ExportResult = serde_json::from_value(c.call(&Request::ExportSettings(p))?)?;
    if json {
        return print_json(&result);
    }
    match &result.files[..] {
        [one] if result.path.ends_with(one.as_str()) => println!("Saved {}", result.path),
        files => {
            println!("Saved {} files in {}:", files.len(), result.path);
            for f in files {
                println!("  {f}");
            }
        }
    }
    Ok(())
}

/// Show what a file holds, or import it.
pub fn import(c: &mut Client, a: ImportArgs, json: bool) -> Result<()> {
    let path = whole(&a.file)?;
    if a.list {
        let seen: ImportInspection =
            serde_json::from_value(c.call(&Request::InspectImport(InspectImportParams { path }))?)?;
        if json {
            return print_json(&seen);
        }
        show::import_inspection(&seen);
        return Ok(());
    }
    let mut choices = BTreeMap::new();
    for r in &a.rename {
        let (id, name) = pair(r, "--rename")?;
        choices.insert(id, ImportChoice::Rename(name));
    }
    let map = |pairs: &[String], what: &str| -> Result<BTreeMap<String, String>> {
        pairs.iter().map(|p| pair(p, what)).collect()
    };
    let p = ImportParams {
        path,
        items: (!a.only.is_empty()).then_some(a.only.clone()),
        choices,
        when_taken: choice(&a.taken, "--taken", "skip, replace and keep-both")?,
        hotkeys: if a.replace_hotkeys {
            HotkeyImport::ReplaceAll
        } else {
            HotkeyImport::Add
        },
        map_strips: map(&a.map_strips, "--map-strip")?,
        map_buses: map(&a.map_buses, "--map-bus")?,
    };
    let result: ImportResult = serde_json::from_value(c.call(&Request::ImportSettings(p))?)?;
    // With no window open to take it, the window finds its look next time.
    let saved_look = match (&result.window_look, result.window_told) {
        (Some(look), false) => {
            save_window_look(look)?;
            true
        }
        _ => false,
    };
    if json {
        return print_json(&result);
    }
    show::import_result(&result, saved_look);
    Ok(())
}
