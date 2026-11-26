//! Dedicated/listen server configuration and fail-fast data validation.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use acvd_proto::{PosQuantizer, NETCODE_PROTOCOL_ID};
use anyhow::{bail, Result};

/// Server startup configuration. `allow_missing_disc` exists only for tests; the dedicated
/// binary always requires the owned disc.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub max_clients: usize,
    pub protocol_id: u64,
    pub map: String,
    pub disc: Option<PathBuf>,
    /// Test-only escape hatch. The binary never sets this.
    pub allow_missing_disc: bool,
    /// Snapshot cadence in 60 Hz ticks (2 = 30 Hz).
    pub snapshot_every: u32,
    pub quantizer: PosQuantizer,
    /// Ticks a dead AC waits before the deterministic respawn drop (180 = 3 s).
    pub respawn_ticks: u32,
    /// Deterministic combat/spread/respawn seed.
    pub match_seed: u64,
    /// Team damage (off by default, like retail team deathmatch).
    pub friendly_fire: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], acvd_proto::DEFAULT_PORT)),
            max_clients: 24,
            protocol_id: NETCODE_PROTOCOL_ID,
            map: "m4000".into(),
            disc: None,
            allow_missing_disc: false,
            snapshot_every: 2,
            quantizer: PosQuantizer::default(),
            respawn_ticks: 180,
            match_seed: acvd_sim::world::DEFAULT_MATCH_SEED,
            friendly_fire: false,
        }
    }
}

impl ServerConfig {
    /// Fails fast when the owned disc is missing. The generated `acvd-data` tables are compiled
    /// into `acvd-sim`/`acvd-server`; a build without them fails before this check can run.
    pub fn validate_data(&self) -> Result<()> {
        if self.allow_missing_disc {
            return Ok(());
        }
        if self.map.trim().is_empty() {
            bail!("no map: pass --map <id>");
        }
        let path = self
            .disc
            .clone()
            .or_else(default_disc_path)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no game disc: pass --disc <path to ACV2.pe> or set ACVD_DISC; \
                     the dedicated server needs the owned disc"
                )
            })?;
        if !path.is_file() {
            bail!(
                "game disc not found at {}; pass --disc <path to ACV2.pe>",
                path.display()
            );
        }
        Ok(())
    }

    /// Stable id used in join data for the map name.
    pub fn map_id(&self) -> u32 {
        self.map.bytes().fold(2_166_136_261u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
        })
    }
}

fn default_disc_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ACVD_DISC") {
        return Some(PathBuf::from(path));
    }
    let candidate = Path::new("private/x360/vd/ACV2.pe");
    candidate.is_file().then(|| candidate.to_path_buf())
}
