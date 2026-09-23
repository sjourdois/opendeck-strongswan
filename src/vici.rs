//! strongSwan's control socket, spoken through `rsvici`.
//!
//! Every call opens its own client: an `initiate` blocks its connection until the
//! CHILD_SA is up or has failed, which can take the whole IKE retransmission
//! schedule, and VICI has no request ids to multiplex several calls on one socket.
//! A Unix socket connect costs next to nothing.

use std::collections::HashMap;

use futures_util::{TryStreamExt, pin_mut};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

/// charon's VICI socket, recreated `root:root` on every start: the user needs a
/// service drop-in that hands it to one of their groups (see the README).
pub const SOCKET: &str = "/run/charon.vici";

/// How long a clean `terminate` may wait for the peer's DELETE response before
/// the SA is torn down without it.
const TERMINATE_TIMEOUT_MS: i64 = 5_000;

pub async fn connect() -> Result<rsvici::Client, rsvici::Error> {
	Ok(rsvici::unix::connect(SOCKET).await?)
}

/// Where a connection stands, from the SAs charon holds for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
	/// No SA at all.
	#[default]
	Down,
	/// An SA is being deleted.
	Disconnecting,
	/// An IKE_SA is being negotiated, or is up with no CHILD_SA installed yet.
	Connecting,
	/// An IKE_SA is established and carries at least one installed CHILD_SA.
	Up,
}

/// A loaded connection: its name and its CHILD_SA configurations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Conn {
	pub name: String,
	pub children: Vec<String>,
}

#[derive(Deserialize)]
struct ConnEntry {
	#[serde(default)]
	children: HashMap<String, IgnoredAny>,
}

#[derive(Deserialize)]
struct IkeSa {
	state: String,
	#[serde(rename = "child-sas", default)]
	child_sas: HashMap<String, ChildSa>,
}

#[derive(Deserialize)]
struct ChildSa {
	state: String,
}

impl IkeSa {
	/// See `ike_sa_state_t` and `child_sa_state_t` in libcharon for the names.
	fn status(&self) -> Status {
		match self.state.as_str() {
			"ESTABLISHED" | "REKEYING" | "REKEYED" => {
				let installed = self.child_sas.values().any(|c| {
					matches!(
						c.state.as_str(),
						"INSTALLED" | "UPDATING" | "REKEYING" | "REKEYED"
					)
				});
				if installed {
					Status::Up
				} else {
					Status::Connecting
				}
			}
			"CREATED" | "CONNECTING" => Status::Connecting,
			"DELETING" | "DESTROYING" => Status::Disconnecting,
			// PASSIVE: an SA kept around for a peer's reauthentication, not in use.
			_ => Status::Down,
		}
	}
}

/// The connections charon has loaded, sorted by name, with sorted children.
pub async fn list_conns(client: &mut rsvici::Client) -> Result<Vec<Conn>, rsvici::Error> {
	let stream =
		client.stream_request::<(), HashMap<String, ConnEntry>>("list-conns", "list-conn", ());
	pin_mut!(stream);
	let mut conns = Vec::new();
	while let Some(batch) = stream.try_next().await? {
		for (name, entry) in batch {
			let mut children: Vec<String> = entry.children.into_keys().collect();
			children.sort();
			conns.push(Conn { name, children });
		}
	}
	conns.sort_by(|a, b| a.name.cmp(&b.name));
	Ok(conns)
}

/// Each connection with at least one SA, and where it stands. When a connection
/// has several SAs (a reauthentication makes the new one before it breaks the
/// old), the most advanced one wins.
pub async fn list_sas(
	client: &mut rsvici::Client,
) -> Result<HashMap<String, Status>, rsvici::Error> {
	let stream = client.stream_request::<(), HashMap<String, IkeSa>>("list-sas", "list-sa", ());
	pin_mut!(stream);
	let mut sas = HashMap::new();
	while let Some(batch) = stream.try_next().await? {
		for (name, sa) in batch {
			let status = sa.status();
			sas.entry(name)
				.and_modify(|s: &mut Status| *s = (*s).max(status))
				.or_insert(status);
		}
	}
	Ok(sas)
}

#[derive(Serialize)]
struct Initiate<'a> {
	ike: &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	child: Option<&'a str>,
	loglevel: i32,
}

#[derive(Serialize)]
struct Terminate<'a> {
	ike: &'a str,
	force: bool,
	timeout: i64,
	loglevel: i32,
}

#[derive(Deserialize)]
struct ControlLog {
	msg: String,
}

/// Run a command that streams `control-log` events, copying them to the log.
async fn logged<T: Serialize>(cmd: &str, conn: &str, request: T) -> Result<(), rsvici::Error> {
	let mut client = connect().await?;
	let stream = client.stream_request::<T, ControlLog>(cmd, "control-log", request);
	pin_mut!(stream);
	while let Some(line) = stream.try_next().await? {
		log::info!("{conn}: {}", line.msg);
	}
	Ok(())
}

/// Bring a connection up: every CHILD_SA it defines, the first one creating the
/// IKE_SA the others then join. Returns once they are all installed, or on the
/// first failure. charon waits out its own retransmission schedule, so an
/// unreachable peer takes minutes to fail; a `terminate` meanwhile cuts it short.
pub async fn initiate(conn: &Conn) -> Result<(), rsvici::Error> {
	let children: Vec<Option<&str>> = if conn.children.is_empty() {
		vec![None]
	} else {
		conn.children.iter().map(|c| Some(c.as_str())).collect()
	};
	for child in children {
		let request = Initiate {
			ike: &conn.name,
			child,
			loglevel: 1,
		};
		logged("initiate", &conn.name, request).await?;
	}
	Ok(())
}

/// Tear a connection's IKE_SAs down, with their CHILD_SAs. A peer that doesn't
/// answer the DELETE in time gets the SA forced down without it.
pub async fn terminate(name: &str) -> Result<(), rsvici::Error> {
	let clean = Terminate {
		ike: name,
		force: false,
		timeout: TERMINATE_TIMEOUT_MS,
		loglevel: 1,
	};
	if let Err(error) = logged("terminate", name, clean).await {
		log::warn!("{name}: terminate failed ({error}), forcing it");
		let forced = Terminate {
			ike: name,
			force: true,
			timeout: 0,
			loglevel: 1,
		};
		logged("terminate", name, forced).await?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	//! The shapes below mirror what charon 6.1 streams (its vici README), trimmed
	//! but keeping what trips a deserializer: nested sections, lists, and keys the
	//! plugin doesn't read.

	use super::*;

	#[derive(Serialize)]
	struct Child {
		name: &'static str,
		state: &'static str,
		mode: &'static str,
		#[serde(rename = "local-ts")]
		local_ts: Vec<&'static str>,
		#[serde(rename = "remote-ts")]
		remote_ts: Vec<&'static str>,
	}

	#[derive(Serialize)]
	struct Sa {
		uniqueid: &'static str,
		version: &'static str,
		state: &'static str,
		#[serde(rename = "local-vips")]
		local_vips: Vec<&'static str>,
		#[serde(rename = "tasks-active")]
		tasks_active: Vec<&'static str>,
		#[serde(rename = "child-sas")]
		child_sas: HashMap<&'static str, Child>,
	}

	fn sa(state: &'static str, children: &[(&'static str, &'static str)]) -> Sa {
		Sa {
			uniqueid: "7",
			version: "2",
			state,
			local_vips: vec!["10.8.0.12"],
			tasks_active: vec![],
			child_sas: children
				.iter()
				.map(|&(key, state)| {
					(
						key,
						Child {
							name: "work1",
							state,
							mode: "TUNNEL",
							local_ts: vec!["10.8.0.12/32"],
							remote_ts: vec!["0.0.0.0/0", "::/0"],
						},
					)
				})
				.collect(),
		}
	}

	fn status(sa: Sa) -> Status {
		let bytes = serde_vici::to_vec(&HashMap::from([("work1", sa)])).unwrap();
		let parsed: HashMap<String, IkeSa> = serde_vici::from_slice(&bytes).unwrap();
		parsed["work1"].status()
	}

	#[test]
	fn established_with_installed_child_is_up() {
		assert_eq!(
			status(sa("ESTABLISHED", &[("work1-3", "INSTALLED")])),
			Status::Up
		);
	}

	#[test]
	fn established_without_child_is_still_connecting() {
		assert_eq!(status(sa("ESTABLISHED", &[])), Status::Connecting);
		assert_eq!(
			status(sa("ESTABLISHED", &[("work1-3", "INSTALLING")])),
			Status::Connecting
		);
	}

	#[test]
	fn negotiation_and_teardown() {
		assert_eq!(status(sa("CONNECTING", &[])), Status::Connecting);
		assert_eq!(
			status(sa("DELETING", &[("work1-3", "INSTALLED")])),
			Status::Disconnecting
		);
		assert_eq!(status(sa("PASSIVE", &[])), Status::Down);
	}

	#[test]
	fn conn_children_are_read_past_unknown_sections() {
		#[derive(Serialize)]
		struct Auth {
			class: &'static str,
			certs: Vec<&'static str>,
		}
		#[derive(Serialize)]
		struct ChildConf {
			mode: &'static str,
			#[serde(rename = "remote-ts")]
			remote_ts: Vec<&'static str>,
		}
		#[derive(Serialize)]
		struct ConnConf {
			local_addrs: Vec<&'static str>,
			remote_addrs: Vec<&'static str>,
			version: &'static str,
			#[serde(rename = "local-1")]
			local: Auth,
			children: HashMap<&'static str, ChildConf>,
		}
		let conf = ConnConf {
			local_addrs: vec!["%any"],
			remote_addrs: vec!["192.0.2.1"],
			version: "IKEv2",
			local: Auth {
				class: "EAP_TLS",
				certs: vec!["CN=me"],
			},
			children: HashMap::from([(
				"work2",
				ChildConf {
					mode: "TUNNEL",
					remote_ts: vec!["0.0.0.0/0"],
				},
			)]),
		};
		let bytes = serde_vici::to_vec(&HashMap::from([("work2", conf)])).unwrap();
		let parsed: HashMap<String, ConnEntry> = serde_vici::from_slice(&bytes).unwrap();
		let children: Vec<&String> = parsed["work2"].children.keys().collect();
		assert_eq!(children, ["work2"]);
	}
}
