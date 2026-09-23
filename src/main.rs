//! OpenDeck strongSwan plugin — native Rust.
//!
//! Launched by OpenDeck, it speaks the OpenAction WebSocket protocol through the
//! `openaction` crate and drives charon over its VICI socket through `rsvici`.
//! `opendeck-strongswan --status` prints what the plugin sees of charon and exits.

mod actions;
mod exclusive;
mod vici;
mod vpn;

use actions::connection::{self, Bindings, ConnectionAction};
use openaction::global_events::*;
use openaction::*;

use crate::vpn::Vpn;

struct Global {
	vpn: Vpn,
}

#[async_trait]
impl GlobalEventHandler for Global {
	async fn plugin_ready(&self) -> OpenActionResult<()> {
		get_global_settings().await
	}

	async fn did_receive_global_settings(
		&self,
		event: DidReceiveGlobalSettingsEvent,
	) -> OpenActionResult<()> {
		match serde_json::from_value(event.payload.settings) {
			Ok(settings) => exclusive::store(settings),
			Err(error) => log::warn!("unreadable global settings, ignored: {error}"),
		}
		log::info!("exclusive pairs: {:?}", exclusive::get().exclusive);
		Ok(())
	}

	async fn system_did_wake_up(&self, _event: SystemDidWakeUpEvent) -> OpenActionResult<()> {
		self.vpn.kick();
		Ok(())
	}
}

#[tokio::main]
async fn main() -> OpenActionResult<()> {
	if std::env::args().nth(1).as_deref() == Some("--status") {
		status().await;
		return Ok(());
	}

	// Logs go to stdout, which OpenDeck redirects into the plugin log file
	// (~/.config/opendeck/logs/plugins/<uuid>.log). UTC, to the millisecond.
	{
		use simplelog::*;
		let config = ConfigBuilder::new()
			.set_time_format_custom(format_description!(
				"[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
			))
			.build();
		if let Err(error) = TermLogger::init(
			LevelFilter::Info,
			config,
			TerminalMode::Stdout,
			ColorChoice::Never,
		) {
			eprintln!("Logger initialization failed: {error}");
		}
	}

	let vpn = Vpn::default();
	let bindings = Bindings::default();

	set_global_event_handler(Box::leak(Box::new(Global { vpn: vpn.clone() })));
	register_action(ConnectionAction {
		vpn: vpn.clone(),
		bindings: bindings.clone(),
	})
	.await;

	tokio::spawn(vpn::monitor(vpn.clone()));
	{
		let vpn = vpn.clone();
		tokio::spawn(async move {
			let mut changes = vpn.subscribe();
			while changes.changed().await.is_ok() {
				connection::refresh_all(&vpn, &bindings).await;
			}
		});
	}

	run(std::env::args().collect()).await
}

/// Print the connections and SAs charon reports, the way the keys read them.
async fn status() {
	let mut client = match vici::connect().await {
		Ok(client) => client,
		Err(error) => {
			eprintln!("{}: {error}", vici::SOCKET);
			std::process::exit(1);
		}
	};
	let conns = vici::list_conns(&mut client).await;
	let sas = vici::list_sas(&mut client).await;
	match (conns, sas) {
		(Ok(conns), Ok(sas)) => {
			for conn in conns {
				let status = sas.get(&conn.name).copied().unwrap_or_default();
				println!(
					"{:<16} {status:?}  children: {}",
					conn.name,
					conn.children.join(", ")
				);
			}
		}
		(Err(error), _) | (_, Err(error)) => {
			eprintln!("{error}");
			std::process::exit(1);
		}
	}
}
