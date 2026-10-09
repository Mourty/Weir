//! `weirctl`: command line client for the Weir daemon.
//!
//! Every command is a request or two over the control socket; see
//! `docs/CLI.md`. Strips and buses can be given by id or by name, and buses
//! also by their label (`A1`, `B2`).
//!
//! * [`args`]: the commands and their options.
//! * [`client`]: the connection to the daemon.
//! * [`parse`]: reading what people type.
//! * [`commands`]: what each command does.
//! * [`show`]: what gets printed.

mod args;
mod client;
mod commands;
mod parse;
mod show;
mod transfer;

use anyhow::Result;
use args::{Cli, Cmd};
use clap::Parser;
use client::Client;
use commands::SettingsArgs;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let path = cli
        .socket
        .clone()
        .unwrap_or_else(weir_protocol::default_socket_path);
    let c = &mut Client::connect(&path)?;
    let json = cli.json;
    match cli.cmd {
        Cmd::Status => commands::status(c, json),
        Cmd::State => commands::state(c, json),
        Cmd::Devices => commands::devices(c, json),
        Cmd::Apps => commands::apps(c, json),
        Cmd::Strip(a) => commands::strip(c, a, json),
        Cmd::Bus(a) => commands::bus(c, a, json),
        Cmd::Route {
            strip,
            bus,
            action,
            level,
            level_by,
        } => commands::route(c, &strip, &bus, action.as_deref(), level, level_by, json),
        Cmd::AddStrip {
            name,
            kind,
            layout,
            device,
            routes,
        } => commands::add_strip(c, name, &kind, &layout, device, &routes, json),
        Cmd::RemoveStrip { strip } => commands::remove_strip(c, &strip),
        Cmd::AddBus {
            name,
            kind,
            layout,
            device,
        } => commands::add_bus(c, name, &kind, &layout, device, json),
        Cmd::RemoveBus { bus } => commands::remove_bus(c, &bus),
        Cmd::Rules => commands::rules(c, json),
        Cmd::Rule { app, strip } => commands::rule(c, app, &strip, json),
        Cmd::Unrule { app } => commands::unrule(c, &app, json),
        Cmd::MoveStrip { strip, position } => commands::move_strip(c, &strip, position, json),
        Cmd::MoveBus { bus, position } => commands::move_bus(c, &bus, position, json),
        Cmd::MoveApp { app, strip } => commands::move_app(c, app, &strip),
        Cmd::AppVolume {
            app,
            gain,
            gain_by,
            mute,
        } => commands::app_volume(c, app, gain, gain_by, mute.as_deref()),
        Cmd::Show => commands::show_window(c),
        Cmd::Undo { steps } => commands::step_history(c, steps, true, json),
        Cmd::Redo { steps } => commands::step_history(c, steps, false, json),
        Cmd::History => commands::history(c, json),
        Cmd::Settings {
            solo,
            rate,
            buffer,
            meter_rate,
            startup,
            start_at_login,
            tray,
            tray_icon,
        } => {
            let a = SettingsArgs {
                solo,
                rate,
                buffer,
                meter_rate,
                startup,
                start_at_login,
                tray,
                tray_icon,
            };
            commands::settings(c, a, json)
        }
        Cmd::Setup { action } => commands::library(c, action, false, json),
        Cmd::Scene { action } => commands::library(c, action, true, json),
        Cmd::Eq { action } => commands::eq(c, action, json),
        Cmd::Hotkeys => commands::hotkeys(c, json),
        Cmd::Hotkey { action } => commands::hotkey(c, action, json),
        Cmd::Export(a) => transfer::export(c, a, json),
        Cmd::Import(a) => transfer::import(c, a, json),
        Cmd::Watch { meters } => commands::watch(c, meters, json),
        Cmd::Raw { method, params } => commands::raw(c, &method, params.as_deref()),
    }
}
