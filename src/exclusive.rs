//! Connections that can't be up together.
//!
//! Exclusivity is a relation between two connections, not a property of a key:
//! two full-tunnel VPNs conflict whichever key brings one up, and a connection
//! may have no key at all. So it lives in the plugin's global settings, as a
//! list of unordered pairs, and each key's property inspector edits the pairs its
//! connection is part of. Connecting a connection first disconnects its rivals.

use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct GlobalSettings {
	/// Pairs of connection names, each sorted, the list sorted and deduplicated.
	pub exclusive: Vec<[String; 2]>,
}

impl GlobalSettings {
	/// The connections `name` is exclusive with, sorted.
	pub fn rivals(&self, name: &str) -> Vec<String> {
		let mut rivals: Vec<String> = self
			.exclusive
			.iter()
			.filter_map(|[a, b]| {
				if a == name {
					Some(b.clone())
				} else if b == name {
					Some(a.clone())
				} else {
					None
				}
			})
			.collect();
		rivals.sort();
		rivals.dedup();
		rivals
	}

	/// Make `rivals` exactly the connections `name` is exclusive with, leaving
	/// the pairs between other connections alone.
	pub fn set_rivals(&mut self, name: &str, rivals: &[String]) {
		self.exclusive.retain(|[a, b]| a != name && b != name);
		for rival in rivals {
			if rival != name && !rival.is_empty() {
				self.exclusive.push(pair(name, rival));
			}
		}
		self.normalize();
	}

	/// Sort each pair and the list, and drop duplicates and self-pairs, so a
	/// hand-edited settings file reads the same as one this plugin wrote.
	pub fn normalize(&mut self) {
		for p in &mut self.exclusive {
			p.sort();
		}
		self.exclusive.retain(|[a, b]| a != b && !a.is_empty());
		self.exclusive.sort();
		self.exclusive.dedup();
	}
}

fn pair(a: &str, b: &str) -> [String; 2] {
	let mut p = [a.to_owned(), b.to_owned()];
	p.sort();
	p
}

/// The global settings as last received from OpenDeck or set by this plugin.
static GLOBAL: LazyLock<Mutex<GlobalSettings>> =
	LazyLock::new(|| Mutex::new(GlobalSettings::default()));

pub fn get() -> GlobalSettings {
	GLOBAL.lock().unwrap().clone()
}

pub fn store(mut settings: GlobalSettings) {
	settings.normalize();
	*GLOBAL.lock().unwrap() = settings;
}

#[cfg(test)]
mod tests {
	use super::*;

	fn names(v: &[&str]) -> Vec<String> {
		v.iter().map(|s| s.to_string()).collect()
	}

	#[test]
	fn the_relation_is_symmetric() {
		let mut g = GlobalSettings::default();
		g.set_rivals("work2", &names(&["work1"]));
		assert_eq!(g.rivals("work1"), ["work2"]);
		assert_eq!(g.rivals("work2"), ["work1"]);
		assert!(g.rivals("roadwarrior").is_empty());
	}

	#[test]
	fn setting_rivals_replaces_only_that_connections_pairs() {
		let mut g = GlobalSettings::default();
		g.set_rivals("work1", &names(&["work2", "work3"]));
		g.set_rivals("work2", &names(&["work3"]));
		assert_eq!(g.rivals("work1"), ["work3"]);
		assert_eq!(g.rivals("work2"), ["work3"]);
		assert_eq!(g.rivals("work3"), ["work1", "work2"]);
	}

	#[test]
	fn normalize_cleans_a_hand_written_file() {
		let mut g: GlobalSettings = serde_json::from_str(
			r#"{"exclusive":[["work2","work1"],["work1","work2"],["x","x"]],"other":1}"#,
		)
		.unwrap();
		g.normalize();
		assert_eq!(g.exclusive, [pair("work1", "work2")]);
	}

	#[test]
	fn missing_or_odd_settings_read_as_empty() {
		let g: GlobalSettings = serde_json::from_str("{}").unwrap();
		assert!(g.exclusive.is_empty());
	}
}
