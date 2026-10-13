//! Connection action: one key per strongSwan connection, pressed to bring it up
//! or down.
//!
//! The plugin draws the key (see [`crate::render`]): a globe in the colour set
//! in the property inspector, over the connection's name or a label of the
//! user's. A press on a connection that is down connects it, after disconnecting
//! the connections it is exclusive with (see [`crate::exclusive`]); a press on
//! one that is up, or still negotiating, disconnects it. A failure shows
//! OpenDeck's alert on the key and its reason in the plugin log.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openaction::*;
use serde::{Deserialize, Serialize};

use crate::exclusive;
use crate::render;
use crate::vici::{self, Conn, Status};
use crate::vpn::{self, Shown, Snapshot, Vpn};

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ConnectionSettings {
	/// The connection's name, as `swanctl --list-conns` prints it.
	pub connection: Option<String>,
	/// Text under the globe. `None` = the connection's name; an empty string
	/// leaves the key without a label.
	pub label: Option<String>,
	/// The globe's colour, `#rrggbb`. `None` = [`render::DEFAULT_COLOR`].
	pub color: Option<String>,
}

impl ConnectionSettings {
	pub fn name(&self) -> Option<&str> {
		self.connection.as_deref().filter(|s| !s.is_empty())
	}

	fn label(&self) -> &str {
		self.label.as_deref().or(self.name()).unwrap_or_default()
	}

	fn image(&self, snapshot: &Snapshot) -> String {
		render::key(
			snapshot.shown(self.name()),
			render::color(self.color.as_deref()),
			self.label(),
		)
	}
}

/// A visible instance's settings, and the image last pushed to it.
struct Key {
	settings: ConnectionSettings,
	drawn: Option<String>,
}

/// Every visible instance. The openaction runtime keeps an instance's settings
/// to itself, and redraws on a charon event need them.
#[derive(Clone, Default)]
pub struct Keys(Arc<Mutex<HashMap<String, Key>>>);

impl Keys {
	fn set(&self, instance_id: &str, settings: &ConnectionSettings) {
		let mut keys = self.0.lock().unwrap();
		let key = keys.entry(instance_id.to_owned()).or_insert(Key {
			settings: settings.clone(),
			drawn: None,
		});
		key.settings = settings.clone();
	}

	fn forget(&self, instance_id: &str) {
		self.0.lock().unwrap().remove(instance_id);
	}

	/// The image `instance_id` should show now, unless it already does; the
	/// image is recorded as drawn.
	fn redraw(&self, instance_id: &str, snapshot: &Snapshot) -> Option<String> {
		let mut keys = self.0.lock().unwrap();
		let key = keys.get_mut(instance_id)?;
		let image = key.settings.image(snapshot);
		if key.drawn.as_ref() == Some(&image) {
			return None;
		}
		key.drawn = Some(image.clone());
		Some(image)
	}

	/// Forget what was drawn on `instance_id`, so the next redraw pushes again.
	fn undrawn(&self, instance_id: &str) {
		if let Some(key) = self.0.lock().unwrap().get_mut(instance_id) {
			key.drawn = None;
		}
	}
}

pub struct ConnectionAction {
	pub vpn: Vpn,
	pub keys: Keys,
}

/// What the property inspector is told: the connections to pick from and the
/// exclusive pairs, from which it ticks the rivals of the one picked.
#[derive(Serialize)]
struct InspectorState {
	event: &'static str,
	reachable: bool,
	connections: Vec<String>,
	exclusive: Vec<[String; 2]>,
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum InspectorMessage {
	/// Make `rivals` exactly the connections `connection` is exclusive with.
	SetRivals {
		connection: String,
		rivals: Vec<String>,
	},
}

#[async_trait]
impl Action for ConnectionAction {
	const UUID: &'static str = "fr.jourdois.strongswan.connection";
	type Settings = ConnectionSettings;

	async fn will_appear(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		self.keys.set(&instance.instance_id, settings);
		show(instance, &self.keys, &self.vpn.snapshot()).await
	}

	async fn will_disappear(
		&self,
		instance: &Instance,
		_settings: &Self::Settings,
	) -> OpenActionResult<()> {
		self.keys.forget(&instance.instance_id);
		Ok(())
	}

	async fn did_receive_settings(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		self.keys.set(&instance.instance_id, settings);
		show(instance, &self.keys, &self.vpn.snapshot()).await
	}

	async fn key_down(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		self.toggle(instance, settings).await
	}

	async fn dial_down(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
	) -> OpenActionResult<()> {
		self.toggle(instance, settings).await
	}

	async fn touch_tap(
		&self,
		instance: &Instance,
		settings: &Self::Settings,
		_position: (u16, u16),
		_hold: bool,
	) -> OpenActionResult<()> {
		self.toggle(instance, settings).await
	}

	async fn property_inspector_did_appear(
		&self,
		instance: &Instance,
		_settings: &Self::Settings,
	) -> OpenActionResult<()> {
		self.inform(instance).await
	}

	async fn send_to_plugin(
		&self,
		instance: &Instance,
		_settings: &Self::Settings,
		payload: &serde_json::Value,
	) -> OpenActionResult<()> {
		match serde_json::from_value(payload.clone()) {
			Ok(InspectorMessage::SetRivals { connection, rivals }) => {
				let mut global = exclusive::get();
				global.set_rivals(&connection, &rivals);
				log::info!("exclusive pairs now {:?}", global.exclusive);
				exclusive::store(global.clone());
				set_global_settings(&global).await?;
				self.inform(instance).await
			}
			Err(error) => {
				log::warn!("unexpected message from the property inspector: {error}");
				Ok(())
			}
		}
	}
}

impl ConnectionAction {
	async fn inform(&self, instance: &Instance) -> OpenActionResult<()> {
		let snapshot = self.vpn.snapshot();
		instance
			.send_to_property_inspector(InspectorState {
				event: "state",
				reachable: snapshot.reachable,
				connections: snapshot.conns.into_iter().map(|c| c.name).collect(),
				exclusive: exclusive::get().exclusive,
			})
			.await
	}

	/// Start whatever a press means for the connection's current state. The
	/// operation runs in its own task: an `initiate` holds on until charon is
	/// done, and events for every other key would queue behind it.
	async fn toggle(
		&self,
		instance: &Instance,
		settings: &ConnectionSettings,
	) -> OpenActionResult<()> {
		let snapshot = self.vpn.snapshot();
		let Some(name) = settings.name() else {
			return instance.show_alert().await;
		};
		let shown = snapshot.shown(Some(name));
		log::info!("press on {name} ({shown:?})");
		let vpn = self.vpn.clone();
		let id = instance.instance_id.clone();
		match shown {
			Shown::Unavailable => return instance.show_alert().await,
			Shown::Down => {
				let conn = snapshot
					.conn(name)
					.cloned()
					.expect("shown checks it is loaded");
				tokio::spawn(connect(vpn, conn, id));
			}
			Shown::Up | Shown::Busy => {
				tokio::spawn(disconnect(vpn, name.to_owned(), id));
			}
		}
		Ok(())
	}
}

/// Push the image matching the connection, unless the key already shows it:
/// every `setImage` makes OpenDeck write the profile out again. Nothing is
/// pushed before the monitor has first heard from charon.
async fn show(instance: &Instance, keys: &Keys, snapshot: &Snapshot) -> OpenActionResult<()> {
	if !snapshot.settled {
		return Ok(());
	}
	let Some(image) = keys.redraw(&instance.instance_id, snapshot) else {
		return Ok(());
	};
	let result = instance.set_image(Some(image), None).await;
	if result.is_err() {
		keys.undrawn(&instance.instance_id);
	}
	result
}

/// Redraw every visible key from the latest snapshot.
pub async fn refresh_all(vpn: &Vpn, keys: &Keys) {
	let snapshot = vpn.snapshot();
	for instance in visible_instances(ConnectionAction::UUID).await {
		if let Err(error) = show(&instance, keys, &snapshot).await {
			log::warn!("could not update {}: {error}", instance.instance_id);
		}
	}
}

async fn alert(instance_id: &str) {
	if let Some(instance) = get_instance(instance_id.to_owned()).await {
		let _ = instance.show_alert().await;
	}
}

/// Whether a rival has to come down before `conn` goes up: it has SAs, or an
/// operation of ours in flight (another key's connect still negotiating).
fn active(snapshot: &Snapshot, name: &str) -> bool {
	snapshot.status(name) != Status::Down || snapshot.pending.contains_key(name)
}

async fn connect(vpn: Vpn, conn: Conn, instance_id: String) {
	let name = conn.name.as_str();
	let pending = vpn.begin(name);

	for rival in exclusive::get().rivals(name) {
		if !active(&vpn.snapshot(), &rival) {
			continue;
		}
		log::info!("{name}: disconnecting {rival} first, they are exclusive");
		let rival_pending = vpn.begin(&rival);
		if let Err(error) = vici::terminate(&rival).await {
			log::warn!("{rival}: {error}");
		}
		vpn.finish(&rival, rival_pending, vpn::sas_now().await);
	}
	if !vpn.current(name, pending) {
		log::info!("{name}: connect cancelled");
		return;
	}

	log::info!("{name}: connecting");
	let result = vici::initiate(&conn).await;
	let cancelled = !vpn.current(name, pending);
	vpn.finish(name, pending, vpn::sas_now().await);
	match result {
		Ok(()) => log::info!("{name}: up"),
		Err(_) if cancelled => log::info!("{name}: connect cancelled"),
		Err(error) => {
			log::error!("{name}: connect failed: {error}");
			alert(&instance_id).await;
		}
	}
}

async fn disconnect(vpn: Vpn, name: String, instance_id: String) {
	let pending = vpn.begin(&name);
	log::info!("{name}: disconnecting");
	let result = vici::terminate(&name).await;
	let sas = vpn::sas_now().await;
	let down = sas
		.as_ref()
		.is_some_and(|s| s.get(&name).copied().unwrap_or_default() == Status::Down);
	vpn.finish(&name, pending, sas);
	match result {
		Ok(()) => log::info!("{name}: down"),
		Err(error) if down => log::info!("{name}: down ({error})"),
		Err(error) => {
			log::error!("{name}: disconnect failed: {error}");
			alert(&instance_id).await;
		}
	}
}

#[cfg(test)]
mod tests {
	//! The payloads `propertyInspector/connection.html` sends.

	use super::*;

	#[test]
	fn settings_payload() {
		let s: ConnectionSettings = serde_json::from_str(r#"{"connection":"work1"}"#).unwrap();
		assert_eq!(s.name(), Some("work1"));
		let s: ConnectionSettings = serde_json::from_str(r#"{"connection":""}"#).unwrap();
		assert_eq!(s.name(), None);
		let s: ConnectionSettings = serde_json::from_str("{}").unwrap();
		assert_eq!(s.name(), None);
		assert_eq!(s.label(), "");
	}

	#[test]
	fn label_payload() {
		let s: ConnectionSettings =
			serde_json::from_str(r##"{"connection":"work1","label":null,"color":"#3b82f6"}"##)
				.unwrap();
		assert_eq!(s.label(), "work1");
		assert_eq!(render::color(s.color.as_deref()), "#3b82f6");
		let s: ConnectionSettings =
			serde_json::from_str(r##"{"connection":"work1","label":"Travail","color":"#3b82f6"}"##)
				.unwrap();
		assert_eq!(s.label(), "Travail");
		let s: ConnectionSettings =
			serde_json::from_str(r##"{"connection":"work1","label":"","color":null}"##).unwrap();
		assert_eq!(s.label(), "");
		assert_eq!(render::color(s.color.as_deref()), render::DEFAULT_COLOR);
	}

	#[test]
	fn set_rivals_payload() {
		let m: InspectorMessage = serde_json::from_str(
			r#"{"event":"setRivals","connection":"work1","rivals":["work2"]}"#,
		)
		.unwrap();
		let InspectorMessage::SetRivals { connection, rivals } = m;
		assert_eq!(connection, "work1");
		assert_eq!(rivals, ["work2"]);
	}
}
