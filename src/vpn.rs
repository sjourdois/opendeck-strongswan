//! What the keys show, and the task that keeps it current.
//!
//! [`Vpn`] holds one [`Snapshot`] behind a watch channel: whether charon is
//! reachable, the connections it has loaded, where each one's SAs stand, and the
//! operations this plugin has in flight. [`monitor`] refreshes it from charon's
//! `ike-updown` / `child-updown` events, plus a poll: VICI raises no event while
//! an SA is merely negotiating or being deleted, so a transition started from
//! elsewhere (`swanctl`, the waybar menu, DPD) only shows through `list-sas`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{StreamExt, stream};
use serde::de::IgnoredAny;
use tokio::sync::{Notify, watch};

use crate::vici::{self, Conn, Status};

/// Poll interval while some SA is between states.
const FAST_POLL: Duration = Duration::from_secs(1);
/// Poll interval otherwise: only catches what raises no event, such as a
/// `swanctl --load-all` that changed the connection list.
const SLOW_POLL: Duration = Duration::from_secs(30);
/// Delay before reconnecting to a charon that went away.
const RETRY: Duration = Duration::from_secs(2);

/// The events after which the SA list is worth reading again.
const EVENTS: [&str; 4] = ["ike-updown", "child-updown", "ike-rekey", "child-rekey"];

/// A connect or disconnect this plugin started and hasn't seen finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
	/// Tells a superseded operation from the current one: an initiate cancelled
	/// by a terminate must not clear the terminate's mark when it fails.
	pub token: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
	/// Whether the monitor has made its first attempt: until then nothing is
	/// known, and keys keep the state their profile saved.
	pub settled: bool,
	pub reachable: bool,
	pub conns: Vec<Conn>,
	pub sas: HashMap<String, Status>,
	pub pending: HashMap<String, Pending>,
}

/// What a key for one connection shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
	/// charon unreachable, the connection isn't loaded, or none is configured.
	Unavailable,
	Down,
	/// Connecting or disconnecting.
	Busy,
	Up,
}

impl Snapshot {
	pub fn conn(&self, name: &str) -> Option<&Conn> {
		self.conns.iter().find(|c| c.name == name)
	}

	pub fn status(&self, name: &str) -> Status {
		self.sas.get(name).copied().unwrap_or_default()
	}

	pub fn shown(&self, name: Option<&str>) -> Shown {
		let Some(name) = name else {
			return Shown::Unavailable;
		};
		if !self.reachable || self.conn(name).is_none() {
			return Shown::Unavailable;
		}
		if self.pending.contains_key(name) {
			return Shown::Busy;
		}
		match self.status(name) {
			Status::Down => Shown::Down,
			Status::Connecting | Status::Disconnecting => Shown::Busy,
			Status::Up => Shown::Up,
		}
	}

	fn transitional(&self) -> bool {
		!self.pending.is_empty()
			|| self
				.sas
				.values()
				.any(|s| matches!(s, Status::Connecting | Status::Disconnecting))
	}
}

#[derive(Clone)]
pub struct Vpn {
	state: Arc<watch::Sender<Snapshot>>,
	kick: Arc<Notify>,
	tokens: Arc<AtomicU64>,
}

impl Default for Vpn {
	fn default() -> Self {
		Self {
			state: Arc::new(watch::Sender::new(Snapshot::default())),
			kick: Arc::new(Notify::new()),
			tokens: Arc::new(AtomicU64::new(0)),
		}
	}
}

impl Vpn {
	pub fn snapshot(&self) -> Snapshot {
		self.state.borrow().clone()
	}

	pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
		self.state.subscribe()
	}

	/// Have [`monitor`] read charon again now rather than at its next poll.
	pub fn kick(&self) {
		self.kick.notify_one();
	}

	/// Mark `name` as having an operation in flight, superseding any earlier one.
	pub fn begin(&self, name: &str) -> Pending {
		let pending = Pending {
			token: self.tokens.fetch_add(1, Ordering::Relaxed),
		};
		self.state.send_modify(|s| {
			s.pending.insert(name.to_owned(), pending);
		});
		pending
	}

	/// Whether `pending` is still the operation in flight on `name`.
	pub fn current(&self, name: &str, pending: Pending) -> bool {
		self.state.borrow().pending.get(name) == Some(&pending)
	}

	/// End `pending` on `name`, taking `sas` as the new SA list in the same step
	/// so the key never flashes the state from before the operation. Does
	/// nothing to the mark if a later operation superseded this one.
	pub fn finish(&self, name: &str, pending: Pending, sas: Option<HashMap<String, Status>>) {
		self.state.send_if_modified(|s| {
			let mut changed = false;
			if s.pending.get(name) == Some(&pending) {
				s.pending.remove(name);
				changed = true;
			}
			if let Some(sas) = sas
				&& s.sas != sas
			{
				s.sas = sas;
				changed = true;
			}
			changed
		});
		self.kick();
	}

	fn update(&self, conns: Vec<Conn>, sas: HashMap<String, Status>) {
		self.state.send_if_modified(|s| {
			let changed = !s.settled || !s.reachable || s.conns != conns || s.sas != sas;
			s.settled = true;
			s.reachable = true;
			s.conns = conns;
			s.sas = sas;
			changed
		});
	}

	fn unreachable(&self) {
		self.state.send_if_modified(|s| {
			let changed = !s.settled || s.reachable || !s.sas.is_empty();
			s.settled = true;
			s.reachable = false;
			s.sas.clear();
			changed
		});
	}
}

/// Read the SA list on a fresh client, for an operation that just finished.
pub async fn sas_now() -> Option<HashMap<String, Status>> {
	let mut client = vici::connect().await.ok()?;
	vici::list_sas(&mut client).await.ok()
}

/// Keep [`Vpn`] in line with charon for the life of the plugin, reconnecting
/// whenever charon goes away (a restart recreates its socket).
pub async fn monitor(vpn: Vpn) {
	let mut lost = false;
	loop {
		match session(&vpn, &mut lost).await {
			Ok(()) => log::warn!("charon closed the VICI connection"),
			Err(error) if !lost => log::warn!("charon unreachable on {}: {error}", vici::SOCKET),
			Err(_) => {}
		}
		lost = true;
		vpn.unreachable();
		tokio::time::sleep(RETRY).await;
	}
}

async fn session(vpn: &Vpn, lost: &mut bool) -> Result<(), rsvici::Error> {
	let mut client = vici::connect().await?;
	let mut events =
		stream::select_all(EVENTS.map(|event| client.subscribe::<IgnoredAny>(event).boxed()));
	let mut first = true;
	loop {
		let conns = vici::list_conns(&mut client).await?;
		let sas = vici::list_sas(&mut client).await?;
		if first {
			first = false;
			if std::mem::take(lost) {
				log::info!("charon is back");
			}
			log::info!(
				"{} connection(s) loaded, {} with SAs",
				conns.len(),
				sas.len()
			);
		}
		vpn.update(conns, sas);

		let poll = if vpn.state.borrow().transitional() {
			FAST_POLL
		} else {
			SLOW_POLL
		};
		tokio::select! {
			event = events.next() => match event {
				Some(Ok(IgnoredAny)) => {}
				Some(Err(error)) => return Err(error),
				None => return Ok(()),
			},
			() = tokio::time::sleep(poll) => {}
			() = vpn.kick.notified() => {}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn snapshot() -> Snapshot {
		Snapshot {
			settled: true,
			reachable: true,
			conns: vec![
				Conn {
					name: "work1".into(),
					children: vec!["work1".into()],
				},
				Conn {
					name: "work2".into(),
					children: vec!["work2".into()],
				},
			],
			..Snapshot::default()
		}
	}

	#[test]
	fn shown_follows_sas() {
		let mut s = snapshot();
		assert_eq!(s.shown(Some("work1")), Shown::Down);
		s.sas.insert("work1".into(), Status::Up);
		assert_eq!(s.shown(Some("work1")), Shown::Up);
		s.sas.insert("work1".into(), Status::Disconnecting);
		assert_eq!(s.shown(Some("work1")), Shown::Busy);
	}

	#[test]
	fn pending_shows_busy() {
		let mut s = snapshot();
		s.sas.insert("work1".into(), Status::Up);
		s.pending.insert("work1".into(), Pending { token: 0 });
		assert_eq!(s.shown(Some("work1")), Shown::Busy);
		assert!(s.transitional());
	}

	#[test]
	fn unavailable_cases() {
		let mut s = snapshot();
		assert_eq!(s.shown(None), Shown::Unavailable);
		assert_eq!(s.shown(Some("gone")), Shown::Unavailable);
		s.reachable = false;
		assert_eq!(s.shown(Some("work1")), Shown::Unavailable);
	}

	#[test]
	fn a_superseded_operation_leaves_the_mark_alone() {
		let vpn = Vpn::default();
		let connect = vpn.begin("work1");
		let cancel = vpn.begin("work1");
		assert!(!vpn.current("work1", connect));
		vpn.finish("work1", connect, None);
		assert_eq!(vpn.snapshot().pending.get("work1"), Some(&cancel));
		vpn.finish("work1", cancel, None);
		assert!(vpn.snapshot().pending.is_empty());
	}
}
